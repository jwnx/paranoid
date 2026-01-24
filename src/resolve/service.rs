//! Resolution service implementation.

use std::collections::HashMap;

use thiserror::Error;

use crate::arch::Architecture;
use crate::config::Manifest;
use crate::http::{Client, ClientError};
use crate::repository::debian::{
    DebianPackage, DebianRepository, DebianRepositoryError, DebianVersion, PackageIndexSet,
    ResolveError,
};
use crate::version::VersionConstraint;

/// Errors that can occur during resolution.
#[derive(Debug, Error)]
pub enum ResolveServiceError {
    #[error("client error: {0}")]
    Client(#[from] ClientError),

    #[error("repository error: {0}")]
    Repository(#[from] DebianRepositoryError),

    #[error("resolve error: {0}")]
    Resolve(#[from] ResolveError),

    #[error("unknown architecture: {0}")]
    UnknownArchitecture(String),

    #[error("unknown repository: {0}")]
    UnknownRepository(String),

    #[error("no repositories configured")]
    NoRepositories,

    #[error("invalid version constraint for package '{package}': {source}")]
    InvalidVersionConstraint {
        package: String,
        #[source]
        source: crate::version::ParseVersionConstraintError<
            <DebianVersion as std::str::FromStr>::Err,
        >,
    },
}

/// A resolved package with all metadata needed for the lock file.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: String,
    pub architecture: Architecture,
    pub sha256: String,
    pub filename: String,
    pub size: u64,
    /// The repository this package came from.
    pub repository: String,
    /// Whether this package was requested directly in the manifest
    /// (`true`) or pulled in as a transitive dependency (`false`).
    pub direct: bool,
}

impl ResolvedPackage {
    /// Create a ResolvedPackage from a DebianPackage and repository name.
    #[must_use]
    pub fn from_debian_package(
        pkg: &DebianPackage,
        repository: impl Into<String>,
        direct: bool,
    ) -> Self {
        Self {
            name: pkg.name.clone(),
            version: pkg.version.to_string(),
            architecture: pkg.architecture.unwrap_or(Architecture::All),
            sha256: pkg.sha256.clone(),
            filename: pkg.filename.clone(),
            size: pkg.size,
            repository: repository.into(),
            direct,
        }
    }
}

/// The complete resolution result.
#[derive(Debug)]
pub struct ResolvedManifest {
    /// All resolved packages (including transitive dependencies).
    pub packages: Vec<ResolvedPackage>,
    /// Packages that could not be resolved.
    pub unmet: Vec<String>,
    /// Merged dependency graph: parent name → sorted list of child names.
    pub graph: std::collections::HashMap<String, Vec<String>>,
}

/// Service for resolving packages from a manifest.
pub struct ResolveService {
    client: Client,
}

impl ResolveService {
    /// Create a new resolve service.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn new() -> Result<Self, ResolveServiceError> {
        Ok(Self {
            client: Client::new()?,
        })
    }

    /// Resolve all packages from a manifest.
    pub async fn resolve(
        &self,
        manifest: &Manifest,
    ) -> Result<ResolvedManifest, ResolveServiceError> {
        if manifest.repositories.is_empty() {
            return Err(ResolveServiceError::NoRepositories);
        }

        // Validate that all referenced repositories exist
        for pkg in &manifest.packages {
            if let Some(ref repo_name) = pkg.repository
                && !manifest.repositories.contains_key(repo_name)
            {
                return Err(ResolveServiceError::UnknownRepository(repo_name.clone()));
            }
        }

        let arch = manifest
            .image
            .as_ref()
            .map(|i| i.architecture.parse::<Architecture>())
            .unwrap_or(Ok(Architecture::Amd64))
            .map_err(|e| ResolveServiceError::UnknownArchitecture(e.to_string()))?;

        // Build the index set from all repositories
        let mut index_set = PackageIndexSet::new();
        for (repo_name, repo_config) in &manifest.repositories {
            let repo = DebianRepository::new(&self.client, repo_name, repo_config);
            let index = repo.fetch_index(arch).await?;
            index_set.add_index(repo_name.clone(), index);
        }

        // Resolve each requested package
        let mut all_packages: HashMap<String, ResolvedPackage> = HashMap::new();
        let mut all_unmet: Vec<String> = Vec::new();
        let mut all_graph: HashMap<String, Vec<String>> = HashMap::new();

        for pkg_spec in &manifest.packages {
            let version_constraint = pkg_spec
                .version
                .as_deref()
                .map(|v| {
                    v.parse::<VersionConstraint<DebianVersion>>()
                        .map_err(|source| ResolveServiceError::InvalidVersionConstraint {
                            package: pkg_spec.name.clone(),
                            source,
                        })
                })
                .transpose()?;

            let result = index_set
                .resolve_all(&pkg_spec.name, version_constraint.as_ref(), arch)
                .execute()?;

            // Use repository override if specified, otherwise use resolved repo
            let root_repo = pkg_spec
                .repository
                .clone()
                .unwrap_or_else(|| result.root.repository.clone());

            all_packages.insert(
                result.root.package.name.clone(),
                ResolvedPackage::from_debian_package(&result.root.package, root_repo, true),
            );

            // Add dependencies (repository comes directly from resolution)
            for dep in &result.dependencies {
                all_packages
                    .entry(dep.package.name.clone())
                    .or_insert_with(|| {
                        ResolvedPackage::from_debian_package(&dep.package, &dep.repository, false)
                    });
            }

            // Merge dependency graph (union of all per-root graphs)
            for (parent, mut children) in result.graph {
                let entry = all_graph.entry(parent).or_default();
                for child in children.drain(..) {
                    if !entry.contains(&child) {
                        entry.push(child);
                    }
                }
            }

            // Track unmet dependencies
            for unmet in result.unmet {
                all_unmet.push(format!(
                    "{} (required by {})",
                    unmet.name, unmet.required_by
                ));
            }
        }

        // Sort children in each adjacency list for determinism
        for children in all_graph.values_mut() {
            children.sort();
        }

        Ok(ResolvedManifest {
            packages: all_packages.into_values().collect(),
            unmet: all_unmet,
            graph: all_graph,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::debian::PackageIndex;

    #[test]
    fn test_resolve_service_creation() {
        let _service = ResolveService::new().expect("Should create service");
    }

    /// Helper: build a PackageIndexSet with a single package at a given version.
    fn make_index_set_with(name: &str, version: &str, arch: &str) -> PackageIndexSet {
        use crate::repository::debian::test_utils::make_package;

        let mut index = PackageIndex::new();
        index.add(make_package(name, version, arch));
        let mut set = PackageIndexSet::new();
        set.add_index("test-repo".to_string(), index);
        set
    }

    #[test]
    fn test_version_constraint_exact_match_accepted() {
        use crate::arch::Architecture;

        let set = make_index_set_with("bash", "5.2.15-2", "amd64");
        let constraint: VersionConstraint<DebianVersion> = "= 5.2.15-2".parse().unwrap();
        let result = set
            .resolve_all("bash", Some(&constraint), Architecture::Amd64)
            .execute();
        assert!(result.is_ok(), "exact version should resolve");
        assert_eq!(result.unwrap().root.package.name, "bash");
    }

    #[test]
    fn test_version_constraint_too_new_rejected() {
        use crate::arch::Architecture;

        let set = make_index_set_with("bash", "5.2.15-2", "amd64");
        // Require a version newer than what's available
        let constraint: VersionConstraint<DebianVersion> = ">> 5.2.15-2".parse().unwrap();
        let result = set
            .resolve_all("bash", Some(&constraint), Architecture::Amd64)
            .execute();
        // Should fail: no package satisfies >> 5.2.15-2
        assert!(result.is_err(), "newer-than constraint should fail");
    }

    #[test]
    fn test_version_constraint_gte_accepted() {
        use crate::arch::Architecture;

        let set = make_index_set_with("bash", "5.2.15-2", "amd64");
        let constraint: VersionConstraint<DebianVersion> = ">= 5.0".parse().unwrap();
        let result = set
            .resolve_all("bash", Some(&constraint), Architecture::Amd64)
            .execute();
        assert!(result.is_ok(), ">= older version should resolve");
    }

    #[test]
    fn test_invalid_version_string_returns_error() {
        // A version with an invalid epoch (non-numeric) must fail.
        let result = "= abc:1.0".parse::<VersionConstraint<DebianVersion>>();
        assert!(result.is_err(), "invalid epoch should fail to parse");
    }
}
