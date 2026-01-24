//! Debian dependency resolver.
//!
//! Resolves package dependencies using a stack-based algorithm to handle
//! circular dependencies and alternatives.
//!
//! # Example
//!
//! ```ignore
//! use paranoid::repository::debian::{PackageIndexSet, Architecture};
//!
//! let index: PackageIndexSet = /* ... */;
//!
//! // Simple resolution
//! let result = index.resolve_all("bash", None, Architecture::Amd64).execute()?;
//!
//! // With options
//! let result = index
//!     .resolve_all("bash", None, Architecture::Amd64)
//!     .with_recommends(true)
//!     .execute()?;
//!
//! // Access results with provenance
//! println!("Root: {} from {}", result.root.package.name, result.root.repository);
//! ```

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use super::architecture::Architecture;
use super::package::DebianPackage;
use super::package_index_set::PackageIndexSet;
use super::version::DebianVersion;
use crate::repository::Package;
use crate::version::VersionConstraint;

/// Errors that can occur during dependency resolution.
#[derive(Debug, Error)]
pub enum ResolveError {
    /// The requested package was not found in the index.
    #[error("package not found: {name}{} for architecture {architecture:?}", version_constraint.as_ref().map(|c| format!(" ({:?})", c)).unwrap_or_default())]
    PackageNotFound {
        name: String,
        version_constraint: Option<VersionConstraint<DebianVersion>>,
        architecture: Architecture,
    },
}

/// A resolved package with its source repository (owned).
///
/// This is the owned version of [`super::index_set::SourcedPackage`],
/// used in resolution results where we need to own the data.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub package: DebianPackage,
    pub repository: String,
}

/// Dependency graph: maps each package name to the names of its direct dependencies.
///
/// Only packages that appear as parents (i.e. have at least one resolved child)
/// are present as keys. The root package is always a key if it has any deps.
pub type DepGraph = HashMap<String, Vec<String>>;

/// Result of resolving a package and its dependencies.
#[derive(Debug)]
pub struct ResolveResult {
    /// The requested root package with its source repository.
    pub root: ResolvedEntry,
    /// All transitive dependencies (not including root) with their repositories.
    pub dependencies: Vec<ResolvedEntry>,
    /// Dependencies that could not be resolved.
    pub unmet: Vec<UnmetDependency>,
    /// Adjacency map: parent package name → list of direct dependency names.
    ///
    /// Used to reconstruct the dependency tree for display.
    pub graph: DepGraph,
}

/// A dependency that could not be satisfied.
#[derive(Debug, Clone)]
pub struct UnmetDependency {
    /// Package that requires this dependency.
    pub required_by: String,
    /// Name of the unmet dependency.
    pub name: String,
    /// Version constraint (if any).
    pub version_constraint: Option<VersionConstraint<DebianVersion>>,
}

/// Builder for configuring and executing dependency resolution.
///
/// Created via [`PackageIndexSet::resolve_all`].
#[must_use = "ResolveBuilder does nothing until .execute() is called"]
pub struct ResolveBuilder<'a> {
    index: &'a PackageIndexSet,
    name: String,
    version_constraint: Option<VersionConstraint<DebianVersion>>,
    arch: Architecture,
    include_recommends: bool,
}

impl<'a> ResolveBuilder<'a> {
    /// Create a new resolve builder.
    ///
    /// This is typically called via [`PackageIndexSet::resolve_all`] rather than directly.
    pub fn new(
        index: &'a PackageIndexSet,
        name: &str,
        version_constraint: Option<&VersionConstraint<DebianVersion>>,
        arch: Architecture,
    ) -> Self {
        Self {
            index,
            name: name.to_string(),
            version_constraint: version_constraint.cloned(),
            arch,
            include_recommends: false,
        }
    }

    /// Include Recommends as dependencies.
    ///
    /// By default, only Depends and Pre-Depends are resolved.
    /// Setting this to `true` will also resolve Recommends.
    pub fn with_recommends(mut self, include: bool) -> Self {
        self.include_recommends = include;
        self
    }

    /// Execute the resolution and return the result.
    ///
    /// This resolves the root package and all its transitive dependencies
    /// using a stack-based algorithm to handle circular dependencies.
    pub fn execute(self) -> Result<ResolveResult, ResolveError> {
        let root = self
            .index
            .resolve(&self.name, self.version_constraint.as_ref(), self.arch)
            .ok_or_else(|| ResolveError::PackageNotFound {
                name: self.name.clone(),
                version_constraint: self.version_constraint.clone(),
                architecture: self.arch,
            })?;

        let root = ResolvedEntry {
            package: root.package.clone(),
            repository: root.repository.to_string(),
        };

        let mut resolved: HashMap<String, ResolvedEntry> = HashMap::new();
        let mut unmet: Vec<UnmetDependency> = Vec::new();
        let mut already_processed: HashSet<String> = HashSet::new();
        let mut graph: DepGraph = HashMap::new();

        // Stack of (dependency, required_by)
        let mut stack: Vec<(PendingDependency, String)> = Vec::new();

        // Add root's dependencies to the stack
        self.push_dependencies(&mut stack, &root.package, &root.package.name);

        already_processed.insert(package_key(
            &root.package.name,
            &root.package.version.to_string(),
        ));

        while let Some((dep, required_by)) = stack.pop() {
            // Try to resolve this dependency
            let src = self
                .index
                .resolve(&dep.name, dep.version.as_ref(), self.arch);

            match src {
                Some(src) => {
                    let resolved_name = src.package.name.clone();
                    let key = package_key(&resolved_name, &src.package.version.to_string());

                    // Record the edge in the graph regardless of whether we've
                    // already visited this package (we still want to show the edge).
                    graph
                        .entry(required_by.clone())
                        .or_default()
                        .push(resolved_name.clone());

                    // Circular dependency (or duplicate) check — don't traverse again
                    if already_processed.contains(&key) {
                        continue;
                    }

                    already_processed.insert(key);

                    // Add this package's dependencies to the stack
                    self.push_dependencies(&mut stack, src.package, &resolved_name);

                    // Add to resolved list (skip if same as root)
                    if src.package.name != root.package.name
                        || src.package.version != root.package.version
                    {
                        resolved.insert(
                            resolved_name,
                            ResolvedEntry {
                                package: src.package.clone(),
                                repository: src.repository.to_string(),
                            },
                        );
                    }
                }
                None => {
                    // Could not resolve dependency
                    unmet.push(UnmetDependency {
                        required_by,
                        name: dep.name,
                        version_constraint: dep.version,
                    });
                }
            }
        }

        Ok(ResolveResult {
            root,
            dependencies: resolved.into_values().collect(),
            unmet,
            graph,
        })
    }

    /// Push a package's dependencies onto the stack.
    fn push_dependencies(
        &self,
        stack: &mut Vec<(PendingDependency, String)>,
        pkg: &DebianPackage,
        required_by: &str,
    ) {
        // Always include Depends and Pre-Depends
        for dep in pkg.dependencies() {
            stack.push((
                PendingDependency {
                    name: dep.name.clone(),
                    version: dep.version.clone(),
                },
                required_by.to_string(),
            ));
        }

        // Optionally include Recommends
        if self.include_recommends {
            for dep in pkg.recommends() {
                stack.push((
                    PendingDependency {
                        name: dep.name.clone(),
                        version: dep.version.clone(),
                    },
                    required_by.to_string(),
                ));
            }
        }
    }
}

/// A dependency waiting to be resolved.
#[derive(Debug, Clone)]
struct PendingDependency {
    name: String,
    version: Option<VersionConstraint<DebianVersion>>,
}

/// Create a unique key for a package (name + version).
fn package_key(name: &str, version: &str) -> String {
    format!("{}={}", name, version)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::super::package_index::PackageIndex;
    use super::super::test_utils::{make_package, make_package_with_deps, provides};
    use super::*;

    /// Helper to create an PackageIndexSet from a single PackageIndex.
    fn make_index_set(pkg_index: PackageIndex) -> PackageIndexSet {
        let mut set = PackageIndexSet::new();
        set.add_index("test".to_string(), pkg_index);
        set
    }

    #[test]
    fn test_version_constraint_parsing() {
        let constraint = VersionConstraint::try_parse(">= 2.17").unwrap();
        assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.36").unwrap()));
        assert!(constraint.is_satisfied_by(&DebianVersion::from_str("2.17").unwrap()));
        assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.16").unwrap()));

        let constraint = VersionConstraint::try_parse("<< 2.0").unwrap();
        assert!(constraint.is_satisfied_by(&DebianVersion::from_str("1.0").unwrap()));
        assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("2.0").unwrap()));

        let constraint = VersionConstraint::try_parse("= 1.0").unwrap();
        assert!(constraint.is_satisfied_by(&DebianVersion::from_str("1.0").unwrap()));
        assert!(!constraint.is_satisfied_by(&DebianVersion::from_str("1.1").unwrap()));
    }

    #[test]
    fn test_resolve_simple_package() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package("libc6", "2.36-9", "amd64"));
        let index = make_index_set(pkg_index);

        let pkg = index.resolve("libc6", None, Architecture::Amd64);

        assert!(pkg.is_some());
        let pkg = pkg.unwrap();
        assert_eq!(pkg.package.name, "libc6");
        assert_eq!(pkg.repository, "test");
    }

    #[test]
    fn test_resolve_with_version_constraint() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package("libc6", "2.35-1", "amd64"));
        pkg_index.add(make_package("libc6", "2.36-9", "amd64"));
        let index = make_index_set(pkg_index);

        // Should find 2.36-9 (satisfies >= 2.36)
        let constraint = VersionConstraint::try_parse(">= 2.36").unwrap();
        let pkg = index.resolve("libc6", Some(&constraint), Architecture::Amd64);
        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().package.version.to_string(), "2.36-9");

        // Should find either (both satisfy >= 2.30)
        let constraint = VersionConstraint::try_parse(">= 2.30").unwrap();
        let pkg = index.resolve("libc6", Some(&constraint), Architecture::Amd64);
        assert!(pkg.is_some());
    }

    #[test]
    fn test_resolve_virtual_package() {
        let mut pkg_index = PackageIndex::new();
        let mut mawk = make_package("mawk", "1.3.4", "amd64");
        mawk.provides = provides(&["awk"]);
        pkg_index.add(mawk);
        let index = make_index_set(pkg_index);

        let pkg = index.resolve("awk", None, Architecture::Amd64);

        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().package.name, "mawk");
    }

    #[test]
    fn test_resolve_fallback_to_all_arch() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package("ca-certificates", "20230311", "all"));
        let index = make_index_set(pkg_index);

        let pkg = index.resolve("ca-certificates", None, Architecture::Amd64);

        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().package.architecture, Some(Architecture::All));
    }

    #[test]
    fn test_resolve_all_includes_repository() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package("libfoo", "2.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert_eq!(result.root.package.name, "app");
        assert_eq!(result.root.repository, "test");
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].package.name, "libfoo");
        assert_eq!(result.dependencies[0].repository, "test");
    }

    #[test]
    fn test_resolve_all_simple() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package("libfoo", "2.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert_eq!(result.root.package.name, "app");
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].package.name, "libfoo");
        assert!(result.unmet.is_empty());
    }

    #[test]
    fn test_resolve_all_transitive() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package_with_deps(
            "libfoo",
            "2.0",
            "amd64",
            vec!["libbar"],
        ));
        pkg_index.add(make_package("libbar", "3.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert_eq!(result.root.package.name, "app");
        assert_eq!(result.dependencies.len(), 2);

        let names: Vec<_> = result
            .dependencies
            .iter()
            .map(|p| p.package.name.as_str())
            .collect();
        assert!(names.contains(&"libfoo"));
        assert!(names.contains(&"libbar"));
    }

    #[test]
    fn test_resolve_all_circular_deps() {
        let mut pkg_index = PackageIndex::new();
        // Circular: app -> libfoo -> libbar -> libfoo
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package_with_deps(
            "libfoo",
            "2.0",
            "amd64",
            vec!["libbar"],
        ));
        pkg_index.add(make_package_with_deps(
            "libbar",
            "3.0",
            "amd64",
            vec!["libfoo"],
        ));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        // Should not infinite loop, and should include both deps once
        assert_eq!(result.root.package.name, "app");
        assert_eq!(result.dependencies.len(), 2);
    }

    #[test]
    fn test_resolve_all_alternatives_picks_first() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo | libbar | libbaz"],
        ));
        pkg_index.add(make_package("libfoo", "1.0", "amd64"));
        pkg_index.add(make_package("libbar", "1.0", "amd64"));
        pkg_index.add(make_package("libbaz", "1.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        // Should pick libfoo (first alternative)
        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].package.name, "libfoo");
    }

    #[test]
    fn test_resolve_all_alternatives_fallback() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo | libbar"],
        ));
        // libfoo not available, only libbar
        pkg_index.add(make_package("libbar", "1.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        // libfoo not found, so it's unmet (we currently only try first alternative)
        assert_eq!(result.unmet.len(), 1);
        assert_eq!(result.unmet[0].name, "libfoo");
    }

    #[test]
    fn test_resolve_all_unmet_dependency() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libmissing"],
        ));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert!(result.dependencies.is_empty());
        assert_eq!(result.unmet.len(), 1);
        assert_eq!(result.unmet[0].name, "libmissing");
        assert_eq!(result.unmet[0].required_by, "app");
    }

    #[test]
    fn test_resolve_all_with_version_constraint_in_dep() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libc6 (>= 2.36)"],
        ));
        pkg_index.add(make_package("libc6", "2.35-1", "amd64"));
        pkg_index.add(make_package("libc6", "2.36-9", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert_eq!(result.dependencies.len(), 1);
        assert_eq!(result.dependencies[0].package.version.to_string(), "2.36-9");
    }

    #[test]
    fn test_resolve_not_found() {
        let index = PackageIndexSet::new();

        let result = index
            .resolve_all("notexist", None, Architecture::Amd64)
            .execute();
        assert!(result.is_err());
    }

    #[test]
    fn test_graph_direct_dependency() {
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package("libfoo", "2.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        let children = result.graph.get("app").expect("app should have children");
        assert!(
            children.contains(&"libfoo".to_string()),
            "app → libfoo missing from graph"
        );
    }

    #[test]
    fn test_graph_transitive() {
        // app → libfoo → libbar
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package_with_deps(
            "libfoo",
            "2.0",
            "amd64",
            vec!["libbar"],
        ));
        pkg_index.add(make_package("libbar", "3.0", "amd64"));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        assert!(result.graph["app"].contains(&"libfoo".to_string()));
        assert!(result.graph["libfoo"].contains(&"libbar".to_string()));
        assert!(
            !result.graph.contains_key("libbar"),
            "leaf should have no children"
        );
    }

    #[test]
    fn test_graph_circular_no_infinite_loop_and_edges_recorded() {
        // app → libfoo → libbar → libfoo (cycle)
        let mut pkg_index = PackageIndex::new();
        pkg_index.add(make_package_with_deps(
            "app",
            "1.0",
            "amd64",
            vec!["libfoo"],
        ));
        pkg_index.add(make_package_with_deps(
            "libfoo",
            "2.0",
            "amd64",
            vec!["libbar"],
        ));
        pkg_index.add(make_package_with_deps(
            "libbar",
            "3.0",
            "amd64",
            vec!["libfoo"],
        ));
        let index = make_index_set(pkg_index);

        let result = index
            .resolve_all("app", None, Architecture::Amd64)
            .execute()
            .unwrap();

        // All edges must be present even though libfoo is visited twice
        assert!(result.graph["app"].contains(&"libfoo".to_string()));
        assert!(result.graph["libfoo"].contains(&"libbar".to_string()));
        // The back-edge libbar → libfoo must also be recorded
        assert!(result.graph["libbar"].contains(&"libfoo".to_string()));
    }
}
