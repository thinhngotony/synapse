use std::io;
use std::path::Path;

use crate::state;

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
    let nix_bin = crate::nix::resolve_bin()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nix not found"))?;
    let mut failures = Vec::new();

    for name in &rollable {
        let target = st
            .packages
            .get(name)
            .and_then(|record| record.history.first().cloned())
            .expect("rollable implies non-empty history");

        let had_previous = match &target.store_path {
            Some(path) if Path::new(path).exists() => {
                println!("  {name}: restoring profile path for {}", target.version);
                match crate::commands::update::replace_profile_package(
                    &nix_bin, name, path, &flake_dir,
                ) {
                    Ok(had_previous) => had_previous,
                    Err(error) => {
                        eprintln!("  {name}: rollback failed: {error}");
                        failures.push(format!("{name}: {error}"));
                        continue;
                    }
                }
            }
            _ => {
                let message = format!(
                    "rollback path for {} is unavailable; profile history was removed",
                    target.version
                );
                eprintln!("  {name}: {message}");
                failures.push(format!("{name}: {message}"));
                continue;
            }
        };

        let Some((from, to)) = st.rollback_package(name) else {
            let _ = crate::commands::update::undo_profile_replace(&nix_bin, name, had_previous);
            return Err(io::Error::other("rollback state changed unexpectedly"));
        };
        if let Err(error) = state::write(&cfg, &st) {
            let rollback =
                crate::commands::update::undo_profile_replace(&nix_bin, name, had_previous).err();
            return Err(io::Error::new(
                error.kind(),
                match rollback {
                    Some(rollback) => {
                        format!("write state: {error}; profile rollback: {rollback}")
                    }
                    None => format!("write state: {error}"),
                },
            ));
        }
        println!("  {name}: {from} → {} (rolled back)", to.version);
        let _ = crate::commands::log::append(&format!(
            "{} rolled back {name} {from} -> {}",
            now_secs(),
            to.version
        ));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "package rollback failed: {}",
            failures.join("; ")
        )))
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
