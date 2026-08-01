use std::io;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state;
use crate::tui::PACKAGES;

/// `synapse update [package] [--all]`
pub fn run(package: Option<&str>, all: bool) -> io::Result<()> {
    let cfg = state::config_dir();
    let mut st = state::read(&cfg)?;

    if st.packages.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    // Determine which packages to update.
    let targets: Vec<String> = match package {
        Some(name) if !all => {
            if !st.packages.contains_key(name) {
                eprintln!("error: '{name}' is not installed");
                std::process::exit(1);
            }
            vec![name.to_string()]
        }
        // `--all`, or no package named: every installed package.
        _ => {
            let mut keys: Vec<_> = st.packages.keys().cloned().collect();
            keys.sort();
            keys
        }
    };

    // Find the flake dir — use the directory of the running binary, falling
    // back to the current directory (works for dev and installed layouts).
    let flake_dir = locate_flake_dir();
    let nix_bin = match crate::nix::resolve_bin() {
        Some(b) => b,
        None => {
            eprintln!("error: nix not found; cannot build packages");
            if let Some(a) = crate::nix::advice(&crate::nix::NixStatus::Missing) {
                eprintln!("\n{a}");
            }
            std::process::exit(1);
        }
    };

    let _lock = state::acquire(&cfg).map_err(|e| io::Error::other(e.to_string()))?;

    let mut any_updated = false;
    for name in &targets {
        let pkg = PACKAGES.iter().find(|p| p.name == name);
        let nix_attr = pkg.map(|p| p.nix_attr).unwrap_or(name.as_str());

        let old_version = st
            .packages
            .get(name)
            .map(|r| r.version.clone())
            .unwrap_or_else(|| "unknown".into());

        print!("  {name}: building…");
        io::Write::flush(&mut io::stdout())?;

        match build_package(nix_attr, &flake_dir, &nix_bin) {
            Ok(new_version) => {
                if new_version == old_version {
                    println!(" already up to date ({old_version})");
                } else {
                    println!(" {old_version} → {new_version}");
                    // Route through set_package_with_path rather than inserting a
                    // PackageRecord literal: it maintains the rollback history and
                    // does not need touching when the record gains a field.
                    let store_path = store_path_of(nix_attr, &flake_dir, &nix_bin);
                    st.set_package_with_path(name.clone(), new_version.clone(), store_path);

                    let entry = format!(
                        "{} updated {name} {old_version} -> {new_version}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    );
                    let _ = crate::commands::log::append(&entry);
                    any_updated = true;
                }
            }
            Err(msg) => {
                println!(" FAILED: {msg}");
            }
        }
    }

    if any_updated {
        state::write(&cfg, &st)?;
    }

    Ok(())
}

fn build_package(
    nix_attr: &str,
    flake_dir: &std::path::Path,
    nix_bin: &str,
) -> Result<String, String> {
    let status = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "build",
            &format!(".#{nix_attr}"),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("spawn nix: {e}"))?;

    if !status.success() {
        return Err(format!("nix build exited {}", status.code().unwrap_or(-1)));
    }

    // Read new version.
    let out = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "eval",
            "--raw",
            &format!(".#{nix_attr}.version"),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .output()
        .map_err(|e| format!("nix eval: {e}"))?;

    Ok(if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        "unknown".into()
    })
}

/// Resolve the Nix store path for a built package.
///
/// Recorded in state so `synapse rollback` can re-point at an old closure that
/// is still in the store instead of rebuilding it. Returns `None` when the path
/// cannot be resolved — rollback then falls back to a rebuild.
pub fn store_path_of(nix_attr: &str, flake_dir: &std::path::Path, nix_bin: &str) -> Option<String> {
    let out = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "path-info",
            &format!(".#{nix_attr}"),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    // path-info prints one store path per line; the package's own is the first.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|s| s.starts_with("/nix/store/"))
        .map(str::to_string)
}

/// Locate the directory containing `flake.nix`.
///
/// Order matters, and the CWD is deliberately *last*: launchd runs jobs with
/// `PWD=/`, so a scheduled `auto-update now` that trusted the working directory
/// would look for the flake in `/`, fail every package build, and report FAILED
/// while the same command worked perfectly from an interactive shell.
///
/// Checked in order:
/// 1. `SYNAPSE_FLAKE_DIR`, for an explicit override.
/// 2. Next to the executable (installed layout).
/// 3. Walking up from the executable, covering `target/release/synapse` in a
///    dev checkout and `<prefix>/bin/synapse` beside a shared flake.
/// 4. The current directory, last, for `cargo run` from the repo root.
pub fn locate_flake_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("SYNAPSE_FLAKE_DIR") {
        let path = std::path::PathBuf::from(dir);
        if path.join("flake.nix").is_file() {
            return path;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        // Walk up from the binary; bounded so a deep path cannot spin.
        let mut cursor = exe.parent();
        for _ in 0..5 {
            let Some(dir) = cursor else { break };
            if dir.join("flake.nix").is_file() {
                return dir.to_path_buf();
            }
            cursor = dir.parent();
        }
    }

    // Only trust the CWD if it actually holds a flake, so a scheduled run from
    // `/` does not silently proceed with a directory that cannot possibly work.
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("flake.nix").is_file() {
            return cwd;
        }
    }

    // Nothing found. Return the CWD so the caller's `nix build` fails with a
    // real message rather than us inventing a path that does not exist.
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_flake_dir_returns_usable_path() {
        // Must always yield something a Command can be given as cwd.
        let p = locate_flake_dir();
        assert!(p.is_absolute() || p == std::path::Path::new("."));
    }

    /// The override must win, and must be honoured even when the CWD has no
    /// flake — which is the situation a scheduled run is always in.
    #[test]
    fn flake_dir_override_is_honoured() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "synapse-flakedir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flake.nix"), "{}").unwrap();

        let prev = std::env::var_os("SYNAPSE_FLAKE_DIR");
        std::env::set_var("SYNAPSE_FLAKE_DIR", &dir);
        let found = locate_flake_dir();
        match prev {
            Some(v) => std::env::set_var("SYNAPSE_FLAKE_DIR", v),
            None => std::env::remove_var("SYNAPSE_FLAKE_DIR"),
        }

        assert_eq!(found, dir, "override was ignored");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An override pointing somewhere without a flake must be ignored rather
    /// than returned, so we never hand `nix build` a directory we know is wrong.
    #[test]
    fn flake_dir_override_without_a_flake_is_ignored() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let empty = std::env::temp_dir().join(format!(
            "synapse-noflake-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&empty).unwrap();

        let prev = std::env::var_os("SYNAPSE_FLAKE_DIR");
        std::env::set_var("SYNAPSE_FLAKE_DIR", &empty);
        let found = locate_flake_dir();
        match prev {
            Some(v) => std::env::set_var("SYNAPSE_FLAKE_DIR", v),
            None => std::env::remove_var("SYNAPSE_FLAKE_DIR"),
        }

        assert_ne!(
            found, empty,
            "returned an override directory that contains no flake.nix"
        );
        std::fs::remove_dir_all(&empty).ok();
    }

    /// In this checkout the binary lives at `target/{debug,release}/synapse`, so
    /// walking up from the executable must find the repo's own flake without any
    /// reliance on the current directory.
    #[test]
    fn locates_flake_by_walking_up_from_the_executable() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var_os("SYNAPSE_FLAKE_DIR");
        std::env::remove_var("SYNAPSE_FLAKE_DIR");
        let found = locate_flake_dir();
        if let Some(v) = prev {
            std::env::set_var("SYNAPSE_FLAKE_DIR", v);
        }

        assert!(
            found.join("flake.nix").is_file(),
            "resolved {found:?}, which has no flake.nix — a scheduled run from / \
             would fail every package build"
        );
    }
}
