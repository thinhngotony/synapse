use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::state;
use crate::tui::PACKAGES;

/// `synapse rollback [package]`
///
/// Reverts one package, or every installed package, to its most recent prior
/// version. Nix keeps old store paths until a garbage collection, so when the
/// recorded path is still present this is a re-point rather than a rebuild.
pub fn run(package: Option<&str>) -> io::Result<()> {
    let cfg = state::config_dir();
    let mut st = state::read(&cfg)?;

    if st.packages.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    let targets: Vec<String> = match package {
        Some(name) => {
            if !st.packages.contains_key(name) {
                eprintln!("error: '{name}' is not installed");
                std::process::exit(1);
            }
            vec![name.to_string()]
        }
        None => {
            let mut keys: Vec<_> = st.packages.keys().cloned().collect();
            keys.sort();
            keys
        }
    };

    // Report packages with no history up front rather than mid-run.
    let rollable: Vec<String> = targets
        .iter()
        .filter(|n| st.packages.get(*n).is_some_and(|r| !r.history.is_empty()))
        .cloned()
        .collect();

    if rollable.is_empty() {
        println!("Nothing to roll back to — no previous versions recorded.");
        if package.is_none() {
            println!("A rollback target is recorded the first time a package is updated.");
        }
        return Ok(());
    }

    for skipped in targets.iter().filter(|n| !rollable.contains(n)) {
        println!("  {skipped}: no previous version, skipping");
    }

    let _lock = state::acquire(&cfg).map_err(|e| io::Error::other(e.to_string()))?;
    let flake_dir = crate::commands::update::locate_flake_dir();

    let mut changed = false;
    for name in &rollable {
        // Capture the target before mutating state so we can report and verify.
        let target = st
            .packages
            .get(name)
            .and_then(|r| r.history.first().cloned())
            .expect("rollable implies non-empty history");

        let restored = match &target.store_path {
            // Old closure still in the store: no rebuild needed.
            Some(path) if Path::new(path).exists() => {
                println!(
                    "  {name}: reusing existing store path for {}",
                    target.version
                );
                true
            }
            // Path GC'd or never recorded: rebuild the pinned version.
            _ => {
                println!(
                    "  {name}: store path unavailable, rebuilding {}",
                    target.version
                );
                rebuild(name, &flake_dir).unwrap_or_else(|e| {
                    eprintln!("  {name}: rollback failed: {e}");
                    false
                })
            }
        };

        if !restored {
            continue;
        }

        if let Some((from, to)) = st.rollback_package(name) {
            println!("  {name}: {from} → {} (rolled back)", to.version);
            let _ = crate::commands::log::append(&format!(
                "{} rolled back {name} {from} -> {}",
                now_secs(),
                to.version
            ));
            changed = true;
        }
    }

    if changed {
        state::write(&cfg, &st)?;
    }

    Ok(())
}

/// Rebuild a package from the flake so its store path exists again.
fn rebuild(nix_attr: &str, flake_dir: &Path) -> Result<bool, String> {
    let attr = PACKAGES
        .iter()
        .find(|p| p.name == nix_attr)
        .map(|p| p.nix_attr)
        .unwrap_or(nix_attr);

    let nix_bin = crate::nix::resolve_bin().ok_or_else(|| "nix not found".to_string())?;
    let status = Command::new(&nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "build",
            &format!(".#{attr}"),
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("spawn nix: {e}"))?;

    if status.success() {
        Ok(true)
    } else {
        Err(format!("nix build exited {}", status.code().unwrap_or(-1)))
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
