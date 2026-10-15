//! Resolution service implementation.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::arch::Architecture;
use crate::config::Manifest;
use crate::hash::{ParseSha256Error, Sha256};
use crate::http::{Client, ClientError};
use crate::repository::debian::{
    DebianPackage, DebianRepository, DebianRepositoryError, DebianVersion, PackageIndexSet,
    Request, ResolveError, UnmetDependency, resolve,
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

    #[error("package '{package}' has an invalid SHA256 in the repository index")]
    InvalidChecksum {
        package: String,
        #[source]
        source: ParseSha256Error,
    },

    #[error("invalid version constraint for package '{package}': {source}")]
    InvalidVersionConstraint {
        package: String,
        #[source]
        source:
            crate::version::ParseVersionConstraintError<<DebianVersion as std::str::FromStr>::Err>,
    },
}

/// A resolved package with all metadata needed for the lock file.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: String,
    pub architecture: Architecture,
    pub sha256: Sha256,
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
    ///
    /// This is where a package's checksum leaves the repository index and
    /// heads for the lock file, so we parse it here.
    pub fn from_debian_package(
        pkg: &DebianPackage,
        repository: impl Into<String>,
        direct: bool,
    ) -> Result<Self, ResolveServiceError> {
        let sha256 = pkg
            .sha256
            .parse()
            .map_err(|source| ResolveServiceError::InvalidChecksum {
                package: pkg.name.clone(),
                source,
            })?;
        Ok(Self {
            name: pkg.name.clone(),
            version: pkg.version.to_string(),
            architecture: pkg.architecture.unwrap_or(Architecture::All),
            sha256,
            filename: pkg.filename.clone(),
            size: pkg.size,
            repository: repository.into(),
            direct,
        })
    }
}

/// The complete resolution result.
#[derive(Debug)]
pub struct ResolvedManifest {
    /// All resolved packages (including transitive dependencies).
    pub packages: Vec<ResolvedPackage>,
    /// Dependencies nothing in the repositories satisfies.
    pub unmet: Vec<UnmetDependency>,
    /// Dependency graph: parent name → sorted list of child names.
    pub graph: BTreeMap<String, Vec<String>>,
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
        // Repository presence, pins and architecture were validated when
        // the manifest was parsed.
        let arch = manifest
            .image
            .as_ref()
            .map_or(Architecture::Amd64, |i| i.architecture);

        // Build the index set from all repositories
        let mut index_set = PackageIndexSet::new();
        for (repo_name, repo_config) in &manifest.repositories {
            let repo = DebianRepository::new(&self.client, repo_name, repo_config);
            let index = repo.fetch_index(arch).await?;
            index_set.add_index(repo_name.clone(), index);
        }

        let requests = manifest
            .packages
            .iter()
            .map(|spec| {
                let mut request = Request::new(&spec.name);
                request.repository = spec.repository.clone();
                if let Some(version) = &spec.version {
                    let constraint = version
                        .parse::<VersionConstraint<DebianVersion>>()
                        .map_err(|source| ResolveServiceError::InvalidVersionConstraint {
                            package: spec.name.clone(),
                            source,
                        })?;
                    request = request.version(constraint);
                }
                Ok(request)
            })
            .collect::<Result<Vec<_>, ResolveServiceError>>()?;

        let resolution = resolve(&index_set, &requests, arch)?;
        let packages = resolution
            .packages
            .iter()
            .map(|r| ResolvedPackage::from_debian_package(r.package, r.repository, r.direct))
            .collect::<Result<_, _>>()?;

        Ok(ResolvedManifest {
            packages,
            unmet: resolution.unmet,
            graph: resolution.graph,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_service_creation() {
        let _service = ResolveService::new().expect("Should create service");
    }

    #[test]
    fn test_invalid_version_string_returns_error() {
        // A version with an invalid epoch (non-numeric) must fail.
        let result = "= abc:1.0".parse::<VersionConstraint<DebianVersion>>();
        assert!(result.is_err(), "invalid epoch should fail to parse");
    }
}
