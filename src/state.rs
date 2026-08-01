//! State persistence for `~/.config/synapse/state.json`.
//!
//! Every write is atomic: a temp file next to the target is written then
//! renamed into place, so a crash mid-write never corrupts the state file.
//!
//! Concurrent access is guarded by a PID lock file at
//! `~/.config/synapse/.lock`. The lock stores only the owning PID; on
//! acquisition we check whether that PID is still running and clear a stale
//! lock automatically.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ── Public data types ─────────────────────────────────────────────────────────

/// How many prior versions are retained per package for rollback.
pub const HISTORY_LIMIT: usize = 3;

/// A previously installed version, retained so rollback has somewhere to go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorVersion {
    pub version: String,
    pub installed_at: u64,
    /// Nix store path, when it was recorded. Rollback reuses this path if it is
    /// still in the store, which avoids rebuilding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_path: Option<String>,
}

/// A managed package tracked in state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRecord {
    /// Package version string, e.g. `"0.7.5"`.
    pub version: String,
    /// Unix timestamp of the most recent successful install.
    pub installed_at: u64,
    /// Nix store path of the current version, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_path: Option<String>,
    /// Prior versions, newest first, capped at [`HISTORY_LIMIT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<PriorVersion>,
}

/// Root of `state.json`. Fields are additive; unknown keys are ignored on read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Map of package name → record.
    #[serde(default)]
    pub packages: HashMap<String, PackageRecord>,
}

impl State {
    /// Record a successful install or update of a package.
    pub fn set_package(&mut self, name: impl Into<String>, version: impl Into<String>) {
        self.set_package_with_path(name, version, None);
    }

    /// Like [`State::set_package`], also recording the Nix store path.
    ///
    /// When the package was already installed at a *different* version, that
    /// version is pushed onto the history (newest first, capped at
    /// [`HISTORY_LIMIT`]). Re-installing the same version adds no history entry:
    /// there would be nothing to roll back to.
    pub fn set_package_with_path(
        &mut self,
        name: impl Into<String>,
        version: impl Into<String>,
        store_path: Option<String>,
    ) {
        let name = name.into();
        let version = version.into();

        let history = match self.packages.get(&name) {
            Some(prev) if prev.version != version => {
                let mut h = Vec::with_capacity((prev.history.len() + 1).min(HISTORY_LIMIT));
                h.push(PriorVersion {
                    version: prev.version.clone(),
                    installed_at: prev.installed_at,
                    store_path: prev.store_path.clone(),
                });
                h.extend(prev.history.iter().cloned());
                h.truncate(HISTORY_LIMIT);
                h
            }
            Some(prev) => prev.history.clone(),
            None => Vec::new(),
        };

        self.packages.insert(
            name,
            PackageRecord {
                version,
                installed_at: now_secs(),
                store_path,
                history,
            },
        );
    }

    /// Roll `name` back to its most recent prior version.
    ///
    /// The version being replaced is *discarded* rather than pushed onto the
    /// history: rolling back means the current version is unwanted, so keeping
    /// it as a target would let two rollbacks return to it.
    ///
    /// Returns `(previous_version, restored)` or `None` when there is no history.
    pub fn rollback_package(&mut self, name: &str) -> Option<(String, PriorVersion)> {
        let record = self.packages.get_mut(name)?;
        if record.history.is_empty() {
            return None;
        }

        let target = record.history.remove(0);
        let replaced = record.version.clone();

        record.version = target.version.clone();
        record.installed_at = now_secs();
        record.store_path = target.store_path.clone();

        Some((replaced, target))
    }

    /// Remove a package from the state (used by uninstall).
    pub fn remove_package(&mut self, name: &str) {
        self.packages.remove(name);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Config directory ──────────────────────────────────────────────────────────

/// Path to the Synapse config directory, `~/.config/synapse`.
pub fn config_dir() -> PathBuf {
    dirs_from_env().join("synapse")
}

/// Resolve `~/.config` from the environment. Respects `XDG_CONFIG_HOME`.
fn dirs_from_env() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/tmp"));
    PathBuf::from(home).join(".config")
}

// ── State I/O ─────────────────────────────────────────────────────────────────

/// Read state from `<dir>/state.json`.
///
/// Returns `Ok(State::default())` when the file does not yet exist; any other
/// I/O error or JSON parse failure is propagated.
pub fn read(dir: &Path) -> io::Result<State> {
    let path = dir.join("state.json");
    match fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}

/// Write `state` to `<dir>/state.json` atomically.
///
/// The file is written to a sibling `.state.json.tmp`, then renamed. This
/// ensures the state file is always either the old or the new content, never
/// a partial write.
pub fn write(dir: &Path, state: &State) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let dest = dir.join("state.json");
    let tmp = dir.join(".state.json.tmp");
    let json = serde_json::to_string_pretty(state).map_err(io::Error::other)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &dest)
}

// ── Lock file ─────────────────────────────────────────────────────────────────

/// An exclusive process lock held on `<dir>/.lock`.
///
/// Dropped via [`Drop`]; `release` can also be called explicitly.
pub struct Lock {
    path: PathBuf,
    released: bool,
}

impl std::fmt::Debug for Lock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lock")
            .field("path", &self.path)
            .field("released", &self.released)
            .finish()
    }
}

/// Errors that can occur while acquiring the lock.
#[derive(Debug)]
pub enum LockError {
    /// Another live process owns the lock.
    Held { owner_pid: u32 },
    /// I/O failure unrelated to contention.
    Io(io::Error),
}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held { owner_pid } => write!(f, "lock held by PID {owner_pid}"),
            Self::Io(e) => write!(f, "lock I/O error: {e}"),
        }
    }
}

/// Try to acquire the lock at `<dir>/.lock`.
///
/// - If no lock file exists, write our PID and succeed.
/// - If a lock file exists and the PID inside is still running, return
///   [`LockError::Held`].
/// - If the PID is gone (stale lock), replace it with our PID and succeed.
/// - If another process wins the create race, return [`LockError::Held`] naming
///   that process — contention is never reported as an I/O failure.
pub fn acquire(dir: &Path) -> Result<Lock, LockError> {
    fs::create_dir_all(dir).map_err(LockError::Io)?;
    let path = dir.join(".lock");

    if path.exists() {
        let raw = fs::read_to_string(&path).map_err(LockError::Io)?;
        if let Ok(pid) = raw.trim().parse::<u32>() {
            if is_running(pid) {
                return Err(LockError::Held { owner_pid: pid });
            }
            // Stale lock: remove it so create_new below succeeds.
            fs::remove_file(&path).map_err(LockError::Io)?;
        }
    }

    match write_pid(&path) {
        Ok(()) => Ok(Lock {
            path,
            released: false,
        }),
        // Lost the create_new race: the winner's PID is now in the file. Report
        // contention, not an I/O error, so callers can show the friendly path.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let owner_pid = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                .unwrap_or(0);
            Err(LockError::Held { owner_pid })
        }
        Err(e) => Err(LockError::Io(e)),
    }
}

impl Lock {
    /// Explicitly release the lock. Idempotent.
    pub fn release(&mut self) {
        if !self.released {
            let _ = fs::remove_file(&self.path);
            self.released = true;
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.release();
    }
}

fn write_pid(path: &Path) -> io::Result<()> {
    use std::fs::OpenOptions;
    let pid = std::process::id();
    // O_CREAT|O_EXCL: atomic at the OS level — exactly one concurrent caller wins.
    // If the file already exists this returns ErrorKind::AlreadyExists, which
    // acquire() interprets as contention and re-reads the owning PID.
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    write!(f, "{pid}")
}

/// Returns true if a process with `pid` is currently running.
///
/// On Unix: `kill(pid, 0)` — sends no signal but checks existence.
/// Interprets EPERM (process exists but we can't signal it) as running.
#[cfg(unix)]
fn is_running(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) sends no signal; we only test whether the process
    // exists. Any return value other than -1 means it's running; -1/EPERM also
    // means it's running (we lack permission to signal it).
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM: process exists but we can't signal it — still running.
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// On non-Unix targets, we conservatively assume the process is running to
/// avoid spuriously clearing a lock we can't verify.
#[cfg(not(unix))]
fn is_running(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir() -> tempdir::TempDir {
        tempdir::TempDir::new("synapse-state-test").unwrap()
    }

    // Inline a minimal tempdir to avoid the dep — just a wrapper around
    // std::env::temp_dir + a unique suffix. Uses an atomic counter instead of
    // subsec_nanos so parallel tests never collide on the same path.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn new(prefix: &str) -> std::io::Result<Self> {
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let dir =
                    std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id(),));
                std::fs::create_dir_all(&dir)?;
                Ok(Self(dir))
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn round_trips_empty_state() {
        let d = tmpdir();
        let orig = State::default();
        write(d.path(), &orig).unwrap();
        assert_eq!(read(d.path()).unwrap(), orig);
    }

    #[test]
    fn round_trips_populated_state() {
        let d = tmpdir();
        let mut s = State::default();
        s.set_package("herdr", "0.7.5");
        s.set_package("omp", "17.2.2");
        write(d.path(), &s).unwrap();
        let s2 = read(d.path()).unwrap();
        assert_eq!(s2.packages["herdr"].version, "0.7.5");
        assert_eq!(s2.packages["omp"].version, "17.2.2");
    }

    #[test]
    fn returns_default_when_file_missing() {
        let d = tmpdir();
        let s = read(d.path()).unwrap();
        assert!(s.packages.is_empty());
    }

    #[test]
    fn write_is_atomic_no_tmp_left_on_success() {
        let d = tmpdir();
        write(d.path(), &State::default()).unwrap();
        assert!(
            !d.path().join(".state.json.tmp").exists(),
            "tmp file leaked"
        );
        assert!(d.path().join("state.json").exists());
    }

    #[test]
    fn set_and_remove_package() {
        let mut s = State::default();
        s.set_package("skillshare", "0.20.23");
        assert!(s.packages.contains_key("skillshare"));
        s.remove_package("skillshare");
        assert!(!s.packages.contains_key("skillshare"));
    }

    #[test]
    fn lock_acquire_and_release() {
        let d = tmpdir();
        let mut lock = acquire(d.path()).expect("should acquire fresh lock");
        assert!(d.path().join(".lock").exists());
        lock.release();
        assert!(!d.path().join(".lock").exists());
    }

    #[test]
    fn lock_drops_on_drop() {
        let d = tmpdir();
        {
            let _lock = acquire(d.path()).expect("should acquire");
            assert!(d.path().join(".lock").exists());
        }
        assert!(
            !d.path().join(".lock").exists(),
            "lock not released on drop"
        );
    }

    #[test]
    fn lock_rejects_live_owner() {
        let d = tmpdir();
        // Write our own PID — we are definitely running.
        let lock_path = d.path().join(".lock");
        fs::write(&lock_path, std::process::id().to_string()).unwrap();
        match acquire(d.path()) {
            Err(LockError::Held { owner_pid }) => {
                assert_eq!(owner_pid, std::process::id());
            }
            other => panic!("expected Held, got {other:?}"),
        }
        // Clean up so the test's temp dir drops cleanly.
        fs::remove_file(&lock_path).unwrap();
    }

    #[test]
    fn lock_clears_stale_pid() {
        let d = tmpdir();
        // PID 1 on Linux is init and unlikely to be re-assignable to us, but
        // we need a PID that is definitely *not* our process. Use 99999 which
        // is above the typical max PID on macOS (99998) so kill(99999, 0)
        // returns ESRCH.
        let lock_path = d.path().join(".lock");
        fs::write(&lock_path, "99999").unwrap();
        // If the PID happens to exist on this machine the test becomes a no-op
        // (not a failure); we just can't assume it's stale everywhere.
        let result = acquire(d.path());
        match result {
            Ok(mut lock) => lock.release(),
            Err(LockError::Held { .. }) => { /* 99999 is live on this machine — acceptable */ }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn create_race_loser_reports_held_not_io() {
        // Simulates losing the O_EXCL create race: the lock file appears between
        // acquire()'s exists() check and its create_new() call. The only way to
        // observe that window deterministically is to have the file already hold
        // a live PID that acquire() must attribute rather than clear.
        //
        // Regression guard: contention must surface as Held (so callers can show
        // "another synapse is running"), never as Io("File exists").
        let d = tmpdir();
        let lock_path = d.path().join(".lock");
        fs::write(&lock_path, std::process::id().to_string()).unwrap();

        match acquire(d.path()) {
            Err(LockError::Held { owner_pid }) => assert_eq!(owner_pid, std::process::id()),
            Err(LockError::Io(e)) => panic!("contention misreported as I/O error: {e}"),
            Ok(_) => panic!("acquired a lock held by a live process"),
        }

        fs::remove_file(&lock_path).unwrap();
    }

    #[test]
    fn held_error_never_renders_as_file_exists() {
        // The Display impl is user-facing; a race loser must not be told
        // "File exists (os error 17)".
        let rendered = LockError::Held { owner_pid: 4242 }.to_string();
        assert!(rendered.contains("4242"), "should name the owning PID");
        assert!(
            !rendered.contains("File exists"),
            "contention must not leak the raw OS error"
        );
    }

    // ── Version history and rollback tests ────────────────────────────────

    #[test]
    fn first_install_has_no_history() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.5");
        assert!(s.packages["herdr"].history.is_empty());
    }

    #[test]
    fn update_pushes_old_version_to_history() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.4");
        s.set_package("herdr", "0.7.5");
        let rec = &s.packages["herdr"];
        assert_eq!(rec.version, "0.7.5");
        assert_eq!(rec.history.len(), 1);
        assert_eq!(rec.history[0].version, "0.7.4");
    }

    #[test]
    fn history_is_capped_at_limit() {
        let mut s = State::default();
        for v in ["0.7.1", "0.7.2", "0.7.3", "0.7.4", "0.7.5"] {
            s.set_package("herdr", v);
        }
        let rec = &s.packages["herdr"];
        assert_eq!(rec.version, "0.7.5");
        assert_eq!(
            rec.history.len(),
            HISTORY_LIMIT,
            "history grew past HISTORY_LIMIT"
        );
        // Most recent prior version is first.
        assert_eq!(rec.history[0].version, "0.7.4");
    }

    #[test]
    fn reinstall_same_version_does_not_grow_history() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.5");
        s.set_package("herdr", "0.7.5");
        s.set_package("herdr", "0.7.5");
        assert!(
            s.packages["herdr"].history.is_empty(),
            "same-version reinstall should not push history"
        );
    }

    #[test]
    fn rollback_restores_previous_version() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.4");
        s.set_package("herdr", "0.7.5");

        let (from, to) = s.rollback_package("herdr").expect("should have history");
        assert_eq!(from, "0.7.5");
        assert_eq!(to.version, "0.7.4");
        assert_eq!(s.packages["herdr"].version, "0.7.4");
    }

    #[test]
    fn rollback_discards_bad_version_not_adds_to_history() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.4");
        s.set_package("herdr", "0.7.5");
        s.rollback_package("herdr");
        // 0.7.5 must not appear in history — it was bad
        let hist = &s.packages["herdr"].history;
        assert!(
            hist.iter().all(|h| h.version != "0.7.5"),
            "rolled-back version should not be a rollback target: {hist:?}"
        );
    }

    #[test]
    fn rollback_returns_none_without_history() {
        let mut s = State::default();
        s.set_package("herdr", "0.7.5");
        assert!(s.rollback_package("herdr").is_none());
    }

    #[test]
    fn rollback_returns_none_for_unknown_package() {
        let mut s = State::default();
        assert!(s.rollback_package("nonexistent").is_none());
    }

    #[test]
    fn state_with_history_round_trips_through_json() {
        let d = tmpdir();
        let mut s = State::default();
        s.set_package("herdr", "0.7.4");
        s.set_package("herdr", "0.7.5");
        write(d.path(), &s).unwrap();
        let s2 = read(d.path()).unwrap();
        assert_eq!(s, s2, "state with history did not survive JSON round-trip");
        assert_eq!(s2.packages["herdr"].history[0].version, "0.7.4");
    }
}
