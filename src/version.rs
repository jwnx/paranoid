//! Generic version comparison traits.
//!
//! Different package formats (Debian, Alpine, RPM) have different version schemes.
//! This module provides a generic abstraction for version comparison.

use std::cmp::Ordering;
use std::str::FromStr;

use thiserror::Error;

/// Version comparison abstraction.
///
/// Implementors must provide parsing and comparison logic specific to their
/// version scheme. Comparison is done via the standard `Ord` trait.
///
/// Implements `FromStr` for parsing version strings.
pub trait Version: Sized + Clone + PartialEq + Eq + Ord + FromStr<Err = Self::ParseError> {
    /// The error type returned when parsing fails.
    type ParseError: std::error::Error;
}

/// Version relation operators used in dependency constraints.
///
/// These correspond to the operators used in Debian's dependency syntax,
/// but are generic enough for other package formats.
// https://www.debian.org/doc/debian-policy/ch-relationships.html
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    /// `<<` - Strictly earlier than
    StrictlyEarlier,
    /// `<=` - Earlier than or equal to
    EarlierOrEqual,
    /// `=` - Exactly equal to
    Equal,
    /// `>=` - Later than or equal to
    LaterOrEqual,
    /// `>>` - Strictly later than
    StrictlyLater,
}

/// Error parsing a version relation.
#[derive(Debug, Error)]
#[error("unknown version relation: {unknown_relation}")]
pub struct ParseVersionRelationError {
    pub unknown_relation: String,
}

/// Error parsing a version constraint.
#[derive(Debug, Error)]
pub enum ParseVersionConstraintError<E: std::error::Error> {
    #[error("invalid version: {0}")]
    InvalidVersion(#[source] E),
}

impl FromStr for VersionRelation {
    type Err = ParseVersionRelationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "<<" => Ok(VersionRelation::StrictlyEarlier),
            "<=" => Ok(VersionRelation::EarlierOrEqual),
            "=" => Ok(VersionRelation::Equal),
            ">=" => Ok(VersionRelation::LaterOrEqual),
            ">>" => Ok(VersionRelation::StrictlyLater),
            _ => Err(ParseVersionRelationError {
                unknown_relation: s.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for VersionRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionRelation::StrictlyEarlier => write!(f, "<<"),
            VersionRelation::EarlierOrEqual => write!(f, "<="),
            VersionRelation::Equal => write!(f, "="),
            VersionRelation::LaterOrEqual => write!(f, ">="),
            VersionRelation::StrictlyLater => write!(f, ">>"),
        }
    }
}

/// A version constraint combining a relation and a version.
///
/// Example: `>= 1.0` means "version 1.0 or later"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionConstraint<V: Version> {
    pub relation: VersionRelation,
    pub version: V,
}

impl<V: Version> VersionConstraint<V> {
    pub fn new(relation: VersionRelation, version: V) -> Self {
        Self { relation, version }
    }

    /// Parse a constraint string like ">= 2.17" or "<< 3.0".
    ///
    /// Returns `None` if the string cannot be parsed.
    /// For error details, use `str::parse()` instead.
    #[must_use]
    pub fn try_parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    /// Check if a candidate version satisfies this constraint.
    #[must_use]
    pub fn is_satisfied_by(&self, candidate: &V) -> bool {
        let ord = Ord::cmp(candidate, &self.version);
        match self.relation {
            VersionRelation::StrictlyEarlier => ord == Ordering::Less,
            VersionRelation::EarlierOrEqual => ord != Ordering::Greater,
            VersionRelation::Equal => ord == Ordering::Equal,
            VersionRelation::LaterOrEqual => ord != Ordering::Less,
            VersionRelation::StrictlyLater => ord == Ordering::Greater,
        }
    }
}

impl<V: Version> FromStr for VersionConstraint<V> {
    type Err = ParseVersionConstraintError<V::ParseError>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        let (relation, version_str) = if let Some(rest) = s.strip_prefix("<<") {
            (VersionRelation::StrictlyEarlier, rest)
        } else if let Some(rest) = s.strip_prefix("<=") {
            (VersionRelation::EarlierOrEqual, rest)
        } else if let Some(rest) = s.strip_prefix(">=") {
            (VersionRelation::LaterOrEqual, rest)
        } else if let Some(rest) = s.strip_prefix(">>") {
            (VersionRelation::StrictlyLater, rest)
        } else if let Some(rest) = s.strip_prefix('=') {
            (VersionRelation::Equal, rest)
        } else {
            // No relation prefix, treat as exact match
            (VersionRelation::Equal, s)
        };

        let version = version_str
            .trim()
            .parse()
            .map_err(ParseVersionConstraintError::InvalidVersion)?;

        Ok(Self { relation, version })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_relation() {
        assert_eq!(
            VersionRelation::from_str("<<").unwrap(),
            VersionRelation::StrictlyEarlier
        );
        assert_eq!(
            VersionRelation::from_str("<=").unwrap(),
            VersionRelation::EarlierOrEqual
        );
        assert_eq!(
            VersionRelation::from_str("=").unwrap(),
            VersionRelation::Equal
        );
        assert_eq!(
            VersionRelation::from_str(">=").unwrap(),
            VersionRelation::LaterOrEqual
        );
        assert_eq!(
            VersionRelation::from_str(">>").unwrap(),
            VersionRelation::StrictlyLater
        );
    }

    #[test]
    fn test_parse_invalid_relation() {
        assert!(VersionRelation::from_str("<").is_err());
        assert!(VersionRelation::from_str(">").is_err());
        assert!(VersionRelation::from_str("==").is_err());
    }
}
