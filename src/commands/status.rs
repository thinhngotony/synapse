use std::io;

use crate::nix;
use crate::platform;
use crate::state;

/// `synapse status` — platform, Nix, installed packages, update availability.
pub fn run() -> io::Result<()> {
    let os = platform::detect_os();
    let arch = platform::detect_arch();
    let os_str = match os {
        platform::OS::Mac => "macOS",
        platform::OS::Linux => "Linux",
        platform::OS::Windows => "Windows",
    };
    let arch_str = match arch {
        platform::Arch::X86_64 => "x86_64",
        platform::Arch::Aarch64 => "aarch64",
    };
    println!("Platform:  {os_str}/{arch_str}");

    let nix_status = nix::detect();
    match &nix_status {
        nix::NixStatus::Supported(v) => println!("Nix:       {v} ✓"),
        nix::NixStatus::TooOld(v) => println!("Nix:       {v} (too old — need 2.24+)"),
        nix::NixStatus::Missing => println!("Nix:       not found"),
    }
    if let Some(advice) = nix::advice(&nix_status) {
        eprintln!("\n{advice}");
    }

    let cfg = state::config_dir();
    let st = state::read(&cfg)?;
    if st.packages.is_empty() {
        println!("Packages:  none installed");
    } else {
        println!("Packages:");
        let mut pkgs: Vec<_> = st.packages.iter().collect();
        pkgs.sort_by_key(|(k, _)| *k);
        for (name, rec) in &pkgs {
            println!("  {:<14} {}", name, rec.version);
        }
    }

    // ponytail: update availability requires network + nix flake metadata;
    // stub until SYN-6 implements update checking
    println!("Updates:   unknown (run `synapse update --all` to check)");

    Ok(())
}
