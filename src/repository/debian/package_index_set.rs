//! Multi-repository index with provenance tracking.
//!
//! `PackagePackageIndexSet` wraps multiple `PackageIndex` instances and provides lookups
//! that return both the package and its source repository.

use std::collections::HashMap;

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
pub struct PackageIndexSet {
    indices: HashMap<String, PackageIndex>,
}

impl PackageIndexSet {
    pub fn new() -> Self {
        Self {
            indices: HashMap::new(),
        }
    }

    pub fn add_index(&mut self, name: String, index: PackageIndex) {
        self.indices.insert(name, index);
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
    ///
    /// Returns the first provider found that satisfies the constraint.
    fn find_virtual_provider(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Option<SourcedPackage<'_>> {
        for (repo_name, index) in &self.indices {
            if let Some(pkg) = index.find_virtual_provider(name, version_constraint, arch) {
                return Some(SourcedPackage {
                    package: pkg,
                    repository: repo_name,
                });
            }
        }
        None
    }

    /// Find a package that matches the given criteria across all repositories.
    ///
    /// If `version_constraint` is `None`, returns the latest version.
    /// Otherwise, returns the first version that satisfies the constraint.
    fn find_package(
        &self,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Option<SourcedPackage<'_>> {
        for (repo_name, index) in &self.indices {
            if let Some(pkg) = index.find_package(name, version_constraint, arch) {
                return Some(SourcedPackage {
                    package: pkg,
                    repository: repo_name,
                });
            }
        }
        None
    }
}

impl Default for PackageIndexSet {
    fn default() -> Self {
        Self::new()
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
