//! Captures build-time metadata for `synapse version`.
//!
//! The git SHA is resolved at compile time; when the source is built outside a
//! git checkout (release tarball, Nix build with a clean source) it degrades to
//! "unknown" rather than failing the build.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=SYNAPSE_GIT_SHA={sha}");

    // Rebuild when HEAD moves so the embedded SHA does not go stale.
    println!("cargo:rerun-if-changed=.git/HEAD");
}
