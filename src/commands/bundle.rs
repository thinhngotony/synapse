//! All-in-one bundle: export everything (stack + secrets + state) as an
//! encrypted, plug-and-play archive.
//!
//! `synapse bundle export --encrypt` creates a single file that contains the
//! entire portable stack *plus* resolved secrets.  With `--encrypt` the bundle
//! is encrypted with a passphrase; only someone with the password can
//! `synapse bundle import` it.  Without `--encrypt` the bundle is just a
//! compressed tarball with externalized secrets (safe for git).

use std::collections::BTreeMap;
use std::env;
use std::fs::{self};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::stack;

fn default_bundle_path(encrypt: bool) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = if encrypt {
        format!("synapse-bundle-{ts}.tar.zst.age")
    } else {
        format!("synapse-bundle-{ts}.tar.zst")
    };
    PathBuf::from(name)
}

fn resolve_secret_indirection(value: &str) -> Option<String> {
    let cmd_str = value.strip_prefix('!')?.trim();
    if cmd_str.is_empty() {
        return None;
    }
    let output = Command::new("sh").arg("-c").arg(cmd_str).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn collect_resolved_secrets(manifest: &serde_json::Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(profiles) = manifest.get("omp_profiles").and_then(|v| v.as_array()) {
        for profile in profiles {
            if let Some(mcp) = profile.get("mcp") {
                if let Some(servers) = mcp.get("mcpServers").and_then(|v| v.as_object()) {
                    for (_, srv) in servers {
                        if let Some(env) = srv.get("env").and_then(|v| v.as_object()) {
                            for (k, v) in env {
                                if let Some(s) = v.as_str() {
                                    if s.starts_with('!') {
                                        if let Some(resolved) = resolve_secret_indirection(s) {
                                            out.insert(k.clone(), resolved);
                                        }
                                    } else if s == k {
                                        if let Ok(val) = env::var(k) {
                                            if !val.is_empty() {
                                                out.insert(k.clone(), val);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(req) = profile.get("required_env").and_then(|v| v.as_array()) {
                for item in req {
                    if let Some(name) = item.as_str() {
                        if !out.contains_key(name) {
                            if let Ok(val) = env::var(name) {
                                if !val.is_empty() {
                                    out.insert(name.to_string(), val);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

fn encrypt_file(input: &Path, output: &Path, password: &str) -> io::Result<()> {
    // Try `age` first (preferred, modern, audited)
    if Command::new("age").arg("--version").output().is_ok() {
        let _status = Command::new("age")
            .args(["--passphrase", "--output", output.to_str().unwrap(), input.to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    stdin.write_all(format!("{password}\n").as_bytes())?;
                }
                child.wait()
            });
        if let Ok(status) = std::process::Command::new("age")
            .args(["--passphrase", "--output", output.to_str().unwrap(), input.to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    stdin.write_all(format!("{password}\n").as_bytes())?;
                }
                child.wait()
            }) {
                if status.success() {
                    return Ok(());
                }
            }
    }
    // Fallback: openssl aes-256-cbc (widely available)
    if Command::new("openssl").arg("version").output().is_ok() {
        let status = Command::new("openssl")
            .args([
                "enc", "-aes-256-cbc", "-pbkdf2",
                "-in", input.to_str().unwrap(),
                "-out", output.to_str().unwrap(),
                "-pass", &format!("pass:{password}"),
            ])
            .status()?;
        if status.success() {
            return Ok(());
        }
    }
    // Final fallback: XOR obfuscation (NOT secure, but better than plaintext for demo)
    // In production, require `age` or `openssl`.
    eprintln!(
        "warning: age and openssl not found, using insecure XOR obfuscation — install `age` via `brew install age` for real encryption"
    );
    let mut data = fs::read(input)?;
    let key = password.as_bytes();
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i % key.len()];
    }
    fs::write(output, data)?;
    Ok(())
}

fn decrypt_file(input: &Path, output: &Path, password: &str) -> io::Result<()> {
    if Command::new("age").arg("--version").output().is_ok() {
        let status = Command::new("age")
            .args(["--decrypt", "--output", output.to_str().unwrap(), input.to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    stdin.write_all(format!("{password}\n").as_bytes())?;
                }
                child.wait()
            });
        if let Ok(status) = status {
            if status.success() {
                return Ok(());
            }
        }
    }
    if Command::new("openssl").arg("version").output().is_ok() {
        let status = Command::new("openssl")
            .args([
                "enc", "-d", "-aes-256-cbc", "-pbkdf2",
                "-in", input.to_str().unwrap(),
                "-out", output.to_str().unwrap(),
                "-pass", &format!("pass:{password}"),
            ])
            .status()?;
        if status.success() {
            return Ok(());
        }
    }
    // XOR fallback
    let mut data = fs::read(input)?;
    let key = password.as_bytes();
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= key[i % key.len()];
    }
    fs::write(output, data)?;
    Ok(())
}

pub fn export(
    output: Option<&Path>,
    encrypt: bool,
    password: Option<&str>,
) -> io::Result<()> {
    // 1. Capture current stack to a temp dir (reuses existing capture logic)
    let tmp_base = env::temp_dir().join(format!("synapse-bundle-{}", std::process::id()));
    let stack_dir = tmp_base.join("stack");
    fs::create_dir_all(&stack_dir)?;

    // Capture stack; if it fails (e.g., OMP not installed), we still continue
    // with whatever we have, but we should NOT silently fall back to a hardcoded path.
    let capture_result = stack::capture(Some(&stack_dir));
    if let Err(e) = capture_result {
        eprintln!("warning: stack capture failed: {e} — bundle will contain only state.json if available");
    }

    // Ensure we have a stack.json. If capture failed and no stack.json exists,
    // return an error rather than silently using a hardcoded path.
    if !stack_dir.join("stack.json").exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no stack.json found — run `synapse stack capture` first or provide a stack directory with --output",
        ));
    }

    // 2. Collect state.json if exists
    let state_src = crate::state::config_dir().join("state.json");
    if state_src.exists() {
        fs::copy(&state_src, tmp_base.join("state.json"))?;
    }

    let bundle_path = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_bundle_path(encrypt));

    if encrypt {
        // Resolve password: use provided, or prompt interactively
        let pass = if let Some(p) = password {
            p.to_string()
        } else {
            eprint!("Enter bundle password: ");
            io::stderr().flush()?;
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            let p = buf.trim().to_string();
            if p.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "password cannot be empty"));
            }
            // Confirm password
            eprint!("Confirm password: ");
            io::stderr().flush()?;
            let mut buf2 = String::new();
            io::stdin().read_line(&mut buf2)?;
            if buf2.trim() != p {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "passwords do not match"));
            }
            p
        };

        // 3. Resolve secrets for the bundle (from current environment)
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(stack_dir.join("stack.json"))?)?;
        let secrets = collect_resolved_secrets(&manifest);
        let _secrets_json = serde_json::to_string_pretty(&secrets).unwrap();
        
        // Write secrets.json with restrictive permissions (0o600)
        let secrets_path = tmp_base.join("secrets.json");
        fs::write(&secrets_path, serde_json::to_string_pretty(&secrets).unwrap())?;
        // Restrict permissions to owner-only
        let mut perms = fs::metadata(&secrets_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&secrets_path, perms)?;

        let env_snapshot: BTreeMap<String, String> = manifest
            .get("omp_profiles")
            .and_then(|v| v.as_array())
            .map(|profiles| {
                let mut m = BTreeMap::new();
                for p in profiles {
                    if let Some(req) = p.get("required_env").and_then(|v| v.as_array()) {
                        for item in req {
                            if let Some(name) = item.as_str() {
                                if let Ok(val) = env::var(name) {
                                    if !val.is_empty() {
                                        m.insert(name.to_string(), val);
                                    }
                                }
                            }
                        }
                    }
                }
                m
            })
            .unwrap_or_default();
        fs::write(
            tmp_base.join("env.json"),
            serde_json::to_string_pretty(&env_snapshot).unwrap(),
        )?;

        // 4. Create tar.zst of the entire bundle directory
        let tar_path = tmp_base.with_extension("tar.zst");
        create_tar_zst(&tmp_base, &tar_path)?;

        // 5. Encrypt the tar.zst with the provided password
        encrypt_file(&tar_path, &bundle_path, &pass)?;
        fs::remove_file(&tar_path).ok();

        println!(
            "Encrypted bundle ({} secrets) → {} ({} profiles, plug-and-play with password)",
            secrets.len(),
            bundle_path.display(),
            manifest
                .get("omp_profiles")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        );
    } else {
        // Non-encrypted bundle: just tar.zst the stack dir (no secrets)
        create_tar_zst(&stack_dir, &bundle_path)?;
        println!(
            "Bundle (no secrets, externalized) → {} — set required_env on restore",
            bundle_path.display()
        );
    }

    fs::remove_dir_all(&tmp_base).ok();
    println!(
        "Run `synapse bundle import --input {} --password <pass>` on new machine",
        bundle_path.display()
    );
    Ok(())
}

pub fn import(input: &Path, password: Option<&str>) -> io::Result<()> {
    if !input.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("bundle not found: {}", input.display()),
        ));
    }

    let is_encrypted = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e == "age")
        .unwrap_or(false)
        || input.to_string_lossy().ends_with(".age");

    let tmp_base = env::temp_dir().join(format!("synapse-import-{}", std::process::id()));
    fs::create_dir_all(&tmp_base)?;

    // Determine if we need to decrypt
    let tar_path = if is_encrypted {
        let pass = if let Some(p) = password {
            p.to_string()
        } else {
            eprint!("Enter bundle password: ");
            io::stderr().flush()?;
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            buf.trim().to_string()
        };
        if pass.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "password required for encrypted bundle",
            ));
        }
        let decrypted = tmp_base.join("bundle.tar.zst");
        decrypt_file(input, &decrypted, &pass)?;
        decrypted
    } else {
        input.to_path_buf()
    };

    extract_tar_zst(&tar_path, &tmp_base)?;

    // Find stack.json in the extracted bundle
    let stack_src = find_stack_dir(&tmp_base)?;

    // If we have secrets.json (encrypted bundle), restore env vars
    let secrets_path = tmp_base.join("secrets.json");
    if secrets_path.exists() {
        let secrets: BTreeMap<String, String> =
            serde_json::from_str(&fs::read_to_string(&secrets_path)?)?;
        println!("Restoring {} secrets from encrypted bundle...", secrets.len());
        for (k, v) in &secrets {
            unsafe { env::set_var(k, v) };
        }
    }

    // Restore stack with explicit trust flag (respect --trust gate)
    // Note: import does not auto-trust; user must pass --trust on CLI if they reviewed the bundle
    stack::restore(Some(&stack_src), None, "root", false, false)?;

    // Also restore state.json if present
    let state_src = tmp_base.join("state.json");
    if state_src.exists() {
        let dest = crate::state::config_dir().join("state.json");
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::copy(&state_src, &dest)?;
        println!("Restored synapse state → {}", dest.display());
    }

    fs::remove_dir_all(&tmp_base).ok();
    println!("Import complete — run `synapse doctor` and `synapse status` to verify");
    Ok(())
}

fn find_stack_dir(base: &Path) -> io::Result<PathBuf> {
    // Check common locations
    let candidates = [
        base.join("stack"),
        base.join("stack.json").parent().unwrap_or(base).to_path_buf(),
    ];
    for c in candidates {
        if c.join("stack.json").exists() {
            return Ok(c);
        }
    }
    // Search recursively (shallow)
    if let Ok(entries) = fs::read_dir(base) {
        for entry in entries.flatten() {
            let p = entry.path().join("stack.json");
            if p.exists() {
                return Ok(entry.path());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "bundle has no stack.json",
    ))
}

fn create_tar_zst(src: &Path, dest: &Path) -> io::Result<()> {
    let status = Command::new("tar")
        .args([
            "-I",
            "zstd -T0",
            "-cf",
            dest.to_str().unwrap(),
            "-C",
            src.parent().unwrap().to_str().unwrap(),
            src.file_name().unwrap().to_str().unwrap(),
        ])
        .status();
    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }
    let gz_dest = dest.with_extension("tar.gz");
    let status = Command::new("tar")
        .args([
            "-czf",
            gz_dest.to_str().unwrap(),
            "-C",
            src.parent().unwrap().to_str().unwrap(),
            src.file_name().unwrap().to_str().unwrap(),
        ])
        .status()
        .map_err(|e| io::Error::other(format!("tar failed: {e}")))?;
    if !status.success() {
        return Err(io::Error::other("tar failed"));
    }
    fs::rename(gz_dest, dest)?;
    Ok(())
}

fn extract_tar_zst(archive: &Path, dest: &Path) -> io::Result<()> {
    let status = Command::new("tar")
        .args([
            "-I",
            "zstd -d",
            "-xf",
            archive.to_str().unwrap(),
            "-C",
            dest.to_str().unwrap(),
        ])
        .status();
    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }
    let status = Command::new("tar")
        .args([
            "-xzf",
            archive.to_str().unwrap(),
            "-C",
            dest.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| io::Error::other(format!("tar extract failed: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("tar extract failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_default_bundle_path() {
        let p = default_bundle_path(true);
        assert!(p.to_string_lossy().ends_with(".tar.zst.age"));
        let p = default_bundle_path(false);
        assert!(p.to_string_lossy().ends_with(".tar.zst"));
    }

    #[test]
    fn test_collect_resolved_secrets() {
        let manifest = serde_json::json!({
            "omp_profiles": [{
                "mcp": {
                    "mcpServers": {
                        "test": {
                            "env": {
                                "TEST_VAR": "!printenv TEST_VAR",
                                "LITERAL": "LITERAL"
                            }
                        }
                    }
                },
                "required_env": ["REQUIRED_VAR"]
            }]
        });
        // Mock env vars
        unsafe { env::set_var("TEST_VAR", "resolved_value"); }
        unsafe { env::set_var("REQUIRED_VAR", "required_value"); }
        let secrets = collect_resolved_secrets(&manifest);
        assert_eq!(secrets.get("TEST_VAR"), Some(&"resolved_value".to_string()));
        assert_eq!(secrets.get("REQUIRED_VAR"), Some(&"required_value".to_string()));
    }

    #[test]
    fn test_encrypt_decrypt_xor() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("input.txt");
        let encrypted = dir.path().join("encrypted.age");
        let decrypted = dir.path().join("decrypted.txt");
        let password = "test_password";

        fs::write(&input, b"hello world").unwrap();
        encrypt_file(&input, &encrypted, password).unwrap();
        decrypt_file(&encrypted, &decrypted, password).unwrap();
        assert_eq!(fs::read(&decrypted).unwrap(), b"hello world");
    }
}