//! Multi-repository index with provenance tracking.
//!
//! `PackageIndexSet` wraps multiple `PackageIndex` instances and provides lookups
//! that return both the package and its source repository.

use super::architecture::Architecture;
use super::package::DebianPackage;
use super::package_index::PackageIndex;
use super::version::DebianVersion;

/// A package found in an index set, with the repository it came from.
#[derive(Debug, Clone, Copy)]
pub struct SourcedPackage<'a> {
    pub package: &'a DebianPackage,
    pub repository: &'a str,
}

/// A package that provides a virtual package, and the version it provides
/// (`Provides: foo (= 1.0)`), if any.
#[derive(Debug, Clone, Copy)]
pub struct SourcedProvider<'a> {
    pub provider: SourcedPackage<'a>,
    pub provided_version: Option<&'a DebianVersion>,
}

/// The package indexes of every repository in a manifest, in declaration
/// order.
///
/// Lookups cover both the target architecture and `all`, and list
/// repositories in order, so callers that pick "the first best" get the
/// earliest repository on ties. See [`super::resolver`] for how packages are
/// chosen.
#[derive(Default)]
pub struct PackageIndexSet {
    indices: Vec<(String, PackageIndex)>,
}

impl PackageIndexSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a repository's index. Earlier repositories win version ties.
    pub fn add_index(&mut self, name: String, index: PackageIndex) {
        self.indices.push((name, index));
    }

    /// Every real package called `name` for `arch` or `all`, in repository
    /// order.
    pub fn packages<'a, 'n>(
        &'a self,
        name: &'n str,
        arch: Architecture,
    ) -> impl Iterator<Item = SourcedPackage<'a>> + 'n
    where
        'a: 'n,
    {
        self.indices.iter().flat_map(move |(repo, index)| {
            [arch, Architecture::All]
                .into_iter()
                .flat_map(move |arch| index.versions(name, arch))
                .map(move |package| SourcedPackage {
                    package,
                    repository: repo,
                })
        })
    }

    /// Every package for `arch` or `all` that provides `name`, in
    /// repository order.
    pub fn providers<'a, 'n>(
        &'a self,
        name: &'n str,
        arch: Architecture,
    ) -> impl Iterator<Item = SourcedProvider<'a>> + 'n
    where
        'a: 'n,
    {
        self.indices.iter().flat_map(move |(repo, index)| {
            [arch, Architecture::All]
                .into_iter()
                .flat_map(move |arch| index.virtual_providers(name, arch))
                .map(move |p| SourcedProvider {
                    provider: SourcedPackage {
                        package: &p.provider,
                        repository: repo,
                    },
                    provided_version: p.provided_version.as_ref(),
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::debian::test_utils::{make_package, provides};

    fn make_index_set() -> PackageIndexSet {
        let mut debian = PackageIndex::new();
        debian.add(make_package("libc6", "2.36-9", "amd64"));
        debian.add(make_package("ca-certificates", "20230311", "all"));
        let mut mawk = make_package("mawk", "1.3.4", "amd64");
        mawk.provides = provides(&["awk"]);
        debian.add(mawk);

        let mut security = PackageIndex::new();
        security.add(make_package("libc6", "2.36-10", "amd64"));
        let mut gawk = make_package("gawk", "5.2", "all");
        gawk.provides = provides(&["awk (= 5.2)"]);
        security.add(gawk);

        let mut set = PackageIndexSet::new();
        set.add_index("debian".to_string(), debian);
        set.add_index("security".to_string(), security);
        set
    }

    #[test]
    fn packages_lists_every_repository_in_order() {
        let set = make_index_set();
        let found: Vec<_> = set
            .packages("libc6", Architecture::Amd64)
            .map(|p| format!("{} {}", p.repository, p.package.version))
            .collect();
        assert_eq!(found, ["debian 2.36-9", "security 2.36-10"]);
    }

    #[test]
    fn packages_include_arch_all() {
        let set = make_index_set();
        let found: Vec<_> = set
            .packages("ca-certificates", Architecture::Arm64)
            .map(|p| p.package.architecture)
            .collect();
        assert_eq!(found, [Some(Architecture::All)]);
        assert_eq!(set.packages("nonexistent", Architecture::Amd64).count(), 0);
    }

    #[test]
    fn providers_include_arch_all_and_provided_versions() {
        let set = make_index_set();
        let found: Vec<_> = set
            .providers("awk", Architecture::Amd64)
            .map(|p| {
                let version = p.provided_version.map(ToString::to_string);
                (
                    p.provider.package.name.as_str(),
                    p.provider.repository,
                    version,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("mawk", "debian", None),
                ("gawk", "security", Some("5.2".to_string()))
            ]
        );
    }
}
