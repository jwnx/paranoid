//! Common architecture types.
//!
//! CPU architecture definitions used across different package formats.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// CPU architecture.
///
/// These are common architectures supported by Linux distributions.
/// The naming follows Debian conventions but applies broadly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    Armel,
    Armhf,
    Arm64,
    I386,
    Amd64,
    Mips,
    Mipsel,
    Mips64el,
    #[serde(rename = "powerpc")]
    PowerPC,
    Ppc64el,
    S390x,
    Riscv64,
    /// Architecture-independent packages (scripts, documentation, etc.)
    All,
    /// Wildcard matching any architecture (used in dependencies)
    Any,
}

/// Error parsing an architecture string.
#[derive(Debug, Error)]
#[error("unknown architecture: {0}")]
pub struct ParseArchitectureError(pub String);

/// An OCI platform's CPU fields (`architecture` and optional `variant`).
///
/// OCI uses GOARCH-style names, which differ from Debian's for several
/// architectures, and distinguishes ARM ABIs with a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OciPlatform {
    pub architecture: &'static str,
    pub variant: Option<&'static str>,
}

/// One row of the architecture table.
struct ArchInfo {
    arch: Architecture,
    /// Canonical Debian name, used for `Display`.
    debian: &'static str,
    /// Other accepted spellings when parsing.
    aliases: &'static [&'static str],
    /// OCI platform, or `None` for Debian meta-architectures.
    oci: Option<OciPlatform>,
}

const fn oci(architecture: &'static str, variant: Option<&'static str>) -> Option<OciPlatform> {
    Some(OciPlatform {
        architecture,
        variant,
    })
}

/// Every architecture we know about, in one place. Parsing, display and the
/// OCI mapping in both directions are all driven from this table.
#[rustfmt::skip]
const ARCHITECTURES: &[ArchInfo] = &[
    ArchInfo { arch: Architecture::Armel,    debian: "armel",    aliases: &[],               oci: oci("arm", Some("v5")) },
    ArchInfo { arch: Architecture::Armhf,    debian: "armhf",    aliases: &["arm"],          oci: oci("arm", Some("v7")) },
    ArchInfo { arch: Architecture::Arm64,    debian: "arm64",    aliases: &["aarch64"],      oci: oci("arm64", None) },
    ArchInfo { arch: Architecture::I386,     debian: "i386",     aliases: &["i686"],         oci: oci("386", None) },
    ArchInfo { arch: Architecture::Amd64,    debian: "amd64",    aliases: &["x86_64"],       oci: oci("amd64", None) },
    ArchInfo { arch: Architecture::Mips,     debian: "mips",     aliases: &[],               oci: oci("mips", None) },
    ArchInfo { arch: Architecture::Mipsel,   debian: "mipsel",   aliases: &[],               oci: oci("mipsle", None) },
    ArchInfo { arch: Architecture::Mips64el, debian: "mips64el", aliases: &[],               oci: oci("mips64le", None) },
    ArchInfo { arch: Architecture::PowerPC,  debian: "powerpc",  aliases: &[],               oci: oci("ppc", None) },
    ArchInfo { arch: Architecture::Ppc64el,  debian: "ppc64el",  aliases: &["ppc64le"],      oci: oci("ppc64le", None) },
    ArchInfo { arch: Architecture::S390x,    debian: "s390x",    aliases: &[],               oci: oci("s390x", None) },
    ArchInfo { arch: Architecture::Riscv64,  debian: "riscv64",  aliases: &[],               oci: oci("riscv64", None) },
    ArchInfo { arch: Architecture::All,      debian: "all",      aliases: &[],               oci: None },
    ArchInfo { arch: Architecture::Any,      debian: "any",      aliases: &[],               oci: None },
];

impl Architecture {
    fn info(self) -> &'static ArchInfo {
        ARCHITECTURES
            .iter()
            .find(|info| info.arch == self)
            .expect("every Architecture variant has a row in ARCHITECTURES")
    }

    /// The canonical Debian name.
    pub fn as_str(self) -> &'static str {
        self.info().debian
    }

    /// Whether this is a real CPU architecture rather than one of Debian's
    /// `all`/`any` meta-architectures.
    pub fn is_concrete(self) -> bool {
        self.info().oci.is_some()
    }

    /// The OCI platform for this architecture, or `None` for `all`/`any`,
    /// which have no OCI equivalent.
    pub fn oci_platform(self) -> Option<OciPlatform> {
        self.info().oci
    }

    /// Map an OCI platform back to an architecture.
    ///
    /// A missing variant on `arm` is read as `armhf`, the common case.
    pub fn from_oci(architecture: &str, variant: Option<&str>) -> Option<Self> {
        let matches = |info: &&ArchInfo| {
            info.oci
                .is_some_and(|p| p.architecture == architecture && p.variant == variant)
        };
        ARCHITECTURES
            .iter()
            .find(matches)
            .or_else(|| match (architecture, variant) {
                ("arm", None) => Some(Architecture::Armhf.info()),
                _ => None,
            })
            .map(|info| info.arch)
    }
}

impl FromStr for Architecture {
    type Err = ParseArchitectureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ARCHITECTURES
            .iter()
            .find(|info| info.debian == s || info.aliases.contains(&s))
            .map(|info| info.arch)
            .ok_or_else(|| ParseArchitectureError(s.to_string()))
    }
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_architecture() {
        assert_eq!(
            Architecture::from_str("amd64").unwrap(),
            Architecture::Amd64
        );
        assert_eq!(
            Architecture::from_str("x86_64").unwrap(),
            Architecture::Amd64
        );
        assert_eq!(
            Architecture::from_str("arm64").unwrap(),
            Architecture::Arm64
        );
        assert_eq!(
            Architecture::from_str("aarch64").unwrap(),
            Architecture::Arm64
        );
        assert_eq!(Architecture::from_str("all").unwrap(), Architecture::All);
    }

    #[test]
    fn test_display_architecture() {
        assert_eq!(Architecture::Amd64.to_string(), "amd64");
        assert_eq!(Architecture::Arm64.to_string(), "arm64");
        assert_eq!(Architecture::All.to_string(), "all");
    }

    #[test]
    fn test_every_variant_has_a_table_row() {
        for info in ARCHITECTURES {
            assert_eq!(info.arch.info().debian, info.debian);
        }
    }

    #[test]
    fn test_oci_platform_roundtrips() {
        for info in ARCHITECTURES {
            if let Some(p) = info.arch.oci_platform() {
                assert_eq!(
                    Architecture::from_oci(p.architecture, p.variant),
                    Some(info.arch),
                    "{} does not round-trip through OCI",
                    info.debian
                );
            }
        }
    }

    #[test]
    fn test_meta_architectures_have_no_oci_platform() {
        assert!(Architecture::All.oci_platform().is_none());
        assert!(Architecture::Any.oci_platform().is_none());
        assert!(!Architecture::All.is_concrete());
        assert!(Architecture::Amd64.is_concrete());
    }

    #[test]
    fn test_from_oci_arm_without_variant_is_armhf() {
        assert_eq!(
            Architecture::from_oci("arm", None),
            Some(Architecture::Armhf)
        );
        assert_eq!(
            Architecture::from_oci("386", None),
            Some(Architecture::I386)
        );
        assert_eq!(Architecture::from_oci("sparc", None), None);
    }

    #[test]
    fn test_parse_unknown_architecture() {
        assert!(Architecture::from_str("unknown").is_err());
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        /// All known architecture variants.
        const KNOWN_ARCHS: &[&str] = &[
            "armel", "armhf", "arm64", "aarch64", "i386", "i686", "amd64", "x86_64", "mips",
            "mipsel", "mips64el", "powerpc", "ppc64el", "ppc64le", "s390x", "riscv64", "all",
            "any",
        ];

        proptest! {
            /// Parsing then displaying a known architecture should roundtrip.
            #[test]
            fn parse_display_roundtrip(idx in 0..KNOWN_ARCHS.len()) {
                let arch_str = KNOWN_ARCHS[idx];
                let parsed = Architecture::from_str(arch_str).unwrap();
                let displayed = parsed.to_string();
                let reparsed = Architecture::from_str(&displayed).unwrap();
                prop_assert_eq!(parsed, reparsed);
            }

            /// Random strings that don't match known architectures should fail.
            #[test]
            fn unknown_arch_fails(s in "[a-z]{1,10}") {
                // Filter out known architectures
                if !KNOWN_ARCHS.contains(&s.as_str()) {
                    prop_assert!(Architecture::from_str(&s).is_err());
                }
            }
        }
    }
}
