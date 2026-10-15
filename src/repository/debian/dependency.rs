//! Package relationships: `Depends`, `Pre-Depends` and `Provides`.
//!
//! The syntax is `name[:qualifier] [(op version)]`, with `|` between
//! alternatives and `,` between relations. See
//! <https://www.debian.org/doc/debian-policy/ch-relationships.html>.
//!
//! Architecture restrictions (`[amd64]`) and build profiles (`<!nocheck>`)
//! are only allowed in source packages, so they are an error here: binary
//! indexes never carry them.

use std::str::FromStr;

use thiserror::Error;

use super::version::{DebianVersion, ParseDebianVersionError};
use crate::version::{VersionConstraint, VersionRelation};

/// A single package relation: `libc6 (>= 2.17)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: String,
    pub version: Option<VersionConstraint<DebianVersion>>,
}

/// Error parsing a package relation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseDependencySpecError {
    #[error("empty relation")]
    Empty,

    #[error("invalid package name '{0}'")]
    Name(String),

    #[error("missing ')' in '{0}'")]
    UnclosedParen(String),

    #[error("missing or unknown operator in '({0})'")]
    Relation(String),

    #[error(transparent)]
    Version(#[from] ParseDebianVersionError),

    #[error("'{0}' is only allowed in source packages")]
    SourceOnly(String),

    #[error("unexpected '{0}'")]
    Trailing(String),

    #[error("Provides only allows '=', got '{0}'")]
    ProvidesRelation(VersionRelation),
}

/// A valid package name: lowercase letters, digits, `+`, `-` and `.`,
/// starting with a letter or digit.
fn is_package_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c))
}

impl FromStr for DependencySpec {
    type Err = ParseDependencySpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use ParseDependencySpecError as E;

        let s = s.trim();
        if s.is_empty() {
            return Err(E::Empty);
        }

        let name_end = s
            .find(|c: char| c.is_whitespace() || "([<".contains(c))
            .unwrap_or(s.len());
        let (name, mut rest) = s.split_at(name_end);

        // The multi-arch qualifier (`:any`, `:native`, `:amd64`) says which
        // architecture may satisfy the relation. We only ever install one
        // architecture, so we resolve the bare name.
        // https://wiki.debian.org/MultiArch/Spec
        let name = match name.split_once(':') {
            Some((name, qualifier)) if !qualifier.is_empty() => name,
            Some(_) => return Err(E::Name(name.to_string())),
            None => name,
        };
        if !is_package_name(name) {
            return Err(E::Name(name.to_string()));
        }

        let mut version = None;
        rest = rest.trim_start();
        if let Some(inside) = rest.strip_prefix('(') {
            let (inside, after) = inside
                .split_once(')')
                .ok_or_else(|| E::UnclosedParen(s.to_string()))?;
            let (relation, v) = VersionRelation::split_prefix(inside.trim())
                .ok_or_else(|| E::Relation(inside.to_string()))?;
            version = Some(VersionConstraint::new(relation, v.trim().parse()?));
            rest = after.trim_start();
        }

        if rest.starts_with(['[', '<']) {
            return Err(E::SourceOnly(rest.to_string()));
        }
        if !rest.is_empty() {
            return Err(E::Trailing(rest.to_string()));
        }

        Ok(DependencySpec {
            name: name.to_string(),
            version,
        })
    }
}

impl std::fmt::Display for DependencySpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)?;
        if let Some(version) = &self.version {
            write!(f, " ({version})")?;
        }
        Ok(())
    }
}

/// A package dependency, either required or one of several alternatives.
///
/// Examples:
/// - `Required`: `libc6 (>= 2.17)`
/// - `Alternate`: `default-mta | mail-transport-agent`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageDependency {
    Alternate(Vec<DependencySpec>),
    Required(DependencySpec),
}

impl std::fmt::Display for PackageDependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageDependency::Required(spec) => write!(f, "{spec}"),
            PackageDependency::Alternate(specs) => {
                for (i, spec) in specs.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" | ")?;
                    }
                    write!(f, "{spec}")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for PackageDependency {
    type Err = ParseDependencySpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut specs = s
            .split('|')
            .map(DependencySpec::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if specs.len() == 1 {
            PackageDependency::Required(specs.remove(0))
        } else {
            PackageDependency::Alternate(specs)
        })
    }
}

/// Error parsing a comma-separated relation field.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("in '{entry}': {source}")]
pub struct ParseRelationsError {
    pub entry: String,
    #[source]
    pub source: ParseDependencySpecError,
}

/// Parse a comma-separated relation field, such as `Depends` or `Provides`,
/// into its entries. Empty entries (`a, , b`) are skipped.
pub fn parse_relations<T>(field: &str) -> Result<Vec<T>, ParseRelationsError>
where
    T: FromStr<Err = ParseDependencySpecError>,
{
    field
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry.parse().map_err(|source| ParseRelationsError {
                entry: entry.to_string(),
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, version: Option<(VersionRelation, &str)>) -> DependencySpec {
        DependencySpec {
            name: name.to_string(),
            version: version.map(|(r, v)| VersionConstraint::new(r, v.parse().unwrap())),
        }
    }

    #[test]
    fn parses_relations() {
        use VersionRelation::*;

        for (input, expected) in [
            ("libc6", spec("libc6", None)),
            (
                "libc6 (>= 2.17)",
                spec("libc6", Some((LaterOrEqual, "2.17"))),
            ),
            // The space after the operator, and around the version, is optional.
            (
                "libc6 (>=2.17)",
                spec("libc6", Some((LaterOrEqual, "2.17"))),
            ),
            (
                "libc6(>= 2.17)",
                spec("libc6", Some((LaterOrEqual, "2.17"))),
            ),
            (
                "libc6 ( >=  2.17 )",
                spec("libc6", Some((LaterOrEqual, "2.17"))),
            ),
            (
                "foo (<< 1:1.0-1)",
                spec("foo", Some((StrictlyEarlier, "1:1.0-1"))),
            ),
            ("foo (= 1.0)", spec("foo", Some((Equal, "1.0")))),
            ("g++-12", spec("g++-12", None)),
            (
                "libstdc++6 (>= 13.1)",
                spec("libstdc++6", Some((LaterOrEqual, "13.1"))),
            ),
            // Multi-arch qualifiers are dropped.
            ("nodejs:any", spec("nodejs", None)),
            ("libc6:native", spec("libc6", None)),
            (
                "libfoo:amd64 (>= 1.0)",
                spec("libfoo", Some((LaterOrEqual, "1.0"))),
            ),
        ] {
            assert_eq!(DependencySpec::from_str(input), Ok(expected), "{input:?}");
        }
    }

    #[test]
    fn rejects_malformed_relations() {
        use ParseDependencySpecError as E;

        for (input, expected) in [
            ("", E::Empty),
            ("   ", E::Empty),
            ("Foo", E::Name("Foo".into())),
            ("foo_bar", E::Name("foo_bar".into())),
            ("foo:", E::Name("foo:".into())),
            ("(>= 1.0)", E::Name("".into())),
            ("foo (>= 1.0", E::UnclosedParen("foo (>= 1.0".into())),
            ("foo (1.0)", E::Relation("1.0".into())),
            ("foo (> 1.0)", E::Relation("> 1.0".into())),
            ("foo (< 1.0)", E::Relation("< 1.0".into())),
            ("foo (== 1.0)", E::Relation("== 1.0".into())),
            ("foo [amd64]", E::SourceOnly("[amd64]".into())),
            ("foo <!nocheck>", E::SourceOnly("<!nocheck>".into())),
            ("foo (>= 1.0) [amd64]", E::SourceOnly("[amd64]".into())),
            ("foo bar", E::Trailing("bar".into())),
            ("foo (>= 1.0) bar", E::Trailing("bar".into())),
        ] {
            assert_eq!(DependencySpec::from_str(input), Err(expected), "{input:?}");
        }

        assert!(matches!(
            DependencySpec::from_str("foo (>= )"),
            Err(E::Version(_))
        ));
        assert!(matches!(
            DependencySpec::from_str("foo (>= 1.0 2.0)"),
            Err(E::Version(_))
        ));
    }

    #[test]
    fn parses_alternatives() {
        assert_eq!(
            PackageDependency::from_str("default-mta | mail-transport-agent | certificates"),
            Ok(PackageDependency::Alternate(vec![
                spec("default-mta", None),
                spec("mail-transport-agent", None),
                spec("certificates", None),
            ]))
        );
        assert_eq!(
            PackageDependency::from_str("default-mta"),
            Ok(PackageDependency::Required(spec("default-mta", None)))
        );
        assert_eq!(
            PackageDependency::from_str("foo |"),
            Err(ParseDependencySpecError::Empty)
        );
    }

    #[test]
    fn parses_relation_fields() {
        let deps: Vec<PackageDependency> = parse_relations("libc6 (>= 2.17), libfoo").unwrap();
        assert_eq!(
            deps,
            [
                PackageDependency::Required(spec(
                    "libc6",
                    Some((VersionRelation::LaterOrEqual, "2.17"))
                )),
                PackageDependency::Required(spec("libfoo", None)),
            ]
        );

        assert!(parse_relations::<PackageDependency>("").unwrap().is_empty());
        assert!(
            parse_relations::<PackageDependency>("   ")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            parse_relations::<PackageDependency>("libc6, , libfoo,")
                .unwrap()
                .len(),
            2
        );
    }

    /// One bad entry fails the whole field, instead of the package silently
    /// losing all its dependencies.
    #[test]
    fn a_bad_entry_fails_the_field() {
        assert_eq!(
            parse_relations::<PackageDependency>("libc6 (>= 2.17), libfoo (> 1)"),
            Err(ParseRelationsError {
                entry: "libfoo (> 1)".into(),
                source: ParseDependencySpecError::Relation("> 1".into()),
            })
        );
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        fn package_name() -> impl Strategy<Value = String> {
            "[a-z0-9][a-z0-9+.-]{1,30}"
        }

        fn operator() -> impl Strategy<Value = &'static str> {
            prop_oneof![Just("<<"), Just("<="), Just("="), Just(">="), Just(">>")]
        }

        fn version() -> impl Strategy<Value = String> {
            "([0-9]:)?[0-9][0-9a-z.+~]{0,8}(-[0-9a-z.+~]{1,6})?"
        }

        proptest! {
            #[test]
            fn bare_names_parse(name in package_name()) {
                let spec = DependencySpec::from_str(&name).unwrap();
                prop_assert_eq!(spec.name, name);
                prop_assert!(spec.version.is_none());
            }

            /// Any spacing around the operator and version gives the same
            /// constraint.
            #[test]
            fn versioned_relations_parse(
                name in package_name(),
                op in operator(),
                version in version(),
                pad in prop::sample::select(vec!["", " ", "  "]),
            ) {
                let input = format!("{name} ({pad}{op}{pad}{version}{pad})");
                let spec = DependencySpec::from_str(&input).unwrap();
                prop_assert_eq!(spec.name, name);
                let constraint = spec.version.unwrap();
                prop_assert_eq!(constraint.relation.as_str(), op);
                prop_assert_eq!(constraint.version, version.parse::<DebianVersion>().unwrap());
            }

            #[test]
            fn alternatives_parse(names in prop::collection::vec(package_name(), 2..5)) {
                let input = names.join(" | ");
                let PackageDependency::Alternate(specs) = PackageDependency::from_str(&input).unwrap()
                else {
                    return Err(TestCaseError::fail("expected alternatives"));
                };
                let parsed: Vec<_> = specs.into_iter().map(|s| s.name).collect();
                prop_assert_eq!(parsed, names);
            }
        }
    }
}
