//! Detection of the Nix runtime that Synapse installs packages through.

use std::fmt;
use std::process::Command;

/// Minimum Nix version Synapse supports.
///
/// 2.24 is the floor because Synapse relies on `nix build`/`nix flake` being
/// available without `--extra-experimental-features` on a default install.
pub const MIN_VERSION: Version = Version {
    major: 2,
    minor: 24,
};

/// A Nix `major.minor` version. Patch level is parsed but not retained: no
/// supported behaviour differs at patch granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// What Synapse found when it looked for a usable Nix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixStatus {
    /// Nix is present and new enough to use.
    Supported(Version),
    /// Nix is present but older than [`MIN_VERSION`].
    TooOld(Version),
    /// No `nix` executable on PATH.
    Missing,
}

/// Parse the Nix version out of the output of `nix --version`.
///
/// Handles the shapes Nix and its distributions emit:
/// - `nix (Nix) 2.35.1`
/// - `nix (Nix) 2.24.0pre20240101_abcdef` (nixpkgs unstable builds)
/// - `nix (Determinate Nix 3.21.9) 2.34.8`
///
/// The last form matters: Determinate ships its own product version inside the
/// parentheses, and that number is *larger* than the Nix version it wraps. Taking
/// the first version-like token would read `3.21.9` and happily accept a Nix
/// older than our floor. Tokens inside parentheses are therefore skipped, and the
/// Nix version is the final bare version token.
pub fn parse_version(output: &str) -> Option<Version> {
    let mut depth = 0i32;
    let mut candidate: Option<&str> = None;

    for token in output.split_whitespace() {
        // Track parenthesis nesting across tokens; a token may open and close.
        let opens = token.matches('(').count() as i32;
        let closes = token.matches(')').count() as i32;
        let inside = depth > 0 || opens > 0;
        depth += opens - closes;

        if inside {
            continue;
        }

        if token.starts_with(|c: char| c.is_ascii_digit()) && token.contains('.') {
            // Keep the last match: the Nix version trails any product banner.
            candidate = Some(token);
        }
    }

    let token = candidate?;
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;

    // Trailing pre-release/suffix data is not part of the minor number.
    let minor_digits: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if minor_digits.is_empty() {
        return None;
    }

    Some(Version {
        major,
        minor: minor_digits.parse().ok()?,
    })
}

/// Classify a parsed version against the supported floor.
pub fn classify(version: Option<Version>) -> NixStatus {
    match version {
        Some(v) if v >= MIN_VERSION => NixStatus::Supported(v),
        Some(v) => NixStatus::TooOld(v),
        None => NixStatus::Missing,
    }
}

/// Absolute paths where Nix installs its client, in probe order.
///
/// Resolution cannot rely on `PATH` alone: launchd agents and cron jobs run
/// with a minimal environment that does not include the Nix profile, so an
/// auto-update triggered by the scheduler would fail with ENOENT while the
/// same command works in an interactive shell.
const NIX_CANDIDATES: &[&str] = &[
    "/nix/var/nix/profiles/default/bin/nix",
    "/run/current-system/sw/bin/nix",
    "/usr/local/bin/nix",
];

/// Absolute path to a runnable `nix`, or `None` if there is none.
///
/// Prefers `PATH` (respects a user's chosen install), then falls back to the
/// standard absolute locations so scheduler-invoked runs behave identically.
pub fn resolve_bin() -> Option<String> {
    if Command::new("nix")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return Some("nix".to_string());
    }

    NIX_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).is_file())
        .map(|p| (*p).to_string())
}

/// Look for a usable Nix, on `PATH` or in a standard install location.
///
/// A `nix` that exists but cannot be executed, or whose output cannot be
/// parsed, is reported as [`NixStatus::Missing`] — from the caller's point of
/// view there is no Nix it can drive either way.
pub fn detect() -> NixStatus {
    let Some(bin) = resolve_bin() else {
        return NixStatus::Missing;
    };
    let output = match Command::new(&bin).arg("--version").output() {
        Ok(o) if o.status.success() => o,
        _ => return NixStatus::Missing,
    };

    classify(parse_version(&String::from_utf8_lossy(&output.stdout)))
}

/// Operator-facing guidance for a given status.
///
/// Synapse never installs Nix itself: on macOS the official installer creates
/// an APFS volume and edits `/etc/synthetic.conf`, which is not a change to
/// make on a user's behalf without consent.
pub fn advice(status: &NixStatus) -> Option<String> {
    match status {
        NixStatus::Supported(_) => None,
        NixStatus::TooOld(found) => Some(format!(
            "Nix {found} is too old; Synapse needs {MIN_VERSION} or newer.\n\
             Upgrade with:  sudo -i nix upgrade-nix"
        )),
        NixStatus::Missing => Some(format!(
            "Nix {MIN_VERSION}+ is required but was not found on PATH.\n\
             Install it with the Determinate Systems installer:\n\
             \n    curl -fsSL https://install.determinate.systems/nix | sh -s -- install\n\
             \n\
             This creates a dedicated volume on macOS and modifies system files,\n\
             so run it yourself rather than having Synapse do it for you."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_output() {
        // The exact string emitted by nix 2.35.1.
        assert_eq!(
            parse_version("nix (Nix) 2.35.1\n"),
            Some(Version {
                major: 2,
                minor: 35
            })
        );
    }

    #[test]
    fn parses_prerelease_suffix() {
        assert_eq!(
            parse_version("nix (Nix) 2.24.0pre20240101_abcdef"),
            Some(Version {
                major: 2,
                minor: 24
            })
        );
    }

    /// Determinate Nix reports its own product version in the parentheses, and
    /// that number is *higher* than the Nix version it wraps. Reading the first
    /// version token would report 3.21 and mask an out-of-date Nix.
    #[test]
    fn parses_determinate_nix_wrapper() {
        assert_eq!(
            parse_version("nix (Determinate Nix 3.21.9) 2.34.8\n"),
            Some(Version {
                major: 2,
                minor: 34
            }),
            "must report the Nix version, not Determinate's product version"
        );
    }

    /// The reason the above matters: a Determinate release could wrap a Nix older
    /// than our floor. Parsing the wrapper version would wrongly accept it.
    #[test]
    fn determinate_wrapper_does_not_mask_too_old_nix() {
        let parsed = parse_version("nix (Determinate Nix 3.21.9) 2.18.1");
        assert_eq!(
            parsed,
            Some(Version {
                major: 2,
                minor: 18
            })
        );
        assert_eq!(
            classify(parsed),
            NixStatus::TooOld(Version {
                major: 2,
                minor: 18
            }),
            "a too-old Nix inside a new Determinate release must still be rejected"
        );
    }

    #[test]
    fn parses_bare_version() {
        assert_eq!(
            parse_version("2.28.4"),
            Some(Version {
                major: 2,
                minor: 28
            })
        );
    }

    #[test]
    fn rejects_unparseable_output() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version("nix (Nix)"), None);
        // Present but malformed: a major with no usable minor.
        assert_eq!(parse_version("nix (Nix) 2.x"), None);
    }

    #[test]
    fn version_ordering_is_numeric_not_lexical() {
        // "2.9" > "2.24" lexically, so a string compare would get this wrong.
        let older = Version { major: 2, minor: 9 };
        let newer = Version {
            major: 2,
            minor: 24,
        };
        assert!(older < newer);
    }

    #[test]
    fn classifies_against_floor() {
        assert_eq!(
            classify(Some(MIN_VERSION)),
            NixStatus::Supported(MIN_VERSION),
            "the floor itself must be accepted"
        );
        assert_eq!(
            classify(Some(Version {
                major: 2,
                minor: 35
            })),
            NixStatus::Supported(Version {
                major: 2,
                minor: 35
            })
        );
        assert_eq!(
            classify(Some(Version {
                major: 2,
                minor: 23
            })),
            NixStatus::TooOld(Version {
                major: 2,
                minor: 23
            }),
            "one minor below the floor must be rejected"
        );
        assert_eq!(classify(None), NixStatus::Missing);
    }

    #[test]
    fn advice_only_for_unusable_nix() {
        assert!(advice(&NixStatus::Supported(MIN_VERSION)).is_none());

        let too_old = advice(&NixStatus::TooOld(Version { major: 2, minor: 4 })).unwrap();
        assert!(too_old.contains("upgrade-nix"), "should say how to upgrade");

        let missing = advice(&NixStatus::Missing).unwrap();
        assert!(
            missing.contains("install.determinate.systems"),
            "should say how to install"
        );
    }
}
