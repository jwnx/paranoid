//! Virtual package provides entries.
//!
//! Handles the `Provides` field in Debian packages.

use std::str::FromStr;

use thiserror::Error;

use super::version::DebianVersion;

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

/// Error parsing a provides entry.
#[derive(Debug, Error)]
#[error("invalid provides entry '{invalid_entry}': {reason}")]
pub struct ParseProvidesEntryError {
    pub invalid_entry: String,
    pub reason: String,
}

impl FromStr for ProvidesEntry {
    type Err = ParseProvidesEntryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseProvidesEntryError {
                invalid_entry: s.to_string(),
                reason: "empty provides entry".to_string(),
            });
        }

        if let Some(paren_start) = s.find('(') {
            let name = s[..paren_start].trim().to_string();
            if name.is_empty() {
                return Err(ParseProvidesEntryError {
                    invalid_entry: s.to_string(),
                    reason: "empty package name".to_string(),
                });
            }

            let rest = &s[paren_start + 1..];

            if let Some(paren_end) = rest.find(')') {
                let constraint = &rest[..paren_end];
                // Skip the operator (should be "=") and get just the version
                let version = constraint
                    .split_whitespace()
                    .last()
                    .and_then(|v| DebianVersion::from_str(v).ok());

                return Ok(ProvidesEntry { name, version });
            }
        }

        Ok(ProvidesEntry {
            name: s.to_string(),
            version: None,
        })
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

/// A comma-separated list of virtual packages provided.
///
/// Parses strings like "awk, libssl (= 3.0.0), libcrypto"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidesList(pub Vec<ProvidesEntry>);

impl std::ops::Deref for ProvidesList {
    type Target = Vec<ProvidesEntry>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<ProvidesList> for Vec<ProvidesEntry> {
    fn from(list: ProvidesList) -> Self {
        list.0
    }
}

/// Error parsing a provides list.
#[derive(Debug, Error)]
#[error("invalid provides entry at index {index} ({entry}): {source}")]
pub struct ParseProvidesListError {
    pub index: usize,
    pub entry: String,
    #[source]
    pub source: ParseProvidesEntryError,
}

impl FromStr for ProvidesList {
    type Err = ParseProvidesListError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Ok(ProvidesList(Vec::new()));
        }

        let mut entries = Vec::new();
        for (i, part) in s.split(',').enumerate() {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match ProvidesEntry::from_str(part) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    return Err(ParseProvidesListError {
                        index: i,
                        entry: part.to_string(),
                        source: e,
                    });
                }
            }
        }
        Ok(ProvidesList(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_provides_entry_empty_error() {
        assert!(ProvidesEntry::from_str("").is_err());
        assert!(ProvidesEntry::from_str("   ").is_err());
    }

    #[test]
    fn test_parse_provides_list() {
        let list = ProvidesList::from_str("awk, libssl (= 3.0)").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "awk");
        assert!(list[0].version.is_none());
        assert_eq!(list[1].name, "libssl");
        assert_eq!(
            list[1].version,
            Some(DebianVersion::from_str("3.0").unwrap())
        );
    }

    #[test]
    fn test_parse_provides_list_empty() {
        let list = ProvidesList::from_str("").unwrap();
        assert!(list.is_empty());
    }
}
