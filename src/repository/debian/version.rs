//! Debian version parsing and comparison.
//!
//! A port of dpkg's `parseversion()` and `verrevcmp()` (`lib/dpkg/parsehelp.c`
//! and `lib/dpkg/version.c`), so that versions order exactly the way dpkg and
//! apt order them. See also
//! <https://www.debian.org/doc/debian-policy/ch-controlfields.html#version>.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use thiserror::Error;

use crate::version::Version;

/// A parsed Debian version: `[epoch:]upstream_version[-debian_revision]`.
///
/// - `epoch` defaults to 0.
/// - `upstream_version` runs up to the last `-`, so it may contain hyphens.
/// - `debian_revision` is empty when there is no `-`.
///
/// Equality and hashing follow the ordering, as in dpkg: `1.0`, `1.0-0` and
/// `1.00` are the same version.
///
/// ```
/// use paranoid::repository::debian::DebianVersion;
///
/// let a: DebianVersion = "1.0~rc1-1".parse().unwrap();
/// let b: DebianVersion = "1.0-1".parse().unwrap();
/// assert!(a < b);
/// assert_eq!("1.0".parse::<DebianVersion>().unwrap(), "1.0-0".parse().unwrap());
/// ```
#[derive(Debug, Clone, Default)]
pub struct DebianVersion {
    epoch: u32,
    upstream: String,
    revision: String,
}

/// Error parsing a Debian version string.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid version '{version}': {reason}")]
pub struct ParseDebianVersionError {
    pub version: String,
    pub reason: &'static str,
}

impl DebianVersion {
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn upstream(&self) -> &str {
        &self.upstream
    }

    /// The Debian revision, or `""` if the version has none.
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl FromStr for DebianVersion {
    type Err = ParseDebianVersionError;

    /// Parse like dpkg's lax parser: syntax errors are rejected, but versions
    /// dpkg only warns about (an upstream version that doesn't start with a
    /// digit, unusual characters) are accepted, as dpkg and apt accept them
    /// when reading archive indexes.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParseDebianVersionError {
            version: s.to_string(),
            reason,
        };

        let trimmed = s.trim_matches([' ', '\t']);
        if trimmed.is_empty() {
            return Err(err("version string is empty"));
        }
        if trimmed.contains([' ', '\t']) {
            return Err(err("version string has embedded spaces"));
        }

        let (epoch, rest) = match trimmed.split_once(':') {
            Some((epoch, rest)) => {
                if epoch.is_empty() {
                    return Err(err("epoch in version is empty"));
                }
                if !epoch.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(err("epoch in version is not number"));
                }
                let epoch = epoch
                    .parse::<i32>()
                    .map_err(|_| err("epoch in version is too big"))?;
                if rest.is_empty() {
                    return Err(err("nothing after colon in version number"));
                }
                (epoch as u32, rest)
            }
            None => (0, trimmed),
        };

        let (upstream, revision) = match rest.rsplit_once('-') {
            Some((_, "")) => return Err(err("revision number is empty")),
            Some((upstream, revision)) => (upstream, revision),
            None => (rest, ""),
        };
        if upstream.is_empty() {
            return Err(err("version number is empty"));
        }

        Ok(Self {
            epoch,
            upstream: upstream.to_string(),
            revision: revision.to_string(),
        })
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
        f.write_str(&self.upstream)?;
        if !self.revision.is_empty() {
            write!(f, "-{}", self.revision)?;
        }
        Ok(())
    }
}

impl Ord for DebianVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| verrevcmp(&self.upstream, &other.upstream))
            .then_with(|| verrevcmp(&self.revision, &other.revision))
    }
}

impl PartialOrd for DebianVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for DebianVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DebianVersion {}

impl Hash for DebianVersion {
    /// Hash the canonical form, so versions that compare equal hash equal.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        for part in [&self.upstream, &self.revision] {
            canonical_chunks(part).hash(state);
        }
    }
}

/// dpkg's sort weight for a character in a non-digit run: `~` sorts before
/// everything, even the end of the string; letters sort before other
/// characters. The end of the string and digits weigh 0.
fn order(c: Option<u8>) -> i32 {
    match c {
        None => 0,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(c),
        Some(b'~') => -1,
        Some(c) => i32::from(c) + 256,
    }
}

/// Compare two upstream versions or two revisions, as dpkg's `verrevcmp`.
///
/// Both strings are consumed in alternating runs: a non-digit run compared
/// character by character with [`order`], then a digit run compared as a
/// number. A missing run is empty, and an empty digit run is zero.
fn verrevcmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    let at = |s: &[u8], k: usize| s.get(k).copied();
    let is_digit = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_digit());
    let is_non_digit = |c: Option<u8>| c.is_some_and(|c| !c.is_ascii_digit());

    while i < a.len() || j < b.len() {
        while is_non_digit(at(a, i)) || is_non_digit(at(b, j)) {
            let (ac, bc) = (order(at(a, i)), order(at(b, j)));
            if ac != bc {
                return ac.cmp(&bc);
            }
            i += 1;
            j += 1;
        }

        while at(a, i) == Some(b'0') {
            i += 1;
        }
        while at(b, j) == Some(b'0') {
            j += 1;
        }

        let mut first_diff = Ordering::Equal;
        while is_digit(at(a, i)) && is_digit(at(b, j)) {
            first_diff = first_diff.then(a[i].cmp(&b[j]));
            i += 1;
            j += 1;
        }
        if is_digit(at(a, i)) {
            return Ordering::Greater;
        }
        if is_digit(at(b, j)) {
            return Ordering::Less;
        }
        if first_diff != Ordering::Equal {
            return first_diff;
        }
    }

    Ordering::Equal
}

/// The parts of a version that [`verrevcmp`] looks at: alternating
/// (non-digit run, digit run without leading zeros) pairs, with trailing
/// empty pairs dropped. Two strings compare equal exactly when these match.
fn canonical_chunks(s: &str) -> Vec<(&[u8], &[u8])> {
    let s = s.as_bytes();
    let mut chunks = Vec::new();
    let mut k = 0;
    while k < s.len() {
        let start = k;
        while k < s.len() && !s[k].is_ascii_digit() {
            k += 1;
        }
        let non_digit = &s[start..k];
        while k < s.len() && s[k] == b'0' {
            k += 1;
        }
        let start = k;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        chunks.push((non_digit, &s[start..k]));
    }
    while chunks
        .last()
        .is_some_and(|(n, d)| n.is_empty() && d.is_empty())
    {
        chunks.pop();
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> DebianVersion {
        s.parse().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_parts() {
        for (input, epoch, upstream, revision) in [
            ("1.0", 0, "1.0", ""),
            ("2.36-9+deb12u4", 0, "2.36", "9+deb12u4"),
            ("1:2.0-1", 1, "2.0", "1"),
            ("3.0~rc1-1", 0, "3.0~rc1", "1"),
            // Only the last hyphen starts the revision, only the first colon
            // ends the epoch.
            ("1.0-beta-2-1", 0, "1.0-beta-2", "1"),
            ("0:0:0-0", 0, "0:0", "0"),
            ("0:0-0:0-0", 0, "0-0:0", "0"),
            ("0:09azAZ.-+~:-0", 0, "09azAZ.-+~:", "0"),
            ("0:0-azAZ09.+~", 0, "0", "azAZ09.+~"),
            ("  \t0:0-1\t  ", 0, "0", "1"),
            // dpkg only warns about these, and accepts them in indexes.
            ("0:abc3-0", 0, "abc3", "0"),
        ] {
            let parsed = v(input);
            assert_eq!(
                (parsed.epoch(), parsed.upstream(), parsed.revision()),
                (epoch, upstream, revision),
                "{input:?}"
            );
        }
    }

    #[test]
    fn rejects_what_dpkg_rejects() {
        for (input, reason) in [
            ("", "version string is empty"),
            ("  ", "version string is empty"),
            ("0:", "nothing after colon in version number"),
            (":1.0", "epoch in version is empty"),
            ("1.0-", "revision number is empty"),
            ("0:0 0-1", "version string has embedded spaces"),
            ("-1:0-1", "epoch in version is not number"),
            (
                "999999999999999999999999:0-1",
                "epoch in version is too big",
            ),
            ("a:0-0", "epoch in version is not number"),
            ("A:0-0", "epoch in version is not number"),
            ("-0", "version number is empty"),
            ("0:-0", "version number is empty"),
        ] {
            assert_eq!(
                input.parse::<DebianVersion>().map_err(|e| e.reason),
                Err(reason),
                "{input:?}"
            );
        }
    }

    #[test]
    fn displays_like_input() {
        for s in ["1.0", "2.0-1", "1:3.0-2", "1.0-beta-2-1"] {
            assert_eq!(v(s).to_string(), s);
        }
        // Epoch 0 is implicit.
        assert_eq!(v("0:1.0").to_string(), "1.0");
    }

    /// Pairs in strictly increasing order. `scripts/dpkg-oracle.sh` copies
    /// these and [`EQUAL`] into the dpkg test data, so dpkg checks them too.
    const ORDERED: &[(&str, &str)] = &[
        ("1.0", "2.0"),
        ("1.9", "1.10"),
        ("2.0", "1:1.0"),
        ("1.0-1", "1.0-2"),
        ("1.0", "1.0-1"),
        ("1.0~rc1", "1.0"),
        ("1.0~alpha", "1.0~beta"),
        ("1.0~beta", "1.0~rc1"),
        ("1.0~~", "1.0~~a"),
        ("1.0~~a", "1.0~"),
        ("1.0~", "1.0"),
        ("1.0", "1.0a"),
        ("1.0", "1.0+b1"),
        ("1.0", "1.0.1"),
        ("1.0a", "1.0+"),
        ("1.0a", "1.0."),
        ("1.0+", "1.0.1"),
        ("2.36-9", "2.36-9+deb12u4"),
        ("2.36-9+deb12u9", "2.36-9+deb12u10"),
        ("1.0-1", "1.0-a1"),
        ("1.0", "a1"),
        ("1.2.3", "1.2.3a"),
        ("1.18.1-3", "1.18.1-3+deb12u1"),
        ("7.88.1-10+deb12u5", "7.88.1-10+deb12u12"),
        ("0:0-0", "0:0-a"),
        ("0:a-0", "0:b-0"),
        ("0:0-a", "0:0-b"),
        ("0:0-0", "1:0-0"),
        ("1.0", "1.0-0.1"),
        ("3.0.18-1~deb12u1", "3.0.20-1~deb12u2"),
    ];

    /// Pairs that compare equal even though they're spelled differently.
    const EQUAL: &[(&str, &str)] = &[
        ("1.0", "1.0-0"),
        ("1.0", "1.00"),
        ("1.01", "1.1"),
        ("1.0", "1."),
        ("0:0-00", "0:00-0"),
        ("0:1.0", "1.0"),
        ("1.0-0", "1.0-00"),
    ];

    #[test]
    fn orders_like_dpkg() {
        for &(lo, hi) in ORDERED {
            assert_eq!(v(lo).cmp(&v(hi)), Ordering::Less, "{lo} < {hi}");
            assert_eq!(v(hi).cmp(&v(lo)), Ordering::Greater, "{hi} > {lo}");
        }
    }

    #[test]
    fn equal_versions_are_eq_and_hash_alike() {
        use std::collections::HashSet;

        for &(a, b) in EQUAL {
            assert_eq!(v(a).cmp(&v(b)), Ordering::Equal, "{a} = {b}");
            assert_eq!(v(a), v(b), "{a} == {b}");
            let set: HashSet<_> = [v(a), v(b)].into();
            assert_eq!(set.len(), 1, "{a} and {b} hash differently");
        }
    }

    /// Every pair in the test data, ordered the way dpkg 1.21 ordered it.
    #[test]
    fn agrees_with_dpkg() {
        let data = include_str!("testdata/dpkg-compare-versions.txt");
        let mut checked = 0;
        for line in data.lines().filter(|l| !l.starts_with('#')) {
            let [a, b, expected] = line.split(' ').collect::<Vec<_>>()[..] else {
                panic!("bad line {line:?}");
            };
            let expected = match expected {
                "lt" => Ordering::Less,
                "eq" => Ordering::Equal,
                "gt" => Ordering::Greater,
                other => panic!("dpkg answered {other:?} for {a} {b}"),
            };
            assert_eq!(v(a).cmp(&v(b)), expected, "{a} vs {b}");
            checked += 1;
        }
        assert!(checked > 3000, "only {checked} pairs");
    }

    #[test]
    fn verrevcmp_handles_non_digit_starts() {
        // Strings dpkg would only warn about still need a total order.
        assert_eq!(verrevcmp("a", "a0"), Ordering::Equal);
        assert_eq!(verrevcmp("a", "b"), Ordering::Less);
        assert_eq!(verrevcmp("~", ""), Ordering::Less);
        assert_eq!(verrevcmp("", "a"), Ordering::Less);
        assert_eq!(verrevcmp("", "0"), Ordering::Equal);
    }

    #[test]
    fn sorts() {
        let mut versions: Vec<_> = ["2.36-9+deb12u4", "1.0", "2.36-9", "1:1.0", "1.0~rc1"]
            .map(v)
            .to_vec();
        versions.sort();
        let sorted: Vec<_> = versions.iter().map(ToString::to_string).collect();
        assert_eq!(
            sorted,
            ["1.0~rc1", "1.0", "2.36-9", "2.36-9+deb12u4", "1:1.0"]
        );
    }

    mod constraint_tests {
        use super::*;
        use crate::version::{VersionConstraint, VersionRelation};

        fn satisfies(relation: VersionRelation, bound: &str, candidate: &str) -> bool {
            VersionConstraint::new(relation, v(bound)).is_satisfied_by(&v(candidate))
        }

        #[test]
        fn relations() {
            use VersionRelation::*;

            assert!(satisfies(StrictlyEarlier, "2.0", "1.9"));
            assert!(!satisfies(StrictlyEarlier, "2.0", "2.0"));
            assert!(satisfies(LaterOrEqual, "2.17", "2.17"));
            assert!(satisfies(LaterOrEqual, "2.17", "2.36"));
            assert!(!satisfies(LaterOrEqual, "2.17", "2.17~rc1"));
            assert!(satisfies(Equal, "1:2.0-1", "1:2.0-1"));
            assert!(!satisfies(Equal, "1:2.0-1", "2.0-1"));
            // Equal versions satisfy `=` however they're spelled.
            assert!(satisfies(Equal, "1.0", "1.0-0"));
        }
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;
        use std::collections::hash_map::DefaultHasher;

        /// Any string of version characters, including ones that start with a
        /// letter or `~`, and runs of zeros.
        fn part() -> impl Strategy<Value = String> {
            "[0-9a-c.+~]{0,6}"
        }

        fn version() -> impl Strategy<Value = DebianVersion> {
            (0u32..3, "[0-9][0-9a-c.+~]{0,5}", part()).prop_map(|(epoch, upstream, revision)| {
                DebianVersion {
                    epoch,
                    upstream,
                    revision,
                }
            })
        }

        fn hash(v: &DebianVersion) -> u64 {
            let mut h = DefaultHasher::new();
            v.hash(&mut h);
            h.finish()
        }

        proptest! {
            #[test]
            fn display_parse_roundtrip(v in version()) {
                prop_assert_eq!(v.to_string().parse::<DebianVersion>().unwrap(), v);
            }

            #[test]
            fn verrevcmp_is_antisymmetric(a in part(), b in part()) {
                prop_assert_eq!(verrevcmp(&a, &b), verrevcmp(&b, &a).reverse());
            }

            #[test]
            fn verrevcmp_is_transitive(a in part(), b in part(), c in part()) {
                if verrevcmp(&a, &b).is_le() && verrevcmp(&b, &c).is_le() {
                    prop_assert!(verrevcmp(&a, &c).is_le());
                }
            }

            /// `Eq` and `Hash` must agree with `Ord`, or maps keyed by version
            /// lose entries.
            #[test]
            fn eq_and_hash_follow_ord(a in version(), b in version()) {
                let equal = a.cmp(&b) == Ordering::Equal;
                prop_assert_eq!(a == b, equal);
                if equal {
                    prop_assert_eq!(hash(&a), hash(&b));
                }
            }

            /// The canonical form decides equality exactly.
            #[test]
            fn canonical_chunks_match_verrevcmp(a in part(), b in part()) {
                prop_assert_eq!(
                    canonical_chunks(&a) == canonical_chunks(&b),
                    verrevcmp(&a, &b) == Ordering::Equal
                );
            }

            #[test]
            fn tilde_sorts_before(base in "[0-9]+\\.[0-9]+", suffix in "[a-z0-9]+") {
                let tilde = v(&format!("{base}~{suffix}"));
                prop_assert!(tilde < v(&base));
            }

            #[test]
            fn epoch_dominates(low in "[0-9]+\\.[0-9]+", high in "[0-9]+\\.[0-9]+") {
                let (low, high) = (v(&format!("1:{low}")), v(&format!("0:{high}")));
                prop_assert!(low > high);
            }
        }
    }
}
