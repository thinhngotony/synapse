use std::io;

use crate::platform;

/// Git SHA captured at build time by build.rs; "unknown" outside a checkout.
pub const GIT_SHA: &str = env!("SYNAPSE_GIT_SHA");

/// `synapse version` — Synapse version plus build info.
pub fn run() -> io::Result<()> {
    let os = match platform::detect_os() {
        platform::OS::Mac => "macos",
        platform::OS::Linux => "linux",
        platform::OS::Windows => "windows",
    };
    let arch = match platform::detect_arch() {
        platform::Arch::X86_64 => "x86_64",
        platform::Arch::Aarch64 => "aarch64",
    };

    println!("synapse {}", env!("CARGO_PKG_VERSION"));
    println!("commit:  {GIT_SHA}");
    println!("target:  {arch}-{os}");
    println!("rustc:   {}", env!("CARGO_PKG_RUST_VERSION"));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sha_is_populated() {
        // Either a real short SHA or the explicit fallback — never empty.
        assert!(!GIT_SHA.is_empty());
        if GIT_SHA != "unknown" {
            assert!(
                GIT_SHA.chars().all(|c| c.is_ascii_hexdigit()),
                "expected a hex SHA, got {GIT_SHA:?}"
            );
        }
    }

    #[test]
    fn version_matches_cargo_manifest() {
        assert_eq!(env!("CARGO_PKG_VERSION"), "1.0.0");
    }
}
