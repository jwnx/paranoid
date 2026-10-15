//! `Release` files: what a repository snapshot contains and how to check it.
//!
//! The signed `Release` (inside `InRelease`) lists the SHA-256 and size of
//! every index in the snapshot. Checking each `Packages` file against that
//! table is what extends the signature to the packages themselves.

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::deb822::parse_stanza;
use super::package_index_set::DEFAULT_PIN_PRIORITY;
use crate::hash::Sha256;

/// Error parsing a `Release` file.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseReleaseError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("invalid {field} field '{value}'")]
    InvalidField { field: &'static str, value: String },

    #[error("invalid SHA256 entry '{0}'")]
    InvalidEntry(String),
}

/// A `Release` file that doesn't describe what was asked for.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReleaseError {
    #[error(
        "expected distribution '{expected}', but the Release file is for suite '{suite}', codename '{codename}'"
    )]
    WrongDistribution {
        expected: String,
        suite: String,
        codename: String,
    },

    #[error("the Release file expired at {0}")]
    Expired(DateTime<Utc>),

    #[error("the Release file expired at {until}, before the snapshot at {at}")]
    ExpiredAtSnapshot {
        until: DateTime<Utc>,
        at: DateTime<Utc>,
    },

    #[error("the Release file is dated {date}, after the snapshot at {at}")]
    AfterSnapshot {
        date: DateTime<Utc>,
        at: DateTime<Utc>,
    },

    #[error("the Release file lists no Packages index for {component}/binary-{architecture}")]
    MissingIndex {
        component: String,
        architecture: String,
    },
}

/// One file listed in a `Release` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexFile {
    pub sha256: Sha256,
    pub size: u64,
}

/// A parsed `Release` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub suite: Option<String>,
    pub codename: Option<String>,
    pub date: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    /// `NotAutomatic: yes`: apt installs from here only when asked to.
    pub not_automatic: bool,
    /// `ButAutomaticUpgrades: yes`: but upgrades what came from here.
    pub but_automatic_upgrades: bool,
    /// Indexes may be fetched by their hash, which keeps a mirror update
    /// between fetching `InRelease` and the indexes from mixing snapshots.
    pub acquire_by_hash: bool,
    /// The `SHA256` table: path under `dists/<dist>/` → checksum and size.
    pub files: BTreeMap<String, IndexFile>,
}

/// Compressions of `Packages` we can read, in order of preference.
pub const PACKAGES_VARIANTS: &[&str] = &["Packages.xz", "Packages.gz", "Packages"];

impl Release {
    /// Check that this is the distribution we asked for and that it's
    /// still valid at `now`, or, for a repository pinned to a snapshot,
    /// that it was valid at the snapshot's time.
    pub fn check(
        &self,
        distribution: &str,
        now: DateTime<Utc>,
        snapshot: Option<DateTime<Utc>>,
    ) -> Result<(), ReleaseError> {
        // A validly signed Release for another distribution (an older one,
        // say) is a downgrade, not a mirror quirk.
        let names = [&self.suite, &self.codename];
        if !names.iter().any(|n| n.as_deref() == Some(distribution)) {
            let or_none = |n: &Option<String>| n.clone().unwrap_or_else(|| "(none)".to_string());
            return Err(ReleaseError::WrongDistribution {
                expected: distribution.to_string(),
                suite: or_none(&self.suite),
                codename: or_none(&self.codename),
            });
        }
        // Valid-Until is what stops a mirror from serving a stale, but
        // validly signed, snapshot forever. A snapshot is stale on purpose,
        // so it has to have been fresh at its own time instead, and can't
        // hold a Release from after it.
        match (snapshot, self.valid_until) {
            (Some(at), _) if self.date > at => Err(ReleaseError::AfterSnapshot {
                date: self.date,
                at,
            }),
            (Some(at), Some(until)) if until <= at => {
                Err(ReleaseError::ExpiredAtSnapshot { until, at })
            }
            (None, Some(until)) if until <= now => Err(ReleaseError::Expired(until)),
            _ => Ok(()),
        }
    }

    /// The pin priority apt gives this repository unless told otherwise:
    /// 1 for `NotAutomatic` (experimental), 100 if it also has
    /// `ButAutomaticUpgrades` (backports), 500 for everything else.
    pub fn default_pin_priority(&self) -> i32 {
        match (self.not_automatic, self.but_automatic_upgrades) {
            (true, false) => 1,
            (true, true) => 100,
            (false, _) => DEFAULT_PIN_PRIORITY,
        }
    }

    /// The `Packages` index to fetch for a component and architecture: its
    /// path under `dists/<dist>/` and what it must hash to.
    pub fn packages_index(
        &self,
        component: &str,
        architecture: &str,
    ) -> Result<(&str, IndexFile), ReleaseError> {
        PACKAGES_VARIANTS
            .iter()
            .find_map(|variant| {
                let path = format!("{component}/binary-{architecture}/{variant}");
                self.files
                    .get_key_value(&path)
                    .map(|(p, f)| (p.as_str(), *f))
            })
            .ok_or_else(|| ReleaseError::MissingIndex {
                component: component.to_string(),
                architecture: architecture.to_string(),
            })
    }
}

/// Parse a `Release` date, like `Sat, 11 Jul 2026 10:16:37 UTC`.
///
/// It is RFC 2822 except that Debian writes `UTC` for the zone, which
/// RFC 2822 doesn't allow.
fn parse_date(field: &'static str, value: &str) -> Result<DateTime<Utc>, ParseReleaseError> {
    let normalized = match value.strip_suffix(" UTC") {
        Some(rest) => format!("{rest} +0000"),
        None => value.to_string(),
    };
    DateTime::parse_from_rfc2822(&normalized)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| ParseReleaseError::InvalidField {
            field,
            value: value.to_string(),
        })
}

/// Parse the `SHA256` table: one ` <sha256> <size> <path>` per line.
fn parse_files(table: &str) -> Result<BTreeMap<String, IndexFile>, ParseReleaseError> {
    table
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let bad = || ParseReleaseError::InvalidEntry(line.to_string());
            let mut parts = line.split_whitespace();
            let (Some(sha), Some(size), Some(path), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(bad());
            };
            let file = IndexFile {
                sha256: sha.parse().map_err(|_| bad())?,
                size: size.parse().map_err(|_| bad())?,
            };
            Ok((path.to_string(), file))
        })
        .collect()
}

impl FromStr for Release {
    type Err = ParseReleaseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut suite = None;
        let mut codename = None;
        let mut date = None;
        let mut valid_until = None;
        let mut acquire_by_hash = false;
        let mut not_automatic = false;
        let mut but_automatic_upgrades = false;
        let mut files = None;

        for (key, value) in parse_stanza(s) {
            match key.as_str() {
                "Suite" => suite = Some(value),
                "Codename" => codename = Some(value),
                "Date" => date = Some(parse_date("Date", &value)?),
                "Valid-Until" => valid_until = Some(parse_date("Valid-Until", &value)?),
                "Acquire-By-Hash" => acquire_by_hash = value == "yes",
                "NotAutomatic" => not_automatic = value == "yes",
                "ButAutomaticUpgrades" => but_automatic_upgrades = value == "yes",
                "SHA256" => files = Some(parse_files(&value)?),
                _ => {}
            }
        }

        Ok(Release {
            suite,
            codename,
            date: date.ok_or(ParseReleaseError::MissingField("Date"))?,
            valid_until,
            not_automatic,
            but_automatic_upgrades,
            acquire_by_hash,
            // Without it nothing the Release signs reaches the packages.
            files: files.ok_or(ParseReleaseError::MissingField("SHA256"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = "\
Origin: Debian
Suite: oldstable-security
Codename: bookworm-security
Date: Thu, 15 Oct 2026 20:04:09 UTC
Valid-Until: Thu, 22 Oct 2026 20:04:09 UTC
Acquire-By-Hash: yes
Components: updates/main
SHA256:
 76cebcedb6d42d967993e871fe800f58657f75d0029e6a5c812bd0b75a654717  2327837 main/binary-arm64/Packages
 66b36878589681e4267d25941754f718e7dc7f92664f6da08c2eecf0f0cb754b   350676 main/binary-arm64/Packages.xz
";

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn parses_a_release() {
        let r: Release = RELEASE.parse().unwrap();
        assert_eq!(r.suite.as_deref(), Some("oldstable-security"));
        assert_eq!(r.codename.as_deref(), Some("bookworm-security"));
        assert_eq!(r.date, at("2026-10-15T20:04:09Z"));
        assert_eq!(r.valid_until, Some(at("2026-10-22T20:04:09Z")));
        assert!(r.acquire_by_hash);
        assert_eq!(r.files.len(), 2);
    }

    #[test]
    fn parses_the_real_bookworm_release() {
        let signed = include_str!("testdata/bookworm.InRelease");
        let text = signed
            .split_once("\n\n")
            .and_then(|(_, rest)| rest.split_once("-----BEGIN PGP SIGNATURE"))
            .map(|(text, _)| text)
            .unwrap();
        let r: Release = text.parse().unwrap();
        assert_eq!(r.codename.as_deref(), Some("bookworm"));
        assert_eq!(r.valid_until, None);
        let (path, _) = r.packages_index("main", "arm64").unwrap();
        assert_eq!(path, "main/binary-arm64/Packages.xz");
    }

    #[test]
    fn prefers_xz_and_names_missing_indexes() {
        let r: Release = RELEASE.parse().unwrap();
        let (path, file) = r.packages_index("main", "arm64").unwrap();
        assert_eq!(path, "main/binary-arm64/Packages.xz");
        assert_eq!(file.size, 350676);
        assert_eq!(
            r.packages_index("contrib", "arm64")
                .unwrap_err()
                .to_string(),
            "the Release file lists no Packages index for contrib/binary-arm64"
        );
    }

    #[test]
    fn checks_distribution_and_expiry() {
        let r: Release = RELEASE.parse().unwrap();
        let before = at("2026-10-16T00:00:00Z");
        assert_eq!(r.check("bookworm-security", before, None), Ok(()));
        assert_eq!(r.check("oldstable-security", before, None), Ok(()));
        assert!(matches!(
            r.check("trixie-security", before, None),
            Err(ReleaseError::WrongDistribution { .. })
        ));
        assert_eq!(
            r.check("bookworm-security", at("2026-10-23T00:00:00Z"), None),
            Err(ReleaseError::Expired(at("2026-10-22T20:04:09Z")))
        );
    }

    #[test]
    fn checks_snapshots_at_their_own_time() {
        // Dated 2026-10-15 20:04, valid until 2026-10-22 20:04.
        let r: Release = RELEASE.parse().unwrap();
        let long_after = at("2027-01-01T00:00:00Z");
        let dist = "bookworm-security";

        // Expired now, but valid when the snapshot was taken.
        assert_eq!(
            r.check(dist, long_after, Some(at("2026-10-16T00:00:00Z"))),
            Ok(())
        );
        assert_eq!(
            r.check(dist, long_after, Some(at("2026-10-23T00:00:00Z"))),
            Err(ReleaseError::ExpiredAtSnapshot {
                until: at("2026-10-22T20:04:09Z"),
                at: at("2026-10-23T00:00:00Z"),
            })
        );
        // A snapshot can't hold a Release from after it.
        assert_eq!(
            r.check(dist, long_after, Some(at("2026-10-15T00:00:00Z"))),
            Err(ReleaseError::AfterSnapshot {
                date: at("2026-10-15T20:04:09Z"),
                at: at("2026-10-15T00:00:00Z"),
            })
        );
        // The distribution is still checked.
        assert!(matches!(
            r.check(
                "trixie-security",
                long_after,
                Some(at("2026-10-16T00:00:00Z"))
            ),
            Err(ReleaseError::WrongDistribution { .. })
        ));
    }

    #[test]
    fn defaults_the_pin_priority_like_apt() {
        let priority = |extra: &str| {
            format!("{extra}{RELEASE}")
                .parse::<Release>()
                .unwrap()
                .default_pin_priority()
        };
        assert_eq!(priority(""), 500);
        assert_eq!(priority("NotAutomatic: yes\n"), 1);
        assert_eq!(
            priority("NotAutomatic: yes\nButAutomaticUpgrades: yes\n"),
            100
        );
        assert_eq!(priority("ButAutomaticUpgrades: yes\n"), 500);
    }

    #[test]
    fn rejects_malformed_releases() {
        let without = |field: &str| {
            RELEASE
                .lines()
                .filter(|l| !l.starts_with(field))
                .filter(|l| field != "SHA256" || !l.starts_with(' '))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            without("Date").parse::<Release>(),
            Err(ParseReleaseError::MissingField("Date"))
        );
        assert_eq!(
            without("SHA256").parse::<Release>(),
            Err(ParseReleaseError::MissingField("SHA256"))
        );
        let bad_date = RELEASE.replace("Thu, 15 Oct", "Thursday");
        assert!(matches!(
            bad_date.parse::<Release>(),
            Err(ParseReleaseError::InvalidField { field: "Date", .. })
        ));
        let bad_entry = RELEASE.replace("  2327837 ", " lots ");
        assert!(matches!(
            bad_entry.parse::<Release>(),
            Err(ParseReleaseError::InvalidEntry(_))
        ));
    }
}
