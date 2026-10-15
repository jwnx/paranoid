//! Multi-repository index with provenance tracking.
//!
//! `PackageIndexSet` wraps multiple `PackageIndex` instances and provides lookups
//! that return both the package and its source repository.

use super::architecture::Architecture;
use super::package::DebianPackage;
use super::package_index::PackageIndex;
use super::resolver::ResolveBuilder;
use super::version::DebianVersion;
use crate::version::VersionConstraint;

/// A package found in an index set, with its source repository (borrowed).
///
/// This is the borrowed version used for lookups. For owned results
/// (e.g., in resolution), see [`super::resolver::SourcedPackage`].
#[derive(Debug, Clone)]
pub struct SourcedPackage<'a> {
    pub package: &'a DebianPackage,
    pub repository: &'a str,
}

/// A set of package indices from multiple repositories.
///
/// Provides unified lookups across all repositories while tracking
/// which repository each package comes from.
///
/// Repositories are kept in insertion order, which is the order they are
/// declared in the manifest. When several repositories carry a matching
/// package, the highest version wins and ties go to the earlier repository,
/// so the same inputs always resolve to the same package.
///
/// # Example
///
/// ```ignore
/// use paranoid::repository::debian::{PackageIndexSet, Architecture};
///
/// let mut index = PackageIndexSet::new();
/// // ... add indices ...
///
/// // Simple package lookup
/// if let Some(pkg) = index.resolve("bash", None, Architecture::Amd64) {
///     println!("Found {} from {}", pkg.package.name, pkg.repository);
/// }
///
/// // Full transitive resolution
/// let result = index
///     .resolve_all("bash", None, Architecture::Amd64)
///     .with_recommends(true)
///     .execute()?;
/// ```
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

    /// Pick the best match across repositories: highest version first, then
    /// the earliest repository.
    fn best<'a>(
        &'a self,
        find: impl Fn(&'a PackageIndex) -> Option<&'a DebianPackage>,
    ) -> Option<SourcedPackage<'a>> {
        self.indices
            .iter()
            .filter_map(|(repo, index)| {
                find(index).map(|package| SourcedPackage {
                    package,
                    repository: repo,
                })
            })
            // `max_by` keeps the last of equal elements, so iterate in reverse
            // to let the earliest repository win ties.
            .rev()
            .max_by(|a, b| a.package.version.cmp(&b.package.version))
    }

    /// Resolve a single package by name, optionally with a version constraint.
    ///
    /// This handles:
    /// 1. Virtual packages (via Provides)
    /// 2. Version constraints
    /// 3. Architecture-specific packages
    /// 4. Architecture "all" fallback
    #[must_use]
    pub fn resolve(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Option<SourcedPackage<'_>> {
        self.find_virtual_provider(name, version_constraint, arch)
            .or_else(|| self.find_package(name, version_constraint, arch))
            .or_else(|| self.find_package(name, version_constraint, Architecture::All))
    }

    /// Resolve a package with all its transitive dependencies.
    ///
    /// Returns a builder that can be configured before execution.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result = index
    ///     .resolve_all("bash", None, Architecture::Amd64)
    ///     .with_recommends(true)
    ///     .execute()?;
    /// ```
    pub fn resolve_all(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> ResolveBuilder<'_> {
        ResolveBuilder::new(self, name, version_constraint, arch)
    }

    /// Find a virtual package provider across all repositories.
    fn find_virtual_provider(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Option<SourcedPackage<'_>> {
        self.best(|index| index.find_virtual_provider(name, version_constraint, arch))
    }

    /// Find the highest version matching the given criteria across all
    /// repositories.
    fn find_package(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Option<SourcedPackage<'_>> {
        self.best(|index| index.find_package(name, version_constraint, arch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::debian::test_utils::make_package;

    fn make_index_set() -> PackageIndexSet {
        let mut debian = PackageIndex::new();
        debian.add(make_package("libc6", "2.36-9", "amd64"));
        debian.add(make_package("bash", "5.2.15", "amd64"));

        let mut security = PackageIndex::new();
        security.add(make_package("libc6", "2.36-10", "amd64"));
        security.add(make_package("openssl", "3.0.11", "amd64"));

        let mut set = PackageIndexSet::new();
        set.add_index("debian".to_string(), debian);
        set.add_index("security".to_string(), security);
        set
    }

    #[test]
    fn test_resolve_returns_match_with_repository() {
        let set = make_index_set();

        let result = set.resolve("bash", None, Architecture::Amd64);
        assert!(result.is_some());

        let sourced = result.unwrap();
        assert_eq!(sourced.package.name, "bash");
        assert_eq!(sourced.repository, "debian");
    }

    #[test]
    fn test_resolve_prefers_highest_version_across_repositories() {
        let set = make_index_set();

        let sourced = set.resolve("libc6", None, Architecture::Amd64).unwrap();
        assert_eq!(sourced.package.version.to_string(), "2.36-10");
        assert_eq!(sourced.repository, "security");
    }

    #[test]
    fn test_resolve_breaks_version_ties_by_repository_order() {
        let mut set = PackageIndexSet::new();
        for repo in ["first", "second", "third"] {
            let mut index = PackageIndex::new();
            index.add(make_package("bash", "5.2.15", "amd64"));
            set.add_index(repo.to_string(), index);
        }

        let sourced = set.resolve("bash", None, Architecture::Amd64).unwrap();
        assert_eq!(sourced.repository, "first");
    }

    #[test]
    fn test_resolve_not_found() {
        let set = make_index_set();

        let result = set.resolve("nonexistent", None, Architecture::Amd64);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_uses_fallback_strategy() {
        let mut set = PackageIndexSet::new();
        let mut index = PackageIndex::new();
        // Only available as arch: all
        index.add(make_package("ca-certificates", "20230311", "all"));
        set.add_index("debian".to_string(), index);

        // Request for amd64 should fall back to all
        let result = set.resolve("ca-certificates", None, Architecture::Amd64);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().package.architecture,
            Some(Architecture::All)
        );
    }
}
