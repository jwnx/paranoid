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

impl FromStr for Architecture {
    type Err = ParseArchitectureError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "armel" => Ok(Architecture::Armel),
            "armhf" | "arm" => Ok(Architecture::Armhf),
            "arm64" | "aarch64" => Ok(Architecture::Arm64),
            "i386" | "i686" => Ok(Architecture::I386),
            "amd64" | "x86_64" => Ok(Architecture::Amd64),
            "mips" => Ok(Architecture::Mips),
            "mipsel" => Ok(Architecture::Mipsel),
            "mips64el" => Ok(Architecture::Mips64el),
            "powerpc" => Ok(Architecture::PowerPC),
            "ppc64el" | "ppc64le" => Ok(Architecture::Ppc64el),
            "s390x" => Ok(Architecture::S390x),
            "riscv64" => Ok(Architecture::Riscv64),
            "all" => Ok(Architecture::All),
            "any" => Ok(Architecture::Any),
            _ => Err(ParseArchitectureError(s.to_string())),
        }
    }
}

impl Architecture {
    /// Returns the OCI-compatible architecture name.
    ///
    /// OCI uses GOARCH-style names which differ from Debian in some cases.
    pub const fn oci_name(&self) -> &'static str {
        match self {
            Architecture::Amd64 => "amd64",
            Architecture::Arm64 => "arm64",
            Architecture::I386 => "386",
            Architecture::Armhf | Architecture::Armel => "arm",
            Architecture::Ppc64el => "ppc64le",
            Architecture::S390x => "s390x",
            Architecture::Riscv64 => "riscv64",
            Architecture::Mips => "mips",
            Architecture::Mipsel => "mipsle",
            Architecture::Mips64el => "mips64le",
            Architecture::PowerPC => "ppc",
            // `All` and `Any` are Debian meta-architectures with no OCI equivalent.
            // Callers must resolve them to a concrete architecture before serializing
            // an OCI image config. Reaching this branch is a programming error.
            Architecture::All | Architecture::Any => {
                panic!("`All`/`Any` architecture has no OCI representation; resolve to a concrete architecture first")
            }
        }
    }
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Architecture::Armel => "armel",
            Architecture::Armhf => "armhf",
            Architecture::Arm64 => "arm64",
            Architecture::I386 => "i386",
            Architecture::Amd64 => "amd64",
            Architecture::Mips => "mips",
            Architecture::Mipsel => "mipsel",
            Architecture::Mips64el => "mips64el",
            Architecture::PowerPC => "powerpc",
            Architecture::Ppc64el => "ppc64el",
            Architecture::S390x => "s390x",
            Architecture::Riscv64 => "riscv64",
            Architecture::All => "all",
            Architecture::Any => "any",
        };
        write!(f, "{}", s)
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
