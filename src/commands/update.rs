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

    let _lock = state::acquire(&cfg).map_err(|e| io::Error::other(e.to_string()))?;

    let nix_bin = match crate::nix::resolve_bin() {
        Some(b) => b,
        None => {
            let advice = crate::nix::advice(&crate::nix::NixStatus::Missing)
                .unwrap_or_else(|| "nix is required".to_string());
            return Err(io::Error::new(io::ErrorKind::NotFound, advice));
        }
    };
    let mut failures = Vec::new();

    for name in &targets {
        let pkg = PACKAGES.iter().find(|p| p.name == name);
        let nix_attr = pkg.map(|p| p.nix_attr).unwrap_or(name.as_str());

        let old_version = st
            .packages
            .get(name)
            .map(|r| r.version.clone())
            .unwrap_or_else(|| "unknown".into());

        print!("  {name}: updating…");
        io::Write::flush(&mut io::stdout())?;

        match build_package(nix_attr, &flake_dir, &nix_bin) {
            Ok((new_version, had_previous)) => {
                let store_path = store_path_of(nix_attr, &flake_dir, &nix_bin);
                st.set_package_with_path(name.clone(), new_version.clone(), store_path);
                if let Err(error) = state::write(&cfg, &st) {
                    let rollback = undo_profile_replace(&nix_bin, name, had_previous).err();
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
                if new_version == old_version {
                    println!(" already up to date ({old_version})");
                } else {
                    println!(" {old_version} → {new_version}");
                    let entry = format!(
                        "{} updated {name} {old_version} -> {new_version}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    );
                    let _ = crate::commands::log::append(&entry);
                }
            }
            Err(message) => {
                println!(" FAILED: {message}");
                failures.push(format!("{name}: {message}"));
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "package update failed: {}",
            failures.join("; ")
        )))
    }
}

pub fn flake_attr(flake_dir: &std::path::Path, attribute: &str) -> String {
    let explicit_local = std::env::var_os("SYNAPSE_FLAKE_DIR")
        .map(std::path::PathBuf::from)
        .is_some_and(|path| path == flake_dir && path.join("flake.nix").is_file());
    if explicit_local {
        return format!(".#{attribute}");
    }
    let reference = std::env::var("SYNAPSE_FLAKE_REF")
        .ok()
        .filter(|value| !value.is_empty() && !value.chars().any(char::is_whitespace))
        .unwrap_or_else(|| "github:thinhngotony/synapse".to_string());
    format!("{}#{attribute}", reference.trim_end_matches('#'))
}

pub fn synapse_profile() -> Result<std::path::PathBuf, String> {
    if let Some(path) = std::env::var_os("SYNAPSE_PROFILE") {
        if !path.is_empty() {
            return Ok(path.into());
        }
    }
    crate::state::home_dir()
        .map(|home| home.join(".local/share/synapse/profile"))
        .ok_or_else(|| "cannot resolve Synapse Nix profile path".to_string())
}

pub fn replace_profile_package(
    nix_bin: &str,
    name: &str,
    installable: &str,
    working_dir: &std::path::Path,
) -> Result<bool, String> {
    let profile = synapse_profile()?;
    let parent = profile
        .parent()
        .ok_or_else(|| "Synapse Nix profile has no parent directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create profile directory: {error}"))?;
    let binary = profile.join("bin").join(name);
    let had_previous = is_executable(&binary);
    if had_previous {
        remove_profile_package(nix_bin, name)?;
    }

    let status = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "--no-accept-flake-config",
            "profile",
            "install",
            "--profile",
        ])
        .arg(&profile)
        .arg(installable)
        .args(["--no-write-lock-file", "--refresh"])
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .status();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            if had_previous {
                let _ = rollback_profile(nix_bin);
            }
            return Err(format!("start nix profile install: {error}"));
        }
    };
    if !status.success() {
        if had_previous {
            let _ = rollback_profile(nix_bin);
        }
        return Err(format!(
            "nix profile install exited {}",
            status.code().unwrap_or(-1)
        ));
    }

    if !is_executable(&binary) {
        let _ = rollback_profile(nix_bin);
        if had_previous {
            let _ = rollback_profile(nix_bin);
        }
        return Err(format!(
            "profile install did not expose executable {}",
            binary.display()
        ));
    }
    Ok(had_previous)
}

pub fn remove_profile_package(nix_bin: &str, name: &str) -> Result<(), String> {
    let profile = synapse_profile()?;
    let status = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "--no-accept-flake-config",
            "profile",
            "remove",
            "--profile",
        ])
        .arg(profile)
        .arg(name)
        .status()
        .map_err(|error| format!("start nix profile remove: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "nix profile remove exited {}",
            status.code().unwrap_or(-1)
        ))
    }
}

fn rollback_profile(nix_bin: &str) -> Result<(), String> {
    let profile = synapse_profile()?;
    let status = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "--no-accept-flake-config",
            "profile",
            "rollback",
            "--profile",
        ])
        .arg(profile)
        .status()
        .map_err(|error| format!("start nix profile rollback: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "nix profile rollback exited {}",
            status.code().unwrap_or(-1)
        ))
    }
}

pub fn undo_profile_replace(nix_bin: &str, name: &str, had_previous: bool) -> Result<(), String> {
    if had_previous {
        rollback_profile(nix_bin)?;
        rollback_profile(nix_bin)?;
        let binary = synapse_profile()?.join("bin").join(name);
        if !is_executable(&binary) {
            return Err(format!(
                "profile rollback did not restore executable {}",
                binary.display()
            ));
        }
        Ok(())
    } else {
        remove_profile_package(nix_bin, name)
    }
}

pub fn undo_profile_remove(nix_bin: &str, name: &str) -> Result<(), String> {
    rollback_profile(nix_bin)?;
    let binary = synapse_profile()?.join("bin").join(name);
    if is_executable(&binary) {
        Ok(())
    } else {
        Err(format!(
            "profile rollback did not restore executable {}",
            binary.display()
        ))
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn build_package(
    nix_attr: &str,
    flake_dir: &std::path::Path,
    nix_bin: &str,
) -> Result<(String, bool), String> {
    let attr = flake_attr(flake_dir, nix_attr);
    let version_attr = flake_attr(flake_dir, &format!("{nix_attr}.version"));
    let out = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "--no-accept-flake-config",
            "eval",
            "--raw",
            &version_attr,
            "--no-write-lock-file",
        ])
        .current_dir(flake_dir)
        .output()
        .map_err(|e| format!("nix eval: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "nix eval exited {}",
            out.status.code().unwrap_or(-1)
        ));
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        return Err("nix eval returned an empty package version".to_string());
    }

    let had_previous = replace_profile_package(nix_bin, nix_attr, &attr, flake_dir)?;
    Ok((version, had_previous))
}

/// Resolve the Nix store path for an installed package.
///
/// The dedicated profile roots the current path; recording it also lets rollback
/// select the prior closure directly from state history.
pub fn store_path_of(nix_attr: &str, flake_dir: &std::path::Path, nix_bin: &str) -> Option<String> {
    let attr = flake_attr(flake_dir, nix_attr);
    let out = Command::new(nix_bin)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "--no-accept-flake-config",
            "path-info",
            &attr,
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

/// Resolve a working directory for Nix.
///
/// A local flake is trusted only through the explicit `SYNAPSE_FLAKE_DIR`
/// development override. For development workflows (running from a checkout),
/// we also walk up from the executable to find a local flake — the installed
/// layout (`~/.local/bin/synapse`) has no flake nearby, so this correctly
/// falls back to the remote flake. If no override and no local flake is found,
/// we return the current directory; `flake_attr` will then use the remote ref.
pub fn locate_flake_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("SYNAPSE_FLAKE_DIR") {
        let path = std::path::PathBuf::from(dir);
        if path.join("flake.nix").is_file() {
            return path;
        }
    }
    // Walk up from the executable — in a checkout (target/debug/synapse)
    // this finds the repo root within 5 parents. The installed layout
    // (~/.local/bin/synapse) has no flake nearby.
    if let Ok(exe) = std::env::current_exe() {
        let mut cursor = exe.parent();
        for _ in 0..5 {
            if let Some(dir) = cursor {
                if dir.join("flake.nix").is_file() {
                    return dir.to_path_buf();
                }
                cursor = dir.parent();
            } else {
                break;
            }
        }
    }
    // Fall back to CWD (dev workflow when running from repo root without
    // SYNAPSE_FLAKE_DIR set). `flake_attr` will use remote if no flake.nix.
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

    #[test]
    fn implicit_checkout_flake_is_not_trusted() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous_dir = std::env::var_os("SYNAPSE_FLAKE_DIR");
        let previous_ref = std::env::var_os("SYNAPSE_FLAKE_REF");
        std::env::remove_var("SYNAPSE_FLAKE_DIR");
        std::env::set_var("SYNAPSE_FLAKE_REF", "github:example/synapse");
        let attr = flake_attr(&locate_flake_dir(), "omp");
        match previous_dir {
            Some(value) => std::env::set_var("SYNAPSE_FLAKE_DIR", value),
            None => std::env::remove_var("SYNAPSE_FLAKE_DIR"),
        }
        match previous_ref {
            Some(value) => std::env::set_var("SYNAPSE_FLAKE_REF", value),
            None => std::env::remove_var("SYNAPSE_FLAKE_REF"),
        }
        assert_eq!(attr, "github:example/synapse#omp");
    }

    #[test]
    fn flake_attr_uses_explicit_local_flake() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = std::env::temp_dir().join(format!("synapse-local-flake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flake.nix"), "{}").unwrap();
        let previous = std::env::var_os("SYNAPSE_FLAKE_DIR");
        std::env::set_var("SYNAPSE_FLAKE_DIR", &dir);
        assert_eq!(flake_attr(&dir, "omp"), ".#omp");
        match previous {
            Some(value) => std::env::set_var("SYNAPSE_FLAKE_DIR", value),
            None => std::env::remove_var("SYNAPSE_FLAKE_DIR"),
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn flake_attr_uses_remote_when_no_local_flake_exists() {
        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("SYNAPSE_FLAKE_REF");
        std::env::set_var("SYNAPSE_FLAKE_REF", "github:example/synapse");
        let attr = flake_attr(std::path::Path::new("/"), "omp");
        match previous {
            Some(value) => std::env::set_var("SYNAPSE_FLAKE_REF", value),
            None => std::env::remove_var("SYNAPSE_FLAKE_REF"),
        }
        assert_eq!(attr, "github:example/synapse#omp");
    }

    #[cfg(unix)]
    #[test]
    fn package_install_uses_remote_flake_and_roots_executable() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("synapse-remote-flake-{}", std::process::id()));
        let fake_nix = dir.join("nix");
        let log = dir.join("nix.log");
        let profile = dir.join("profile");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &fake_nix,
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"$SYNAPSE_NIX_TEST_LOG\"\n\
             case \" $* \" in\n\
               *' profile install '*)\n\
                 mkdir -p \"$SYNAPSE_PROFILE/bin\"\n\
                 printf '#!/bin/sh\\n' > \"$SYNAPSE_PROFILE/bin/omp\"\n\
                 chmod 755 \"$SYNAPSE_PROFILE/bin/omp\"\n\
                 ;;\n\
               *' eval '*) printf '18.0.4' ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_nix, std::fs::Permissions::from_mode(0o755)).unwrap();
        let previous_ref = std::env::var_os("SYNAPSE_FLAKE_REF");
        let previous_log = std::env::var_os("SYNAPSE_NIX_TEST_LOG");
        let previous_profile = std::env::var_os("SYNAPSE_PROFILE");
        std::env::set_var("SYNAPSE_FLAKE_REF", "github:example/synapse");
        std::env::set_var("SYNAPSE_NIX_TEST_LOG", &log);
        std::env::set_var("SYNAPSE_PROFILE", &profile);

        assert_eq!(
            build_package("omp", &dir, fake_nix.to_str().unwrap())
                .unwrap()
                .0,
            "18.0.4"
        );
        let calls = std::fs::read_to_string(&log).unwrap();

        match previous_ref {
            Some(value) => std::env::set_var("SYNAPSE_FLAKE_REF", value),
            None => std::env::remove_var("SYNAPSE_FLAKE_REF"),
        }
        match previous_log {
            Some(value) => std::env::set_var("SYNAPSE_NIX_TEST_LOG", value),
            None => std::env::remove_var("SYNAPSE_NIX_TEST_LOG"),
        }
        match previous_profile {
            Some(value) => std::env::set_var("SYNAPSE_PROFILE", value),
            None => std::env::remove_var("SYNAPSE_PROFILE"),
        }

        assert!(!calls.contains("profile remove"));
        assert!(calls.contains("profile install"));
        assert!(calls.contains("github:example/synapse#omp"));
        assert!(!calls.contains(" build "));
        assert!(profile.join("bin/omp").is_file());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn update_failure_is_nonzero_and_preserves_state_and_profile() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::test_utils::XDG_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir =
            std::env::temp_dir().join(format!("synapse-update-failure-{}", std::process::id()));
        let fake_nix = dir.join("nix");
        let config = dir.join("config");
        let profile = dir.join("profile");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &fake_nix,
            "#!/bin/sh\n\
             if [ \"$1\" = --version ]; then\n\
               echo 'nix (Nix) 2.34.0'\n\
               exit 0\n\
             fi\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_nix, std::fs::Permissions::from_mode(0o755)).unwrap();

        let previous_path = std::env::var_os("PATH");
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_profile = std::env::var_os("SYNAPSE_PROFILE");
        std::env::set_var("PATH", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("SYNAPSE_PROFILE", &profile);

        let cfg = state::config_dir();
        let mut before = state::State::default();
        before.set_package_with_path("herdr", "0.7.4", Some("/nix/store/old-herdr".into()));
        state::write(&cfg, &before).unwrap();
        let error = run(Some("herdr"), false).unwrap_err();
        let after = state::read(&cfg).unwrap();

        match previous_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        match previous_config {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match previous_profile {
            Some(value) => std::env::set_var("SYNAPSE_PROFILE", value),
            None => std::env::remove_var("SYNAPSE_PROFILE"),
        }

        assert!(error.to_string().contains("package update failed"));
        assert_eq!(after, before);
        assert!(!profile.exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
