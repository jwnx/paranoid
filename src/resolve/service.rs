//! Resolution service implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::arch::Architecture;
use crate::config::Manifest;
use crate::hash::{ParseSha256Error, Sha256};
use crate::http::{Client, ClientError};
use crate::repository::debian::{
    DebianPackage, DebianRepository, DebianRepositoryError, Fingerprint, Keyring, KeyringError,
    PackageIndexSet, Request, ResolveError, UnmetDependency, resolve,
};

/// Errors that can occur during resolution.
#[derive(Debug, Error)]
pub enum ResolveServiceError {
    #[error("client error: {0}")]
    Client(#[from] ClientError),

    #[error("repository '{name}': signed_by {}: {source}", path.display())]
    Keyring {
        name: String,
        path: PathBuf,
        #[source]
        source: Box<KeyringError>,
    },

    #[error("repository '{name}': {source}")]
    Repository {
        name: String,
        #[source]
        source: Box<DebianRepositoryError>,
    },

    #[error("resolve error: {0}")]
    Resolve(#[from] ResolveError),

    #[error("package '{package}' has an invalid SHA256 in the repository index")]
    InvalidChecksum {
        package: String,
        #[source]
        source: ParseSha256Error,
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

/// The signed snapshot of a repository that a resolution used. The lock
/// file records these as its `[[repository]]` tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRepository {
    pub name: String,
    /// The `Date` of its `Release`.
    pub date: DateTime<Utc>,
    /// SHA-256 of its `InRelease` file.
    pub inrelease: Sha256,
    /// Primary fingerprints of the keys whose signatures verified.
    pub signed_by: Vec<Fingerprint>,
}

/// The complete resolution result.
#[derive(Debug)]
pub struct ResolvedManifest {
    /// The repository snapshots, in manifest order.
    pub repositories: Vec<ResolvedRepository>,
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

        let now = Utc::now();

        // Build the index set from all repositories, each from a snapshot
        // signed by one of the keys the manifest names for it.
        let mut index_set = PackageIndexSet::new();
        let mut repositories = Vec::new();
        for (name, config) in &manifest.repositories {
            let path = manifest.resolve_path(&config.signed_by);
            let keyring = Keyring::load(&path).map_err(|source| ResolveServiceError::Keyring {
                name: name.clone(),
                path: config.signed_by.clone(),
                source: Box::new(source),
            })?;
            let repository_error = |source| ResolveServiceError::Repository {
                name: name.clone(),
                source: Box::new(source),
            };

            let repo = DebianRepository::new(&self.client, name, config, &keyring);
            let snapshot = repo.fetch_release(now).await.map_err(repository_error)?;
            let index = repo
                .fetch_index(&snapshot, arch)
                .await
                .map_err(repository_error)?;
            index_set.add_index(name.clone(), index);
            repositories.push(ResolvedRepository {
                name: name.clone(),
                date: snapshot.release.date,
                inrelease: snapshot.inrelease,
                signed_by: snapshot.signers,
            });
        }

        let requests: Vec<Request> = manifest
            .packages
            .iter()
            .map(|spec| Request {
                name: spec.name.clone(),
                version: spec.version.clone(),
                repository: spec.repository.clone(),
            })
            .collect();

        let resolution = resolve(&index_set, &requests, arch)?;
        let packages = resolution
            .packages
            .iter()
            .map(|r| ResolvedPackage::from_debian_package(r.package, r.repository, r.direct))
            .collect::<Result<_, _>>()?;

        Ok(ResolvedManifest {
            repositories,
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
}
