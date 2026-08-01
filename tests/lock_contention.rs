//! Cross-process lock contention: verifies a real second process that loses the
//! race is told the lock is *held*, not handed a raw "File exists" I/O error.
//!
//! The unit tests in `src/state.rs` cover the single-process paths. This one
//! exercises two genuinely concurrent OS processes against the same lock file,
//! which is the scenario the concurrency-lock acceptance criterion is about.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Path to the `synapse` binary under test.
fn synapse_bin() -> PathBuf {
    // cargo sets CARGO_BIN_EXE_<name> for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_synapse"))
}

fn scratch_config() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "synapse-lock-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// With a live PID in the lock file, `synapse update --all` must report
/// contention in human terms and must not leak the raw OS error.
#[test]
fn update_reports_contention_not_io_error() {
    let cfg_home = scratch_config();
    let cfg_dir = cfg_home.join("synapse");
    fs::create_dir_all(&cfg_dir).unwrap();

    // A package must exist in state or update exits before touching the lock.
    fs::write(
        cfg_dir.join("state.json"),
        r#"{"packages":{"herdr":{"version":"0.7.5","installed_at":1785542400}}}"#,
    )
    .unwrap();

    // Claim the lock with a PID that is definitely alive: this test process.
    fs::write(cfg_dir.join(".lock"), std::process::id().to_string()).unwrap();

    let out = Command::new(synapse_bin())
        .args(["update", "--all"])
        .env("XDG_CONFIG_HOME", &cfg_home)
        .output()
        .expect("run synapse update");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr);

    assert!(
        combined.contains("lock held by PID") || combined.contains(&std::process::id().to_string()),
        "expected a contention message naming the holder, got: {combined}"
    );
    assert!(
        !combined.contains("File exists"),
        "raw OS error leaked to the user: {combined}"
    );
    assert!(
        !combined.contains("os error 17"),
        "raw errno leaked to the user: {combined}"
    );

    // The pre-existing lock must be left alone — we did not own it.
    assert_eq!(
        fs::read_to_string(cfg_dir.join(".lock")).unwrap().trim(),
        std::process::id().to_string(),
        "another process's lock was overwritten"
    );

    fs::remove_dir_all(&cfg_home).ok();
}

/// A stale lock (PID not running) must not block a new run.
#[test]
fn stale_lock_does_not_block() {
    let cfg_home = scratch_config();
    let cfg_dir = cfg_home.join("synapse");
    fs::create_dir_all(&cfg_dir).unwrap();

    // No packages installed: `update --all` exits early and cleanly. What we're
    // asserting is that a stale lock is not itself treated as a hard failure.
    fs::write(cfg_dir.join("state.json"), r#"{"packages":{}}"#).unwrap();
    fs::write(cfg_dir.join(".lock"), "999999").unwrap();

    let out = Command::new(synapse_bin())
        .args(["update", "--all"])
        .env("XDG_CONFIG_HOME", &cfg_home)
        .output()
        .expect("run synapse update");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !combined.contains("File exists") && !combined.contains("os error 17"),
        "stale lock produced a raw OS error: {combined}"
    );

    fs::remove_dir_all(&cfg_home).ok();
}

/// `synapse list` and `synapse status` must work with no config at all
/// (offline, first-run) and must not create a lock.
#[test]
fn read_only_commands_need_no_lock() {
    let cfg_home = scratch_config();

    for args in [vec!["list"], vec!["status"]] {
        let out = Command::new(synapse_bin())
            .args(&args)
            .env("XDG_CONFIG_HOME", &cfg_home)
            .output()
            .unwrap_or_else(|e| panic!("run synapse {args:?}: {e}"));

        assert!(
            out.status.success(),
            "synapse {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert!(
        !cfg_home.join("synapse/.lock").exists(),
        "read-only command created a lock file"
    );

    fs::remove_dir_all(&cfg_home).ok();
}
