//! Virtual package provides entries.
//!
//! Handles the `Provides` field in Debian packages.

use std::str::FromStr;

use super::dependency::{DependencySpec, ParseDependencySpecError};
use super::version::DebianVersion;
use crate::version::VersionRelation;

/// A virtual package provided by a package.
///
/// From the Provides field. Debian policy only allows exact version matches.
///
/// Examples:
/// - `awk` (no version)
/// - `libssl (= 3.0.0)` (exact version)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidesEntry {
    pub name: String,
    pub version: Option<DebianVersion>,
}

impl FromStr for ProvidesEntry {
    type Err = ParseDependencySpecError;

    /// Parse with the same grammar as `Depends`, then require `=`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let DependencySpec { name, version } = s.parse()?;
        let version = match version {
            None => None,
            Some(c) if c.relation == VersionRelation::Equal => Some(c.version),
            Some(c) => return Err(ParseDependencySpecError::ProvidesRelation(c.relation)),
        };
        Ok(ProvidesEntry { name, version })
    }
}

impl std::fmt::Display for ProvidesEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(ref ver) = self.version {
            write!(f, " (= {})", ver)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::dependency::parse_relations;
    use super::*;

    #[test]
    fn parses_entries() {
        let entries: Vec<ProvidesEntry> = parse_relations("awk, libssl (= 3.0), mta(=1)").unwrap();
        let parsed: Vec<_> = entries.iter().map(ToString::to_string).collect();
        assert_eq!(parsed, ["awk", "libssl (= 3.0)", "mta (= 1)"]);
        assert!(parse_relations::<ProvidesEntry>("").unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_entries() {
        assert_eq!(
            ProvidesEntry::from_str(""),
            Err(ParseDependencySpecError::Empty)
        );
        assert_eq!(
            ProvidesEntry::from_str("libssl (>= 3.0)"),
            Err(ParseDependencySpecError::ProvidesRelation(
                VersionRelation::LaterOrEqual
            ))
        );
        // Used to be read as an unversioned `libssl`.
        assert!(ProvidesEntry::from_str("libssl (= )").is_err());
    }
}
