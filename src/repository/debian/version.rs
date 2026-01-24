//! Debian version parsing and comparison.
//!
//! Implements the Debian version comparison algorithm as specified in:
//! https://www.debian.org/doc/debian-policy/ch-controlfields.html#version
//!
//! Based on the APT implementation:
//! https://github.com/Debian/apt/blob/main/apt-pkg/deb/debversion.cc

use std::cmp::Ordering;
use std::str::FromStr;

use thiserror::Error;

use crate::version::Version;

/// A parsed Debian version.
///
/// Debian versions have the format: `[epoch:]upstream_version[-debian_revision]`
///
/// - `epoch`: Optional integer (defaults to 0). Used when upstream changes versioning schemes.
/// - `upstream_version`: The main version from upstream software.
/// - `debian_revision`: Optional. Debian-specific patches to upstream.
///
/// Examples:
/// - `2.36-9+deb12u4` → epoch=0, upstream="2.36", revision="9+deb12u4"
/// - `1:2.0-1` → epoch=1, upstream="2.0", revision="1"
/// - `3.0~rc1-1` → epoch=0, upstream="3.0~rc1", revision="1"
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct DebianVersion {
    pub epoch: u32,
    pub upstream: String,
    pub revision: Option<String>,
}

/// Error parsing a Debian version string.
#[derive(Debug, Error)]
#[error("invalid version '{invalid_version}': {reason}")]
pub struct ParseDebianVersionError {
    pub invalid_version: String,
    pub reason: String,
}

impl DebianVersion {
    /// Parse a Debian version string.
    fn parse_impl(s: &str) -> Result<Self, ParseDebianVersionError> {
        if s.is_empty() {
            return Err(ParseDebianVersionError {
                invalid_version: s.to_string(),
                reason: "empty version string".to_string(),
            });
        }

        let mut remaining = s;

        // Parse epoch (optional, before ':')
        let epoch = if let Some(colon_idx) = remaining.find(':') {
            let epoch_str = &remaining[..colon_idx];
            let epoch = epoch_str
                .parse::<u32>()
                .map_err(|_| ParseDebianVersionError {
                    invalid_version: s.to_string(),
                    reason: format!("invalid epoch: {}", epoch_str),
                })?;
            remaining = &remaining[colon_idx + 1..];
            epoch
        } else {
            0
        };

        // Parse revision (optional, after last '-')
        // Note: use rfind because upstream can contain '-'
        let (upstream, revision) = if let Some(dash_idx) = remaining.rfind('-') {
            let upstream = &remaining[..dash_idx];
            let revision = &remaining[dash_idx + 1..];
            (upstream.to_string(), Some(revision.to_string()))
        } else {
            (remaining.to_string(), None)
        };

        if upstream.is_empty() {
            return Err(ParseDebianVersionError {
                invalid_version: s.to_string(),
                reason: "empty upstream version".to_string(),
            });
        }

        Ok(DebianVersion {
            epoch,
            upstream,
            revision,
        })
    }

    /// Compare two version parts (upstream or revision).
    ///
    /// The algorithm splits each string into alternating groups of digits and non-digits,
    /// then compares them pairwise.
    ///
    /// Special handling: `~` (tilde) sorts before all other characters, including the
    /// empty string. This enables pre-release versions like "1.0~rc1" to sort before "1.0".
    fn compare_part(a: &str, b: &str) -> Ordering {
        let a_parts = split_version_part(a);
        let b_parts = split_version_part(b);

        let max_len = a_parts.len().max(b_parts.len());

        for i in 0..max_len {
            let a_part = a_parts.get(i).map(|s| s.as_str()).unwrap_or("");
            let b_part = b_parts.get(i).map(|s| s.as_str()).unwrap_or("");

            // Check if both parts are purely numeric
            let a_is_digit = !a_part.is_empty() && a_part.chars().all(|c| c.is_ascii_digit());
            let b_is_digit = !b_part.is_empty() && b_part.chars().all(|c| c.is_ascii_digit());

            let result = if a_is_digit && b_is_digit {
                // Numeric comparison (as integers to handle leading zeros correctly)
                compare_numeric(a_part, b_part)
            } else {
                // Lexicographic comparison with special ordering
                compare_non_digit(a_part, b_part)
            };

            if result != Ordering::Equal {
                return result;
            }
        }

        Ordering::Equal
    }
}

/// Split a version part into alternating groups of digits and non-digits.
///
/// Examples:
/// - "2.36" → ["2", ".", "36"]
/// - "a67bhgs89" → ["a", "67", "bhgs", "89"]
/// - "2.7.2-linux-1" → ["2", ".", "7", ".", "2", "-linux-", "1"]
fn split_version_part(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec![];
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut prev_is_digit: Option<bool> = None;

    for c in s.chars() {
        let is_digit = c.is_ascii_digit();

        if let Some(prev) = prev_is_digit
            && is_digit != prev
        {
            parts.push(current);
            current = String::new();
        }

        current.push(c);
        prev_is_digit = Some(is_digit);
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

/// Compare two numeric strings.
///
/// First tries to parse as u64 for proper numeric comparison.
/// Falls back to length comparison (longer = bigger) then lexicographic
/// for very large numbers that would overflow u64.
fn compare_numeric(a: &str, b: &str) -> Ordering {
    // Strip leading zeros for length comparison
    let a_trimmed = a.trim_start_matches('0');
    let b_trimmed = b.trim_start_matches('0');

    // If both can be parsed as u64, compare numerically
    if let (Ok(a_num), Ok(b_num)) = (a.parse::<u64>(), b.parse::<u64>()) {
        return a_num.cmp(&b_num);
    }

    // Fallback for very large numbers: compare by length first
    match a_trimmed.len().cmp(&b_trimmed.len()) {
        Ordering::Equal => a_trimmed.cmp(b_trimmed),
        other => other,
    }
}

/// Get the sort order for a character in non-digit comparison.
///
/// Special ordering rules:
/// - `~` (tilde) sorts before everything, including empty string
/// - Letters sort in their natural ASCII order
/// - Digits sort after letters (when in non-digit context)
/// - Other characters sort after digits
fn char_order(c: char) -> i32 {
    match c {
        '~' => -1, // Tilde sorts before everything
        c if c.is_ascii_alphabetic() => c as i32,
        c if c.is_ascii_digit() => (c as i32) + 256,
        c => (c as i32) + 512,
    }
}

/// Compare two non-digit strings with Debian's special ordering.
fn compare_non_digit(a: &str, b: &str) -> Ordering {
    let a_chars: Vec<i32> = a.chars().map(char_order).collect();
    let b_chars: Vec<i32> = b.chars().map(char_order).collect();

    let max_len = a_chars.len().max(b_chars.len());

    for i in 0..max_len {
        // Missing characters are treated as 0 (sorts after ~, before everything else)
        let a_ord = a_chars.get(i).copied().unwrap_or(0);
        let b_ord = b_chars.get(i).copied().unwrap_or(0);

        match a_ord.cmp(&b_ord) {
            Ordering::Equal => continue,
            other => return other,
        }
    }

    Ordering::Equal
}

impl FromStr for DebianVersion {
    type Err = ParseDebianVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_impl(s)
    }
}

impl Version for DebianVersion {
    type ParseError = ParseDebianVersionError;
}

impl std::fmt::Display for DebianVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.epoch > 0 {
            write!(f, "{}:", self.epoch)?;
        }
        write!(f, "{}", self.upstream)?;
        if let Some(ref rev) = self.revision {
            write!(f, "-{}", rev)?;
        }
        Ok(())
    }
}

impl PartialOrd for DebianVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DebianVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        // 1. Compare epochs numerically
        match self.epoch.cmp(&other.epoch) {
            Ordering::Equal => {}
            other => return other,
        }

        // 2. Compare upstream versions
        match Self::compare_part(&self.upstream, &other.upstream) {
            Ordering::Equal => {}
            other => return other,
        }

        // 3. Compare debian revisions (missing revision treated as "0")
        let self_rev = self.revision.as_deref().unwrap_or("0");
        let other_rev = other.revision.as_deref().unwrap_or("0");
        Self::compare_part(self_rev, other_rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_version() {
        let v = DebianVersion::from_str("1.0").unwrap();
        assert_eq!(v.epoch, 0);
        assert_eq!(v.upstream, "1.0");
        assert_eq!(v.revision, None);
    }

    #[test]
    fn test_parse_version_with_revision() {
        let v = DebianVersion::from_str("2.36-9+deb12u4").unwrap();
        assert_eq!(v.epoch, 0);
        assert_eq!(v.upstream, "2.36");
        assert_eq!(v.revision, Some("9+deb12u4".to_string()));
    }

    #[test]
    fn test_parse_version_with_epoch() {
        let v = DebianVersion::from_str("1:2.0-1").unwrap();
        assert_eq!(v.epoch, 1);
        assert_eq!(v.upstream, "2.0");
        assert_eq!(v.revision, Some("1".to_string()));
    }

    #[test]
    fn test_parse_version_with_tilde() {
        let v = DebianVersion::from_str("3.0~rc1-1").unwrap();
        assert_eq!(v.epoch, 0);
        assert_eq!(v.upstream, "3.0~rc1");
        assert_eq!(v.revision, Some("1".to_string()));
    }

    #[test]
    fn test_parse_complex_upstream() {
        // Upstream can contain dashes, only last dash separates revision
        let v = DebianVersion::from_str("1.0-beta-2-1").unwrap();
        assert_eq!(v.upstream, "1.0-beta-2");
        assert_eq!(v.revision, Some("1".to_string()));
    }

    #[test]
    fn test_parse_invalid_empty() {
        assert!(DebianVersion::from_str("").is_err());
    }

    #[test]
    fn test_parse_invalid_epoch() {
        assert!(DebianVersion::from_str("abc:1.0").is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(DebianVersion::from_str("1.0").unwrap().to_string(), "1.0");
        assert_eq!(
            DebianVersion::from_str("2.0-1").unwrap().to_string(),
            "2.0-1"
        );
        assert_eq!(
            DebianVersion::from_str("1:3.0-2").unwrap().to_string(),
            "1:3.0-2"
        );
    }

    // Comparison tests

    #[test]
    fn test_compare_equal() {
        let a = DebianVersion::from_str("1.0").unwrap();
        let b = DebianVersion::from_str("1.0").unwrap();
        assert_eq!(a.cmp(&b), Ordering::Equal);
    }

    #[test]
    fn test_compare_simple_versions() {
        let v1 = DebianVersion::from_str("1.0").unwrap();
        let v2 = DebianVersion::from_str("2.0").unwrap();
        assert_eq!(v1.cmp(&v2), Ordering::Less);
        assert_eq!(v2.cmp(&v1), Ordering::Greater);
    }

    #[test]
    fn test_compare_numeric_components() {
        let v9 = DebianVersion::from_str("1.9").unwrap();
        let v10 = DebianVersion::from_str("1.10").unwrap();
        // 10 > 9 numerically (not lexicographically!)
        assert_eq!(v9.cmp(&v10), Ordering::Less);
    }

    #[test]
    fn test_compare_epochs() {
        let v0 = DebianVersion::from_str("2.0").unwrap();
        let v1 = DebianVersion::from_str("1:1.0").unwrap();
        // Epoch 1 > epoch 0, regardless of upstream version
        assert_eq!(v0.cmp(&v1), Ordering::Less);
    }

    #[test]
    fn test_compare_revisions() {
        let v1 = DebianVersion::from_str("1.0-1").unwrap();
        let v2 = DebianVersion::from_str("1.0-2").unwrap();
        assert_eq!(v1.cmp(&v2), Ordering::Less);
    }

    #[test]
    fn test_compare_missing_revision() {
        // Missing revision treated as "0"
        let v_no_rev = DebianVersion::from_str("1.0").unwrap();
        let v_rev_1 = DebianVersion::from_str("1.0-1").unwrap();
        assert_eq!(v_no_rev.cmp(&v_rev_1), Ordering::Less);
    }

    #[test]
    fn test_tilde_sorts_before_release() {
        // This is the key Debian behavior: ~rc sorts BEFORE the release
        let rc = DebianVersion::from_str("1.0~rc1").unwrap();
        let release = DebianVersion::from_str("1.0").unwrap();
        assert_eq!(rc.cmp(&release), Ordering::Less);
    }

    #[test]
    fn test_tilde_ordering() {
        let alpha = DebianVersion::from_str("1.0~alpha").unwrap();
        let beta = DebianVersion::from_str("1.0~beta").unwrap();
        let rc1 = DebianVersion::from_str("1.0~rc1").unwrap();
        let release = DebianVersion::from_str("1.0").unwrap();

        assert_eq!(alpha.cmp(&beta), Ordering::Less);
        assert_eq!(beta.cmp(&rc1), Ordering::Less);
        assert_eq!(rc1.cmp(&release), Ordering::Less);
    }

    #[test]
    fn test_complex_version_comparison() {
        // Real-world example from libc6
        let v1 = DebianVersion::from_str("2.36-9").unwrap();
        let v2 = DebianVersion::from_str("2.36-9+deb12u4").unwrap();
        // "9" < "9+deb12u4" because '+' > end of string
        assert_eq!(v1.cmp(&v2), Ordering::Less);
    }

    #[test]
    fn test_split_version_part() {
        assert_eq!(split_version_part("2.36"), vec!["2", ".", "36"]);
        assert_eq!(
            split_version_part("a67bhgs89"),
            vec!["a", "67", "bhgs", "89"]
        );
        assert_eq!(split_version_part(""), Vec::<String>::new());
    }

    #[test]
    fn test_ord_enables_sorting() {
        let mut versions: Vec<DebianVersion> =
            vec!["2.36-9+deb12u4", "1.0", "2.36-9", "1:1.0", "1.0~rc1"]
                .into_iter()
                .map(|s| DebianVersion::from_str(s).unwrap())
                .collect();

        versions.sort();

        let sorted: Vec<String> = versions.iter().map(|v| v.to_string()).collect();
        assert_eq!(
            sorted,
            vec!["1.0~rc1", "1.0", "2.36-9", "2.36-9+deb12u4", "1:1.0"]
        );
    }

    // Test integration with generic VersionConstraint
    mod constraint_tests {
        use super::*;
        use crate::version::{VersionConstraint, VersionRelation};

        #[test]
        fn test_version_constraint_strictly_earlier() {
            let constraint = VersionConstraint::new(
                VersionRelation::StrictlyEarlier,
                DebianVersion::from_str("2.0").unwrap(),
            );

            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("1.0").unwrap()));
            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("1.9").unwrap()));
            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.0").unwrap()));
            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("3.0").unwrap()));
        }

        #[test]
        fn test_version_constraint_later_or_equal() {
            let constraint = VersionConstraint::new(
                VersionRelation::LaterOrEqual,
                DebianVersion::from_str("2.17").unwrap(),
            );

            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.16").unwrap()));
            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.17").unwrap()));
            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.36").unwrap()));
        }

        #[test]
        fn test_version_constraint_equal() {
            let constraint = VersionConstraint::new(
                VersionRelation::Equal,
                DebianVersion::from_str("1:2.0-1").unwrap(),
            );

            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("1:2.0-1").unwrap()));
            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("1:2.0-2").unwrap()));
            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.0-1").unwrap()));
        }

        #[test]
        fn test_version_constraint_with_tilde() {
            // libc6 (>= 2.17) should accept 2.36 but not 2.17~rc1
            let constraint = VersionConstraint::new(
                VersionRelation::LaterOrEqual,
                DebianVersion::from_str("2.17").unwrap(),
            );

            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.36").unwrap()));
            assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.17").unwrap()));
            assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.17~rc1").unwrap()));
        }
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for generating valid Debian version strings.
        fn version_strategy() -> impl Strategy<Value = String> {
            // Generate versions like "1.2.3-4" with optional epoch
            let upstream = "[0-9]+\\.[0-9]+(\\.[0-9]+)?";
            let revision = "[0-9]+([a-z0-9+.~]*)?";

            prop_oneof![
                // Simple version: "1.2.3"
                upstream.prop_map(|s| s),
                // Version with revision: "1.2.3-1"
                (upstream, revision).prop_map(|(u, r)| format!("{}-{}", u, r)),
                // Version with epoch: "1:1.2.3-1"
                (1u32..10, upstream, revision).prop_map(|(e, u, r)| format!("{}:{}-{}", e, u, r)),
            ]
        }

        proptest! {
            /// Parsing then displaying a version should produce an equivalent version.
            #[test]
            fn parse_display_roundtrip(version in version_strategy()) {
                if let Ok(parsed) = DebianVersion::from_str(&version) {
                    let displayed = parsed.to_string();
                    let reparsed = DebianVersion::from_str(&displayed).unwrap();
                    prop_assert_eq!(parsed, reparsed);
                }
            }

            /// Version comparison is reflexive: v == v
            #[test]
            fn comparison_reflexive(version in version_strategy()) {
                if let Ok(v) = DebianVersion::from_str(&version) {
                    prop_assert_eq!(v.cmp(&v), Ordering::Equal);
                }
            }

            /// Version comparison is antisymmetric: if a < b then b > a
            #[test]
            fn comparison_antisymmetric(
                v1 in version_strategy(),
                v2 in version_strategy()
            ) {
                if let (Ok(a), Ok(b)) = (DebianVersion::from_str(&v1), DebianVersion::from_str(&v2)) {
                    match a.cmp(&b) {
                        Ordering::Less => prop_assert_eq!(b.cmp(&a), Ordering::Greater),
                        Ordering::Greater => prop_assert_eq!(b.cmp(&a), Ordering::Less),
                        Ordering::Equal => prop_assert_eq!(b.cmp(&a), Ordering::Equal),
                    }
                }
            }

            /// Version comparison is transitive: if a <= b and b <= c then a <= c
            #[test]
            fn comparison_transitive(
                v1 in version_strategy(),
                v2 in version_strategy(),
                v3 in version_strategy()
            ) {
                if let (Ok(a), Ok(b), Ok(c)) = (
                    DebianVersion::from_str(&v1),
                    DebianVersion::from_str(&v2),
                    DebianVersion::from_str(&v3)
                ) {
                    if a <= b && b <= c {
                        prop_assert!(a <= c);
                    }
                }
            }

            /// Tilde versions always sort before their non-tilde counterparts.
            #[test]
            fn tilde_sorts_before(base in "[0-9]+\\.[0-9]+", suffix in "[a-z0-9]+") {
                let tilde_version = format!("{}~{}", base, suffix);
                if let (Ok(tilde), Ok(release)) = (
                    DebianVersion::from_str(&tilde_version),
                    DebianVersion::from_str(&base)
                ) {
                    prop_assert!(tilde < release, "{} should be < {}", tilde_version, base);
                }
            }

            /// Higher epochs always beat lower epochs, regardless of upstream version.
            #[test]
            fn epoch_dominates(
                low_epoch in 0u32..5,
                high_epoch in 5u32..10,
                low_upstream in "[0-9]+\\.[0-9]+",
                high_upstream in "[0-9]+\\.[0-9]+"
            ) {
                // Even if low_upstream > high_upstream lexically, high epoch wins
                let low_version = format!("{}:{}", low_epoch, high_upstream);
                let high_version = format!("{}:{}", high_epoch, low_upstream);

                if let (Ok(low), Ok(high)) = (
                    DebianVersion::from_str(&low_version),
                    DebianVersion::from_str(&high_version)
                ) {
                    prop_assert!(low < high, "{} should be < {}", low_version, high_version);
                }
            }
        }
    }
}
