//! Package dependency types.
//!
//! Handles Depends, Pre-Depends, and similar fields.

use std::str::FromStr;

use thiserror::Error;

use super::architecture::{ArchitectureRestriction, ParseArchitectureRestrictionError};
use super::version::DebianVersion;
use crate::version::{VersionConstraint, VersionRelation};

/// A single package dependency specification.
///
/// Example: `libc6 (>= 2.17) [amd64]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub name: String,
    pub version: Option<VersionConstraint<DebianVersion>>,
    pub architecture: Vec<ArchitectureRestriction>,
}

/// Error parsing a dependency specification.
#[derive(Debug, Error)]
pub enum ParseDependencySpecError {
    #[error("invalid version relation: {0}")]
    VersionRelation(#[from] crate::version::ParseVersionRelationError),

    #[error("invalid version: {0}")]
    Version(#[from] super::version::ParseDebianVersionError),

    #[error("invalid architecture restriction: {0}")]
    ArchitectureRestriction(#[from] ParseArchitectureRestrictionError),

    #[error("empty dependency specification")]
    EmptyString,
}

impl FromStr for DependencySpec {
    type Err = ParseDependencySpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err(ParseDependencySpecError::EmptyString);
        }

        let mut name = String::new();
        let mut version_raw = String::new();
        let mut architecture_raw = String::new();
        let mut version_flag: bool = false;
        let mut architecture_flag: bool = false;
        let mut version: Option<VersionConstraint<DebianVersion>> = None;
        let mut architecture: Vec<ArchitectureRestriction> = vec![];

        for c in s.chars() {
            match c {
                '[' => {
                    architecture_flag = true;
                }
                '(' => {
                    version_flag = true;
                }
                ')' => {
                    version_flag = false;
                }
                ']' => {
                    architecture_flag = false;
                }
                _ => {
                    if version_flag {
                        version_raw.push(c);
                    } else if architecture_flag {
                        architecture_raw.push(c);
                    } else if c.is_whitespace() {
                        continue;
                    } else {
                        name.push(c);
                    }
                }
            }
        }

        if let Some((r, v)) = version_raw.split_once(' ') {
            version = Some(VersionConstraint::new(
                VersionRelation::from_str(r)?,
                DebianVersion::from_str(v)?,
            ));
        }

        for sa in architecture_raw.split_whitespace() {
            let arch = ArchitectureRestriction::from_str(sa)?;
            architecture.push(arch);
        }

        // Strip the multi-arch qualifier `:any`, `:native`, `:amd64`, etc.
        // These qualifiers indicate cross-architecture dependency semantics that
        // we don't need to act on — the package name itself is what we resolve.
        // See: https://wiki.debian.org/MultiArch/Spec#The_M-A:_field
        let name = match name.find(':') {
            Some(colon) => name[..colon].to_string(),
            None => name,
        };

        Ok(DependencySpec {
            name,
            version,
            architecture,
        })
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

impl PackageDependency {
    pub fn resolve(&self) -> Option<&DependencySpec> {
        match self {
            // NOTE(juliana): let's just pick the first one.
            PackageDependency::Alternate(specs) => specs.first(),
            PackageDependency::Required(spec) => Some(spec),
        }
    }
}

/// Error parsing a package dependency.
#[derive(Debug, Error)]
#[error("invalid package dependency: {0}")]
pub struct ParsePackageDependencyError(#[from] pub ParseDependencySpecError);

impl FromStr for PackageDependency {
    type Err = ParsePackageDependencyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.find('|') {
            Some(_) => s
                .split('|')
                .map(DependencySpec::from_str)
                .collect::<Result<Vec<DependencySpec>, ParseDependencySpecError>>()
                .map(PackageDependency::Alternate)
                .map_err(ParsePackageDependencyError::from),

            None => DependencySpec::from_str(s)
                .map(PackageDependency::Required)
                .map_err(ParsePackageDependencyError::from),
        }
    }
}

/// A comma-separated list of package dependencies.
///
/// Parses strings like "libc6 (>= 2.17), libfoo | libbar, libssl"
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyList(pub Vec<PackageDependency>);

impl std::ops::Deref for DependencyList {
    type Target = Vec<PackageDependency>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<DependencyList> for Vec<PackageDependency> {
    fn from(list: DependencyList) -> Self {
        list.0
    }
}

/// Error parsing a dependency list.
#[derive(Debug, Error)]
#[error("invalid dependency at index {index} ({entry}): {source}")]
pub struct ParseDependencyListError {
    pub index: usize,
    pub entry: String,
    #[source]
    pub source: ParsePackageDependencyError,
}

impl FromStr for DependencyList {
    type Err = ParseDependencyListError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Ok(DependencyList(Vec::new()));
        }

        let mut deps = Vec::new();
        for (i, part) in s.split(',').enumerate() {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match PackageDependency::from_str(part) {
                Ok(dep) => deps.push(dep),
                Err(e) => {
                    return Err(ParseDependencyListError {
                        index: i,
                        entry: part.to_string(),
                        source: e,
                    });
                }
            }
        }
        Ok(DependencyList(deps))
    }
}

#[cfg(test)]
mod tests {
    use super::super::architecture::{Architecture, System};
    use super::*;

    #[test]
    fn test_parse_dependency_spec() {
        assert_eq!(
            DependencySpec::from_str("foo [armhf]").unwrap(),
            DependencySpec {
                name: "foo".to_string(),
                version: None,
                architecture: vec![ArchitectureRestriction::Include(
                    System::Any,
                    Architecture::Armhf
                )]
            }
        );

        assert_eq!(
            DependencySpec::from_str("foo (<< 1.0) [amd64]").unwrap(),
            DependencySpec {
                name: "foo".to_string(),
                version: Some(VersionConstraint::new(
                    VersionRelation::StrictlyEarlier,
                    DebianVersion::from_str("1.0").unwrap()
                )),
                architecture: vec![ArchitectureRestriction::Include(
                    System::Any,
                    Architecture::Amd64
                )]
            }
        );

        assert_eq!(
            DependencySpec::from_str("foo [!hurd-i386]").unwrap(),
            DependencySpec {
                name: "foo".to_string(),
                version: None,
                architecture: vec![ArchitectureRestriction::Exclude(
                    System::Hurd,
                    Architecture::I386
                )]
            }
        );

        assert_eq!(
            DependencySpec::from_str("foo [i386 amd64 kfreebsd-i386 armel armhf powerpc mips]")
                .unwrap(),
            DependencySpec {
                name: "foo".to_string(),
                version: None,
                architecture: vec![
                    ArchitectureRestriction::Include(System::Any, Architecture::I386),
                    ArchitectureRestriction::Include(System::Any, Architecture::Amd64),
                    ArchitectureRestriction::Include(System::KFreeBSD, Architecture::I386),
                    ArchitectureRestriction::Include(System::Any, Architecture::Armel),
                    ArchitectureRestriction::Include(System::Any, Architecture::Armhf),
                    ArchitectureRestriction::Include(System::Any, Architecture::PowerPC),
                    ArchitectureRestriction::Include(System::Any, Architecture::Mips),
                ]
            }
        );
    }

    #[test]
    fn test_parse_package_dependency() {
        assert_eq!(
            PackageDependency::from_str("default-mta | mail-transport-agent | certificates")
                .unwrap(),
            PackageDependency::Alternate(vec![
                DependencySpec {
                    name: "default-mta".to_string(),
                    version: None,
                    architecture: vec![],
                },
                DependencySpec {
                    name: "mail-transport-agent".to_string(),
                    version: None,
                    architecture: vec![],
                },
                DependencySpec {
                    name: "certificates".to_string(),
                    version: None,
                    architecture: vec![],
                }
            ])
        );

        assert_eq!(
            PackageDependency::from_str("default-mta").unwrap(),
            PackageDependency::Required(DependencySpec {
                name: "default-mta".to_string(),
                version: None,
                architecture: vec![],
            })
        )
    }

    #[test]
    fn test_parse_dependency_spec_multiarch_qualifier_stripped() {
        // `:any`, `:native`, `:amd64` are multi-arch qualifiers — the resolver
        // should see only the package name.
        let spec = DependencySpec::from_str("nodejs:any").unwrap();
        assert_eq!(spec.name, "nodejs");

        let spec = DependencySpec::from_str("libc6:native").unwrap();
        assert_eq!(spec.name, "libc6");

        let spec = DependencySpec::from_str("libfoo:amd64 (>= 1.0)").unwrap();
        assert_eq!(spec.name, "libfoo");
        assert!(spec.version.is_some());
    }

    #[test]
    fn test_parse_dependency_spec_empty_error() {
        assert!(DependencySpec::from_str("").is_err());
        assert!(DependencySpec::from_str("   ").is_err());
    }

    #[test]
    fn test_parse_dependency_list() {
        let list = DependencyList::from_str("libc6 (>= 2.17), libfoo").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0],
            PackageDependency::Required(DependencySpec {
                name: "libc6".to_string(),
                version: Some(VersionConstraint::new(
                    VersionRelation::LaterOrEqual,
                    DebianVersion::from_str("2.17").unwrap()
                )),
                architecture: vec![],
            })
        );
    }

    #[test]
    fn test_parse_dependency_list_empty() {
        let list = DependencyList::from_str("").unwrap();
        assert!(list.is_empty());

        let list = DependencyList::from_str("   ").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_parse_dependency_list_with_empty_entries() {
        // Trailing comma creates empty entry which should be skipped
        let list = DependencyList::from_str("libc6, , libfoo").unwrap();
        assert_eq!(list.len(), 2);
    }

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        /// Valid Debian package name characters (lowercase alphanumeric, +, -, .)
        fn valid_package_name() -> impl Strategy<Value = String> {
            // Package names: start with alphanumeric, contain alphanumeric, +, -, .
            // At least 2 characters long
            prop::string::string_regex("[a-z][a-z0-9+.-]{1,30}").unwrap()
        }

        /// Valid version operators
        fn version_operator() -> impl Strategy<Value = &'static str> {
            prop_oneof![Just("<<"), Just("<="), Just("="), Just(">="), Just(">>"),]
        }

        /// Simple version string (epoch:upstream-revision)
        fn simple_version() -> impl Strategy<Value = String> {
            prop::string::string_regex("[0-9]+\\.[0-9]+(\\.[0-9]+)?(-[0-9]+)?").unwrap()
        }

        proptest! {
            /// Simple package names should parse successfully.
            #[test]
            fn simple_package_name_parses(name in valid_package_name()) {
                let result = DependencySpec::from_str(&name);
                prop_assert!(result.is_ok(), "Failed to parse: {}", name);
                let spec = result.unwrap();
                prop_assert_eq!(&spec.name, &name);
                prop_assert!(spec.version.is_none());
                prop_assert!(spec.architecture.is_empty());
            }

            /// Package with version constraint should parse correctly.
            #[test]
            fn package_with_version_parses(
                name in valid_package_name(),
                op in version_operator(),
                version in simple_version()
            ) {
                let input = format!("{} ({} {})", name, op, version);
                let result = DependencySpec::from_str(&input);
                prop_assert!(result.is_ok(), "Failed to parse: {}", input);
                let spec = result.unwrap();
                prop_assert_eq!(&spec.name, &name);
                prop_assert!(spec.version.is_some());
            }

            /// Alternate dependencies should parse with correct count.
            #[test]
            fn alternate_deps_parse(
                name1 in valid_package_name(),
                name2 in valid_package_name(),
                name3 in valid_package_name()
            ) {
                let input = format!("{} | {} | {}", name1, name2, name3);
                let result = PackageDependency::from_str(&input);
                prop_assert!(result.is_ok(), "Failed to parse: {}", input);
                if let PackageDependency::Alternate(specs) = result.unwrap() {
                    prop_assert_eq!(specs.len(), 3);
                    prop_assert_eq!(&specs[0].name, &name1);
                    prop_assert_eq!(&specs[1].name, &name2);
                    prop_assert_eq!(&specs[2].name, &name3);
                } else {
                    prop_assert!(false, "Expected Alternate, got Required");
                }
            }

            /// Dependency list parsing preserves count.
            #[test]
            fn dependency_list_count(
                name1 in valid_package_name(),
                name2 in valid_package_name()
            ) {
                let input = format!("{}, {}", name1, name2);
                let result = DependencyList::from_str(&input);
                prop_assert!(result.is_ok(), "Failed to parse: {}", input);
                let list = result.unwrap();
                prop_assert_eq!(list.len(), 2);
            }
        }
    }
}
