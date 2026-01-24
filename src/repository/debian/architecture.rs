//! Debian-specific architecture types.
//!
//! Re-exports the common Architecture type and adds Debian-specific
//! concepts like System (kernel type) and ArchitectureRestriction.

use std::str::FromStr;

use thiserror::Error;

// Re-export the common Architecture type
pub use crate::arch::{Architecture, ParseArchitectureError};

/// Operating system kernel type (Debian-specific).
///
/// Ref: https://wiki.debian.org/SupportedArchitectures
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    Hurd,
    Linux,
    KFreeBSD,
    NetBSD,
    Any,
}

/// Error parsing a system string.
#[derive(Debug, Error)]
#[error("unknown system: {0}")]
pub struct ParseSystemError(pub String);

impl FromStr for System {
    type Err = ParseSystemError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "hurd" => Ok(System::Hurd),
            "linux" => Ok(System::Linux),
            "kfreebsd" => Ok(System::KFreeBSD),
            "netbsd" => Ok(System::NetBSD),
            "any" => Ok(System::Any),
            _ => Err(ParseSystemError(s.to_string())),
        }
    }
}

/// Architecture restriction for a dependency (Debian-specific).
///
/// Used in dependency specifications to limit which architectures
/// a dependency applies to.
///
/// Examples:
/// - `Include(Any, Amd64)` from `[amd64]`
/// - `Exclude(Hurd, I386)` from `[!hurd-i386]`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchitectureRestriction {
    Include(System, Architecture),
    Exclude(System, Architecture),
}

/// Error parsing an architecture restriction.
#[derive(Debug, Error)]
pub enum ParseArchitectureRestrictionError {
    #[error("invalid system: {0}")]
    System(#[from] ParseSystemError),
    #[error("invalid architecture: {0}")]
    Architecture(#[from] ParseArchitectureError),
}

impl FromStr for ArchitectureRestriction {
    type Err = ParseArchitectureRestrictionError;

    fn from_str(sa: &str) -> Result<Self, Self::Err> {
        let is_exclude = sa.starts_with('!');
        let sa = sa.trim_start_matches('!');

        let (s, a) = sa.split_once('-').unwrap_or(("any", sa));

        let s = System::from_str(s)?;
        let a = Architecture::from_str(a)?;

        if is_exclude {
            Ok(ArchitectureRestriction::Exclude(s, a))
        } else {
            Ok(ArchitectureRestriction::Include(s, a))
        }
    }
}
