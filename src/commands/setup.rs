use std::fs;
use std::io;
use std::path::PathBuf;

use clap::CommandFactory;
use clap_complete::{generate_to, shells};

use crate::shell::{self, RcOutcome, Shell};

/// `synapse setup-shell` — PATH entries, completions, and verification.
///
/// Idempotent: re-running replaces the managed rc block instead of appending.
pub fn run(dry_run: bool) -> io::Result<()> {
    let home = PathBuf::from(
        std::env::var("HOME")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?,
    );
    let shell_env = std::env::var("SHELL").ok();

    let shells_found = shell::detect_shells(&home, shell_env.as_deref());
    if shells_found.is_empty() {
        println!("No supported shell detected (bash, zsh, or fish).");
        println!("Add ~/.nix-profile/bin to PATH manually.");
        return Ok(());
    }

    if dry_run {
        println!("Would configure:");
        for sh in &shells_found {
            let rc = sh.rc_path(&home);
            let completion_line = shell::completion_rc_line(*sh);
            // Same pure computation the real run uses, so the prediction cannot
            // disagree with what `setup-shell` would actually do.
            let verdict = match shell::plan_rc(*sh, &home, completion_line.as_deref()) {
                Ok(RcOutcome::Added) => "would add managed block".to_string(),
                Ok(RcOutcome::Updated) => "would replace existing managed block".to_string(),
                Ok(RcOutcome::Unchanged) => "already configured, no change".to_string(),
                Err(e) => format!("could not read: {e}"),
            };
            println!("  {:<5} {} — {verdict}", sh.name(), rc.display());

            let comp = shell::completion_dir(*sh, &home).join(shell::completion_file(*sh));
            let comp_verdict = if comp.exists() {
                "would regenerate"
            } else {
                "would create"
            };
            println!("        completions: {} — {comp_verdict}", comp.display());
        }
        return Ok(());
    }

    let mut touched: Vec<PathBuf> = Vec::new();

    for sh in &shells_found {
        // Completions first: the rc line we write must point at a real file.
        match write_completions(*sh, &home) {
            Ok(path) => println!("  completions → {}", path.display()),
            Err(e) => eprintln!("  warning: {} completions failed: {e}", sh.name()),
        }

        let completion_line = shell::completion_rc_line(*sh);
        match shell::configure_rc(*sh, &home, completion_line.as_deref()) {
            Ok(RcOutcome::Added) => {
                println!("  {} → {} (added)", sh.name(), sh.rc_path(&home).display());
                touched.push(sh.rc_path(&home));
            }
            Ok(RcOutcome::Updated) => {
                println!(
                    "  {} → {} (updated)",
                    sh.name(),
                    sh.rc_path(&home).display()
                );
                touched.push(sh.rc_path(&home));
            }
            Ok(RcOutcome::Unchanged) => {
                println!("  {} → already configured", sh.name());
            }
            Err(e) => eprintln!(
                "  warning: could not update {}: {e}",
                sh.rc_path(&home).display()
            ),
        }
    }

    // Verify the managed binaries respond, reporting each one.
    let installed = verify_installed()?;

    let current = shell_env
        .as_deref()
        .and_then(|s| {
            let base = s.rsplit('/').next().unwrap_or(s);
            shell::ALL_SHELLS
                .iter()
                .copied()
                .find(|sh| sh.name() == base)
        })
        .or_else(|| shells_found.first().copied());

    print!("\n{}", shell::next_steps(&installed, &touched, current));

    Ok(())
}

/// Generate the completion script for `shell` and return where it landed.
fn write_completions(sh: Shell, home: &std::path::Path) -> io::Result<PathBuf> {
    let dir = shell::completion_dir(sh, home);
    fs::create_dir_all(&dir)?;

    let mut cmd = crate::Cli::command();
    let bin = "synapse";

    let generated = match sh {
        Shell::Bash => generate_to(shells::Bash, &mut cmd, bin, &dir)?,
        Shell::Zsh => generate_to(shells::Zsh, &mut cmd, bin, &dir)?,
        Shell::Fish => generate_to(shells::Fish, &mut cmd, bin, &dir)?,
    };

    // The rc lines we write assume a specific filename per shell (zsh in
    // particular only autoloads `_synapse`). If clap_complete ever changes its
    // naming, fail loudly here rather than silently shipping dead completions.
    let expected = shell::completion_file(sh);
    if generated.file_name().and_then(|n| n.to_str()) != Some(expected) {
        return Err(io::Error::other(format!(
            "{} completion written as {:?}, expected {expected}",
            sh.name(),
            generated.file_name().unwrap_or_default()
        )));
    }

    Ok(generated)
}

/// Run `<bin> --version` for each package recorded in state.
///
/// Returns `(name, version_line)` for the ones that responded. Failures are
/// reported to stderr but do not abort setup: a package can be installed and
/// simply not yet on PATH until the user reloads their shell.
fn verify_installed() -> io::Result<Vec<(String, String)>> {
    use crate::state;

    let cfg = state::config_dir();
    let st = state::read(&cfg)?;

    let mut names: Vec<&String> = st.packages.keys().collect();
    names.sort();

    let mut ok = Vec::new();
    for name in names {
        match shell::verify_binary(name) {
            Ok(line) => {
                println!("  ✓  {line}");
                let recorded = st
                    .packages
                    .get(name)
                    .map(|r| r.version.clone())
                    .unwrap_or_default();
                ok.push((name.clone(), recorded));
            }
            Err(e) => {
                eprintln!("  ✗  {e} (not on PATH yet?)");
            }
        }
    }

    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_scripts_generate_for_every_shell() {
        let home = std::env::temp_dir().join(format!(
            "synapse-setup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&home).unwrap();

        for sh in shell::ALL_SHELLS {
            let path = write_completions(sh, &home)
                .unwrap_or_else(|e| panic!("{} completions: {e}", sh.name()));
            assert!(path.exists(), "{} completion file missing", sh.name());

            let content = fs::read_to_string(&path).unwrap();
            assert!(
                !content.is_empty(),
                "{} completion script is empty",
                sh.name()
            );
            // Every generated script must know the binary name.
            assert!(
                content.contains("synapse"),
                "{} completion missing binary name",
                sh.name()
            );
        }

        // zsh's script must use the autoload-compatible filename.
        assert!(
            shell::completion_dir(Shell::Zsh, &home)
                .join("_synapse")
                .exists(),
            "zsh completion not named _synapse"
        );

        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn generated_completions_cover_subcommands() {
        let home = std::env::temp_dir().join(format!(
            "synapse-setup-cmds-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&home).unwrap();

        let path = write_completions(Shell::Fish, &home).unwrap();
        let content = fs::read_to_string(&path).unwrap();

        // If a subcommand is added without regenerating, this catches it.
        for cmd in ["status", "doctor", "list", "update", "log", "version"] {
            assert!(
                content.contains(cmd),
                "fish completions missing subcommand: {cmd}"
            );
        }

        fs::remove_dir_all(&home).ok();
    }
}
