/// Supported operating systems
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OS {
    Mac,
    Linux,
    Windows,
}

/// Supported CPU architectures
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

/// Detect current OS at compile time
pub fn detect_os() -> OS {
    if cfg!(target_os = "macos") {
        OS::Mac
    } else if cfg!(target_os = "linux") {
        OS::Linux
    } else if cfg!(target_os = "windows") {
        OS::Windows
    } else {
        panic!("Unsupported OS")
    }
}

/// Detect current architecture at compile time
pub fn detect_arch() -> Arch {
    if cfg!(target_arch = "x86_64") {
        Arch::X86_64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Aarch64
    } else {
        panic!("Unsupported architecture")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_os() {
        let os = detect_os();
        // ponytail: just verify it returns one of the supported variants
        assert!(matches!(os, OS::Mac | OS::Linux | OS::Windows));
    }

    #[test]
    fn test_detect_arch() {
        let arch = detect_arch();
        assert!(matches!(arch, Arch::X86_64 | Arch::Aarch64));
    }

    #[test]
    fn test_current_platform() {
        // Verify current compilation target matches runtime detection
        let os = detect_os();
        let arch = detect_arch();

        #[cfg(target_os = "macos")]
        assert_eq!(os, OS::Mac);

        #[cfg(target_os = "linux")]
        assert_eq!(os, OS::Linux);

        #[cfg(target_os = "windows")]
        assert_eq!(os, OS::Windows);

        #[cfg(target_arch = "x86_64")]
        assert_eq!(arch, Arch::X86_64);

        #[cfg(target_arch = "aarch64")]
        assert_eq!(arch, Arch::Aarch64);
    }
}
