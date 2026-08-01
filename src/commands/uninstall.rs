use std::io;

use crate::state;

/// `synapse uninstall [package] [--all]`
///
/// Drops packages from Synapse's state and, once nothing is left managed,
/// removes the managed shell block. Store paths are left for
/// `nix-collect-garbage`: deleting them directly would break any other profile
/// that still references them.
pub fn run(package: Option<&str>, all: bool) -> io::Result<()> {
    let cfg = state::config_dir();
    let mut st = state::read(&cfg)?;

    if st.packages.is_empty() {
        println!("No packages installed.");
        return Ok(());
    }

    let targets: Vec<String> = match package {
        Some(name) if !all => {
            if !st.packages.contains_key(name) {
                eprintln!("error: '{name}' is not installed");
                std::process::exit(1);
            }
            vec![name.to_string()]
        }
        _ => {
            let mut keys: Vec<_> = st.packages.keys().cloned().collect();
            keys.sort();
            keys
        }
    };

    let _lock = state::acquire(&cfg).map_err(|e| io::Error::other(e.to_string()))?;

    for name in &targets {
        let version = st
            .packages
            .get(name)
            .map(|r| r.version.clone())
            .unwrap_or_default();
        st.remove_package(name);
        println!("  {name} {version} removed from Synapse state");
        let _ =
            crate::commands::log::append(&format!("{} uninstalled {name} {version}", now_secs()));
    }

    state::write(&cfg, &st)?;

    // Only strip shell config once nothing is managed; otherwise the remaining
    // packages lose their PATH entry.
    if st.packages.is_empty() {
        match remove_shell_config() {
            Ok(removed) if !removed.is_empty() => {
                println!("\nRemoved Synapse block from:");
                for path in removed {
                    println!("  {}", path.display());
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("warning: could not clean shell config: {e}"),
        }
    }

    println!("\nStore paths are left in place. To reclaim disk space:");
    println!("  nix-collect-garbage -d");

    Ok(())
}

/// Strip the managed marker block from every configured shell rc.
///
/// Returns the rc files that were actually modified.
fn remove_shell_config() -> io::Result<Vec<std::path::PathBuf>> {
    use crate::shell;

    let home = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h),
        Err(_) => return Ok(Vec::new()),
    };

    let mut removed = Vec::new();
    for sh in shell::detect_shells(&home, std::env::var("SHELL").ok().as_deref()) {
        let rc = sh.rc_path(&home);
        let content = match std::fs::read_to_string(&rc) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };

        let stripped = shell::strip_block(&content);
        if stripped == content {
            continue;
        }

        // Atomic replace, same discipline as writing the block.
        let tmp = rc.with_extension("synapse-tmp");
        std::fs::write(&tmp, stripped.as_bytes())?;
        std::fs::rename(&tmp, &rc)?;
        removed.push(rc);
    }

    Ok(removed)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
