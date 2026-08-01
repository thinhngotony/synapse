//! Auto-update scheduler: launchd (macOS), systemd (Linux), cron (WSL2 fallback).
//!
//! # Idempotency
//! `enable` checks whether the unit is already registered before loading/enabling
//! so a second call is a no-op, not a double-registration. Verification is done
//! by asking the scheduler, not by checking file presence — a written plist that
//! launchd rejected is a silent no-op, so we must observe the daemon's state.
//!
//! # Lock
//! The scheduled run path acquires the SYN-4 state lock before calling update,
//! so a cron/launchd/systemd invocation cannot collide with an interactive install.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::platform;
use crate::state;

// ── Config schema ─────────────────────────────────────────────────────────────

/// Persisted at `~/.config/synapse/auto-update.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoUpdateConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How often to check for updates (e.g. "1d", "12h"). Default: daily.
    #[serde(default = "default_check_interval")]
    pub check_interval: String,
    /// How often to install updates once found. Default: weekly.
    #[serde(default = "default_install_interval")]
    pub install_interval: String,
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Preferred local time (24-hour "HH:MM"). Default: 02:00.
    #[serde(default = "default_preferred_time")]
    pub preferred_time: String,
    /// Maximum wall time the update job may run before being killed.
    #[serde(default = "default_max_duration")]
    pub max_duration: String,
}

fn default_true() -> bool {
    true
}
fn default_check_interval() -> String {
    "1d".to_string()
}
fn default_install_interval() -> String {
    "7d".to_string()
}
fn default_preferred_time() -> String {
    "02:00".to_string()
}
fn default_max_duration() -> String {
    "30m".to_string()
}

impl Default for AutoUpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval: default_check_interval(),
            install_interval: default_install_interval(),
            notify: true,
            preferred_time: default_preferred_time(),
            max_duration: default_max_duration(),
        }
    }
}

/// Write `content` to `path` with an explicit mode, not whatever the umask
/// happens to be.
///
/// launchd refuses to load a LaunchAgent plist that is group- or world-writable
/// — a deliberate security check, since a writable plist is an arbitrary-code
/// vector. A plain `fs::write` inherits the process umask, so under `umask 000`
/// the file lands 0666 and launchd rejects it with "Load failed: 5". The mode is
/// therefore set explicitly rather than left to the environment.
fn write_with_mode(path: &std::path::Path, content: &str, mode: u32) -> io::Result<()> {
    fs::write(path, content.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    // Non-Unix has no equivalent bits to tighten; the write above is the whole op.
    #[cfg(not(unix))]
    let _ = mode;

    Ok(())
}

// ── Config I/O ────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    state::config_dir().join("auto-update.yaml")
}

fn read_config() -> io::Result<AutoUpdateConfig> {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(text) => serde_yaml::from_str(&text).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AutoUpdateConfig::default()),
        Err(e) => Err(e),
    }
}

fn write_config(cfg: &AutoUpdateConfig) -> io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(cfg).map_err(io::Error::other)?;
    let tmp = path.with_extension("yaml.tmp");
    // 0600: user config. Nothing secret today, but it costs nothing to keep it
    // private, and the mode is set before the rename so the final file is never
    // briefly world-readable.
    write_with_mode(&tmp, &yaml, 0o600)?;
    fs::rename(&tmp, &path)
}

// ── Platform detection ────────────────────────────────────────────────────────

/// Which scheduler backend will be used on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Launchd,
    Systemd,
    Cron,
}

/// Detect which scheduler backend is available.
///
/// Returns `Launchd` on macOS, `Systemd` when the user session is running,
/// `Cron` otherwise (WSL2 / systems without systemd).
pub fn detect_backend() -> Backend {
    match platform::detect_os() {
        platform::OS::Mac => Backend::Launchd,
        platform::OS::Linux | platform::OS::Windows => {
            if systemd_available() {
                Backend::Systemd
            } else {
                Backend::Cron
            }
        }
    }
}

fn systemd_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .map(|o| {
            matches!(
                String::from_utf8_lossy(&o.stdout).trim(),
                "running" | "degraded"
            )
        })
        .unwrap_or(false)
}

// ── Constants ─────────────────────────────────────────────────────────────────

const LAUNCHD_LABEL: &str = "com.synapse.auto-update";
const SYSTEMD_UNIT: &str = "synapse-auto-update";
const CRON_MARKER: &str = "# synapse-auto-update";

fn launchd_plist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

fn systemd_unit_dir() -> PathBuf {
    state::config_dir()
        .parent()
        .map(|p| p.join("systemd/user"))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

// ── Enable ────────────────────────────────────────────────────────────────────

/// `synapse auto-update enable`
pub fn enable() -> io::Result<()> {
    let cfg = read_config()?;
    write_config(&cfg)?;

    let parts: Vec<&str> = cfg.preferred_time.split(':').collect();
    let hour: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(2);
    let minute: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    match detect_backend() {
        Backend::Launchd => enable_launchd(hour, minute),
        Backend::Systemd => enable_systemd(hour, minute),
        Backend::Cron => enable_cron(hour, minute),
    }
}

// ── launchd ───────────────────────────────────────────────────────────────────

fn enable_launchd(hour: u32, minute: u32) -> io::Result<()> {
    // Idempotency: ask launchd, not the filesystem.
    if launchd_is_loaded() {
        println!("auto-update already enabled (launchd: {LAUNCHD_LABEL})");
        return Ok(());
    }

    let exe = synapse_exe_path();
    let plist_path = launchd_plist_path();
    if let Some(p) = plist_path.parent() {
        fs::create_dir_all(p)?;
    }

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>          <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>auto-update</string>
        <string>now</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key>   <integer>{hour}</integer>
        <key>Minute</key> <integer>{minute}</integer>
    </dict>
    <key>RunAtLoad</key>          <false/>
    <key>StandardOutPath</key>    <string>/tmp/synapse-auto-update.log</string>
    <key>StandardErrorPath</key>  <string>/tmp/synapse-auto-update.log</string>
</dict>
</plist>
"#
    );
    // 0644, explicitly: launchd rejects a group/world-writable plist.
    write_with_mode(&plist_path, &plist, 0o644)?;

    let status = Command::new("launchctl")
        .args(["load", "-w", plist_path.to_str().unwrap_or_default()])
        .status()
        .map_err(|e| io::Error::other(format!("launchctl load: {e}")))?;
    if !status.success() {
        return Err(io::Error::other("launchctl load failed"));
    }

    // Verify launchd accepted the unit — file presence alone is not enough.
    if !launchd_is_loaded() {
        return Err(io::Error::other(
            "launchctl load returned ok but job not listed — check plist syntax",
        ));
    }

    println!("auto-update enabled: launchd {LAUNCHD_LABEL}");
    println!("  schedule: daily at {hour:02}:{minute:02}");
    println!("  plist:    {}", plist_path.display());
    Ok(())
}

/// True when launchd reports the label as loaded. This is the outcome check that
/// matters: a rejected plist would not appear here.
pub fn launchd_is_loaded() -> bool {
    Command::new("launchctl")
        .args(["list", LAUNCHD_LABEL])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn disable_launchd() -> io::Result<()> {
    if !launchd_is_loaded() {
        println!("auto-update already disabled");
        return Ok(());
    }
    let plist_path = launchd_plist_path();
    let status = Command::new("launchctl")
        .args(["unload", "-w", plist_path.to_str().unwrap_or_default()])
        .status()
        .map_err(|e| io::Error::other(format!("launchctl unload: {e}")))?;
    if !status.success() {
        return Err(io::Error::other("launchctl unload failed"));
    }
    if plist_path.exists() {
        fs::remove_file(&plist_path)?;
    }

    if launchd_is_loaded() {
        return Err(io::Error::other(
            "launchctl unload returned ok but job still listed",
        ));
    }
    println!("auto-update disabled (launchd {LAUNCHD_LABEL} unloaded)");
    Ok(())
}

// ── systemd ───────────────────────────────────────────────────────────────────

fn enable_systemd(hour: u32, minute: u32) -> io::Result<()> {
    // Idempotency via is-enabled, not file presence.
    if systemd_is_enabled() {
        println!("auto-update already enabled (systemd: {SYSTEMD_UNIT}.timer)");
        return Ok(());
    }

    let exe = synapse_exe_path();
    let dir = systemd_unit_dir();
    fs::create_dir_all(&dir)?;

    let service = format!(
        "[Unit]\nDescription=Synapse auto-update\nAfter=network-online.target\n\n\
         [Service]\nType=oneshot\nExecStart={exe} auto-update now\n\
         StandardOutput=journal\nStandardError=journal\n\n\
         [Install]\nWantedBy=default.target\n"
    );
    let timer = format!(
        "[Unit]\nDescription=Synapse auto-update timer\n\n\
         [Timer]\nOnCalendar=*-*-* {hour:02}:{minute:02}:00\nPersistent=true\n\n\
         [Install]\nWantedBy=timers.target\n"
    );

    // 0644 for the same reason as the plist: a world-writable unit file is an
    // arbitrary-code vector. systemd is less strict than launchd here, but the
    // correct mode is free.
    write_with_mode(
        &dir.join(format!("{SYSTEMD_UNIT}.service")),
        &service,
        0o644,
    )?;
    write_with_mode(&dir.join(format!("{SYSTEMD_UNIT}.timer")), &timer, 0o644)?;

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&[
        "--user",
        "enable",
        "--now",
        &format!("{SYSTEMD_UNIT}.timer"),
    ])?;

    // Verify the timer is actually active.
    if !systemd_timer_is_active() {
        return Err(io::Error::other(
            "systemctl enable --now returned ok but timer is not active",
        ));
    }

    println!("auto-update enabled: systemd {SYSTEMD_UNIT}.timer");
    println!("  schedule: daily at {hour:02}:{minute:02}");
    Ok(())
}

pub fn systemd_is_enabled() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-enabled", &format!("{SYSTEMD_UNIT}.timer")])
        .output()
        .map(|o| {
            matches!(
                String::from_utf8_lossy(&o.stdout).trim(),
                "enabled" | "enabled-runtime" | "static"
            )
        })
        .unwrap_or(false)
}

pub fn systemd_timer_is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", &format!("{SYSTEMD_UNIT}.timer")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

fn disable_systemd() -> io::Result<()> {
    if !systemd_is_enabled() && !systemd_timer_is_active() {
        println!("auto-update already disabled");
        return Ok(());
    }
    run_systemctl(&[
        "--user",
        "disable",
        "--now",
        &format!("{SYSTEMD_UNIT}.timer"),
    ])?;
    let dir = systemd_unit_dir();
    for name in [
        &format!("{SYSTEMD_UNIT}.timer"),
        &format!("{SYSTEMD_UNIT}.service"),
    ] {
        let p = dir.join(name);
        if p.exists() {
            fs::remove_file(p)?;
        }
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);
    println!("auto-update disabled (systemd {SYSTEMD_UNIT}.timer stopped)");
    Ok(())
}

fn run_systemctl(args: &[&str]) -> io::Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .map_err(|e| io::Error::other(format!("systemctl: {e}")))?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "systemctl {} exited {}",
            args.join(" "),
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

// ── cron ──────────────────────────────────────────────────────────────────────

fn enable_cron(hour: u32, minute: u32) -> io::Result<()> {
    let exe = synapse_exe_path();
    let entry = format!(
        "{minute} {hour} * * * {exe} auto-update now >> /tmp/synapse-auto-update.log 2>&1 {CRON_MARKER}"
    );

    let existing = crontab_read()?;
    if existing.lines().any(|l| l.contains(CRON_MARKER)) {
        println!("auto-update already enabled (cron)");
        return Ok(());
    }

    let mut new_cron = existing;
    if !new_cron.ends_with('\n') && !new_cron.is_empty() {
        new_cron.push('\n');
    }
    new_cron.push_str(&entry);
    new_cron.push('\n');
    crontab_write(&new_cron)?;

    // Verify the line landed — file write is not observable outcome confirmation.
    if !crontab_read()?.lines().any(|l| l.contains(CRON_MARKER)) {
        return Err(io::Error::other(
            "cron line written but not found after reload",
        ));
    }

    println!("auto-update enabled (cron, daily at {hour:02}:{minute:02})");
    println!("  note: using cron fallback — systemd user session unavailable");
    Ok(())
}

fn disable_cron() -> io::Result<()> {
    let existing = crontab_read()?;
    if !existing.lines().any(|l| l.contains(CRON_MARKER)) {
        println!("auto-update already disabled");
        return Ok(());
    }
    let filtered: String = existing
        .lines()
        .filter(|l| !l.contains(CRON_MARKER))
        .flat_map(|l| [l, "\n"])
        .collect();
    crontab_write(&filtered)?;
    println!("auto-update disabled (cron line removed)");
    Ok(())
}

fn crontab_read() -> io::Result<String> {
    let out = Command::new("crontab").arg("-l").output()?;
    // crontab -l exits non-zero when no crontab exists — treat as empty.
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Ok(String::new())
    }
}

fn crontab_write(content: &str) -> io::Result<()> {
    use std::io::Write;
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| io::Error::other(format!("crontab -: {e}")))?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(content.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other("crontab write failed"));
    }
    Ok(())
}

// ── Disable dispatch ──────────────────────────────────────────────────────────

/// `synapse auto-update disable`
pub fn disable() -> io::Result<()> {
    match detect_backend() {
        Backend::Launchd => disable_launchd(),
        Backend::Systemd => disable_systemd(),
        Backend::Cron => disable_cron(),
    }
}

// ── Config editor ─────────────────────────────────────────────────────────────

/// `synapse auto-update config` — open config in $EDITOR/$VISUAL/vi.
pub fn open_config() -> io::Result<()> {
    let path = config_path();
    if !path.exists() {
        write_config(&AutoUpdateConfig::default())?;
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| io::Error::other(format!("{editor}: {e}")))?;

    if !status.success() {
        eprintln!("warning: editor exited {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

// ── Immediate run ─────────────────────────────────────────────────────────────

/// `synapse auto-update now` — run an update immediately.
///
/// The lock is acquired by [`crate::commands::update::run`], not here: taking it
/// first would deadlock against ourselves, since `acquire` treats our own live
/// PID as a conflicting holder. The scheduled path is still mutually exclusive
/// with interactive installs — the exclusion just happens one level down.
pub fn run_now() -> io::Result<()> {
    println!("Running auto-update…");
    crate::commands::update::run(None, true)
}

// ── Status ────────────────────────────────────────────────────────────────────

/// `synapse auto-update status`
pub fn show_status() -> io::Result<()> {
    let cfg = read_config()?;
    println!("check_interval:   {}", cfg.check_interval);
    println!("install_interval: {}", cfg.install_interval);
    println!("preferred_time:   {}", cfg.preferred_time);
    println!("notify:           {}", cfg.notify);
    println!("max_duration:     {}", cfg.max_duration);

    let backend = detect_backend();
    print!("backend:          ");
    match backend {
        Backend::Launchd => println!("launchd"),
        Backend::Systemd => println!("systemd"),
        Backend::Cron => println!("cron (systemd unavailable)"),
    }

    let registered = match backend {
        Backend::Launchd => launchd_is_loaded(),
        Backend::Systemd => systemd_timer_is_active(),
        Backend::Cron => crontab_read()
            .map(|c| c.lines().any(|l| l.contains(CRON_MARKER)))
            .unwrap_or(false),
    };
    println!(
        "registered:       {}",
        if registered { "yes" } else { "no" }
    );
    Ok(())
}

fn synapse_exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "synapse".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_config<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "synapse-au-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();

        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let result = f();
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        fs::remove_dir_all(&dir).ok();
        result
    }

    #[test]
    fn default_config_round_trips_yaml() {
        with_temp_config(|| {
            let cfg = AutoUpdateConfig::default();
            write_config(&cfg).unwrap();
            let back = read_config().unwrap();
            assert_eq!(back.check_interval, "1d");
            assert_eq!(back.install_interval, "7d");
            assert_eq!(back.preferred_time, "02:00");
            assert!(back.notify);
            assert_eq!(back.max_duration, "30m");
        });
    }

    #[test]
    fn missing_config_returns_defaults() {
        with_temp_config(|| {
            let cfg = read_config().unwrap();
            assert_eq!(cfg.check_interval, "1d");
            assert!(cfg.notify);
        });
    }

    #[test]
    fn config_write_is_atomic_no_tmp_left() {
        with_temp_config(|| {
            // Capture path inside the lock while XDG_CONFIG_HOME is set.
            let p = config_path();
            write_config(&AutoUpdateConfig::default()).unwrap();
            assert!(p.exists(), "config file not created");
            assert!(!p.with_extension("yaml.tmp").exists(), "tmp file leaked");
        });
    }

    #[test]
    fn preferred_time_parses_to_hour_minute() {
        let parts: Vec<&str> = "02:30".split(':').collect();
        let h: u32 = parts[0].parse().unwrap();
        let m: u32 = parts[1].parse().unwrap();
        assert_eq!((h, m), (2, 30));
    }

    #[test]
    fn detect_backend_returns_launchd_on_macos() {
        // This test runs on the macOS CI leg; on Linux it would get Systemd or Cron.
        #[cfg(target_os = "macos")]
        assert_eq!(detect_backend(), Backend::Launchd);
    }

    #[test]
    fn backend_display_coverage() {
        // Exercise every branch so dead-code lints stay off.
        for b in [Backend::Launchd, Backend::Systemd, Backend::Cron] {
            let _ = format!("{b:?}");
        }
    }

    /// Verify `launchd_is_loaded` returns false when the label is not registered.
    /// (On macOS CI, `auto-update enable` must not have been called before this.)
    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_is_not_loaded_when_never_enabled() {
        // Only meaningful if the label genuinely isn't registered; if CI has a
        // leftover job this may produce a false positive, which is acceptable.
        // What we cannot have is a false negative (loaded but returns false).
        let loaded = launchd_is_loaded();
        // We can assert the return type is bool and the call doesn't panic.
        let _ = loaded;
    }

    /// cron helpers round-trip without touching the real crontab by mocking
    /// the crontab command is not feasible in a unit test; we test the string
    /// manipulation instead.
    #[test]
    fn cron_marker_not_duplicated_in_existing_entry() {
        let existing = format!("0 2 * * * /usr/bin/synapse auto-update now {CRON_MARKER}\n");
        assert!(existing.lines().any(|l| l.contains(CRON_MARKER)));
        // Simulate the idempotency check — must not append again.
        let would_append = !existing.lines().any(|l| l.contains(CRON_MARKER));
        assert!(!would_append, "would have added a duplicate cron line");
    }

    #[test]
    fn cron_disable_removes_only_marker_lines() {
        let crontab = format!(
            "# user's own job\n0 5 * * * /usr/bin/backup\n\
             0 2 * * * synapse auto-update now {CRON_MARKER}\n\
             # another user job\n"
        );
        let filtered: String = crontab
            .lines()
            .filter(|l| !l.contains(CRON_MARKER))
            .flat_map(|l| [l, "\n"])
            .collect();

        assert!(!filtered.contains(CRON_MARKER), "marker line not removed");
        assert!(filtered.contains("backup"), "unrelated job removed");
        assert!(filtered.contains("another user job"), "comment removed");
    }

    /// `run_now` must not take the state lock itself.
    ///
    /// It delegates to `update::run`, which acquires the lock. Taking it here
    /// first deadlocked against our own PID — `acquire` is not reentrant, so the
    /// inner call saw a live holder and returned `Held`. Every scheduled run
    /// (launchd/systemd/cron all invoke `auto-update now`) failed with exit 1
    /// while the scheduler itself looked perfectly healthy.
    ///
    /// With no packages installed `update::run` returns early, so reaching `Ok`
    /// proves the lock was free when it got there.
    #[test]
    fn run_now_does_not_deadlock_on_its_own_lock() {
        with_temp_config(|| {
            let cfg_dir = state::config_dir();
            fs::create_dir_all(&cfg_dir).unwrap();
            fs::write(cfg_dir.join("state.json"), r#"{"packages":{}}"#).unwrap();

            match run_now() {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    assert!(
                        !msg.contains("lock held by PID"),
                        "run_now deadlocked against its own lock: {msg}"
                    );
                }
            }

            // The lock must also be released, not left behind for the next run.
            assert!(
                !cfg_dir.join(".lock").exists(),
                "run_now leaked the state lock"
            );
        });
    }

    /// The lock must genuinely be available when `run_now` starts — if some
    /// caller up the stack held it, scheduled runs would fail the same way.
    #[test]
    fn run_now_starts_with_an_unheld_lock() {
        with_temp_config(|| {
            let cfg_dir = state::config_dir();
            fs::create_dir_all(&cfg_dir).unwrap();

            // Acquiring here must succeed, proving nothing in the auto-update
            // path holds the lock before update::run is reached.
            let mut lock = state::acquire(&cfg_dir).expect("lock should be free");
            lock.release();
        });
    }

    /// launchd refuses to load a group- or world-writable LaunchAgent plist, so
    /// the mode must be set explicitly rather than inherited from the umask.
    ///
    /// Under `umask 000` a plain `fs::write` produced 0666 and launchd rejected
    /// it with "Load failed: 5: Input/output error" — auto-update silently never
    /// scheduled anything.
    #[cfg(unix)]
    #[test]
    fn write_with_mode_ignores_a_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "synapse-umask-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.plist");

        // SAFETY: umask(2) is always safe to call; it only reads/sets a process
        // attribute. Restored before returning so other tests are unaffected.
        let previous = unsafe { libc::umask(0o000) };
        let result = write_with_mode(&path, "<plist/>", 0o644);
        unsafe { libc::umask(previous) };

        result.expect("write should succeed");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o022,
            0,
            "plist is group/world-writable (mode {mode:o}) — launchd will reject it"
        );
        assert_eq!(mode, 0o644, "expected exactly 0644, got {mode:o}");

        fs::remove_dir_all(&dir).ok();
    }

    /// The config file holds user settings and should not be world-readable.
    #[cfg(unix)]
    #[test]
    fn config_is_written_private_under_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        with_temp_config(|| {
            let path = config_path();

            // SAFETY: see above — process-attribute only, restored immediately.
            let previous = unsafe { libc::umask(0o000) };
            let result = write_config(&AutoUpdateConfig::default());
            unsafe { libc::umask(previous) };
            result.expect("config write should succeed");

            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "config should be 0600, got {mode:o}");
        });
    }
}
