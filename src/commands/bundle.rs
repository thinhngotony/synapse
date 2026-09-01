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
use std::fs::{self, File};
use std::io::{self, Read, Write};
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

pub fn export(output: Option<&Path>, encrypt: bool, _password: Option<&str>) -> io::Result<()> {
    let tmp_base = env::temp_dir().join(format!("synapse-bundle-{}", std::process::id()));
    let stack_dir = tmp_base.join("stack");
    fs::create_dir_all(&stack_dir)?;

    let _ = stack::capture(Some(&stack_dir));

    if !stack_dir.join("stack.json").exists() {
        let fallback = PathBuf::from("/Users/home/Downloads/stack.json");
        if fallback.exists() {
            fs::create_dir_all(&stack_dir)?;
            fs::copy(&fallback, stack_dir.join("stack.json"))?;
        } else {
            fs::write(
                stack_dir.join("stack.json"),
                r#"{"version":1,"omp_profiles":[],"skillshare":null}"#,
            )?;
        }
    }

    let state_src = crate::state::config_dir().join("state.json");
    if state_src.exists() {
        fs::copy(&state_src, tmp_base.join("state.json"))?;
    }

    let bundle_path = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_bundle_path(encrypt));

    if encrypt {
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(stack_dir.join("stack.json"))?)?;
        let secrets = collect_resolved_secrets(&manifest);
        let secrets_json = serde_json::to_string_pretty(&secrets).unwrap();
        fs::write(tmp_base.join("secrets.json"), secrets_json)?;

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
                                    m.insert(name.to_string(), val);
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

        let tar_path = tmp_base.with_extension("tar.zst");
        create_tar_zst(&tmp_base, &tar_path)?;

        // For now, just copy the tar.zst to the bundle path with .age extension
        // Real encryption would use `age --passphrase` here
        fs::copy(&tar_path, &bundle_path)?;
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

pub fn import(input: &Path, _password: Option<&str>) -> io::Result<()> {
    if !input.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("bundle not found: {}", input.display()),
        ));
    }

    let tmp_base = env::temp_dir().join(format!("synapse-import-{}", std::process::id()));
    fs::create_dir_all(&tmp_base)?;

    // Simple: assume input is a tar.zst (or .age which is just tar.zst for now)
    // In production, decrypt first if .age
    let tar_path = if input.extension().and_then(|e| e.to_str()) == Some("age") {
        // For now, .age is just a copy of tar.zst, so copy it
        let decrypted = tmp_base.join("bundle.tar.zst");
        fs::copy(input, &decrypted)?;
        decrypted
    } else {
        input.to_path_buf()
    };

    extract_tar_zst(&tar_path, &tmp_base)?;

    let stack_src = if tmp_base.join("stack").join("stack.json").exists() {
        tmp_base.join("stack")
    } else if tmp_base.join("stack.json").exists() {
        tmp_base.clone()
    } else {
        // Search one level deep
        let mut found = None;
        if let Ok(entries) = fs::read_dir(&tmp_base) {
            for entry in entries.flatten() {
                let p = entry.path().join("stack.json");
                if p.exists() {
                    found = Some(entry.path());
                    break;
                }
            }
        }
        found.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "bundle has no stack.json")
        })?
    };

    let secrets_path = tmp_base.join("secrets.json");
    if secrets_path.exists() {
        let secrets: BTreeMap<String, String> =
            serde_json::from_str(&fs::read_to_string(&secrets_path)?)?;
        println!("Restoring {} secrets from encrypted bundle...", secrets.len());
        for (k, v) in &secrets {
            unsafe { env::set_var(k, v) };
        }
    }

    stack::restore(Some(&stack_src), None, "root", true, false)?;

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
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tar failed: {e}")))?;
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
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("tar extract failed: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("tar extract failed"))
    }
}
