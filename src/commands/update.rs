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

pub fn locate_flake_dir() -> std::path::PathBuf {
    // Check exe dir first (installed layout: synapse binary next to flake).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.join("flake.nix").exists() {
                return parent.to_path_buf();
            }
        }
    }
    // Fall back to cwd (dev layout).
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
}
