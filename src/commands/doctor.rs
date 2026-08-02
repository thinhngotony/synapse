use std::io;
use std::path::Path;

use crate::nix;
use crate::state;

/// `synapse doctor` — diagnose PATH, symlinks, stale locks.
pub fn run() -> io::Result<()> {
    let mut issues = 0usize;

    // 1. Nix available and supported.
    match nix::detect() {
        nix::NixStatus::Supported(v) => println!("  ✓  nix {v}"),
        nix::NixStatus::TooOld(v) => {
            println!("  ✗  nix {v} is too old (need 2.24+)");
            issues += 1;
        }
        nix::NixStatus::Missing => {
            println!("  ✗  nix not found on PATH");
            issues += 1;
        }
    }

    // 2. Installed binaries reachable on PATH.
    let cfg = state::config_dir();
    let st = state::read(&cfg)?;
    for name in st.packages.keys() {
        let found = which(name);
        if found {
            println!("  ✓  {name} on PATH");
        } else {
            println!("  ✗  {name} not found on PATH (is Nix profile active?)");
            issues += 1;
        }
    }

    // 3. Stale lock file.
    let lock_path = cfg.join(".lock");
    if lock_path.exists() {
        match std::fs::read_to_string(&lock_path) {
            Ok(raw) => {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    if is_pid_running(pid) {
                        println!("  ✓  lock file (PID {pid} is running)");
                    } else {
                        println!("  ⚠  stale lock file (PID {pid} is gone) — safe to delete");
                        println!("     rm {}", lock_path.display());
                        issues += 1;
                    }
                } else {
                    println!("  ⚠  lock file unreadable — may be corrupt");
                    issues += 1;
                }
            }
            Err(e) => {
                println!("  ⚠  could not read lock file: {e}");
                issues += 1;
            }
        }
    } else {
        println!("  ✓  no lock file");
    }

    // 4. Broken symlinks in Nix profile (best-effort).
    check_nix_profile_symlinks(&mut issues);

    if issues == 0 {
        println!("\nAll checks passed.");
    } else {
        println!("\n{issues} issue(s) found.");
    }

    Ok(())
}

fn which(bin: &str) -> bool {
    std::process::Command::new("which")
        .arg(bin)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn is_pid_running(pid: u32) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn is_pid_running(_pid: u32) -> bool {
    true
}

fn check_nix_profile_symlinks(issues: &mut usize) {
    // Standard Nix single-user profile location.
    let home = std::env::var("HOME").unwrap_or_default();
    let profile_bin = Path::new(&home).join(".nix-profile/bin");
    if !profile_bin.exists() {
        return; // Nix profile not present — not necessarily an error here
    }
    let rd = match std::fs::read_dir(&profile_bin) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(_) => {}
                Err(_) => {
                    println!("  ✗  broken symlink: {}", path.display());
                    *issues += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_binary() {
        assert!(
            which("true"),
            "`which`/`sh` must be available in the build/test environment"
        );
    }

    #[test]
    fn which_misses_nonexistent() {
        assert!(!which("synapse-totally-absent-binary-xyzzy"));
    }
}
