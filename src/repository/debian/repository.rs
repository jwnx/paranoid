//! Debian repository client.
//!
//! Handles fetching and parsing Debian repository metadata.

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::config::RepositoryConfig;
use crate::hash::Sha256;
use crate::http::{Client, ClientError};

use super::Architecture;
use super::ParsePackageIndexError;
use super::keyring::{Fingerprint, Keyring, VerifyError};
use super::package_index::PackageIndex;
use super::release::{IndexFile, ParseReleaseError, Release, ReleaseError};

/// Errors that can occur when interacting with a Debian repository.
#[derive(Debug, Error)]
pub enum DebianRepositoryError {
    #[error("HTTP error: {0}")]
    Http(#[from] ClientError),

    #[error("{url} not found: the repository isn't signed, and paranoid only reads signed ones")]
    Unsigned { url: String },

    #[error("{url}: {source}")]
    Signature {
        url: String,
        #[source]
        source: VerifyError,
    },

    #[error("{url}: {source}")]
    ParseRelease {
        url: String,
        #[source]
        source: ParseReleaseError,
    },

    #[error("{url}: {source}")]
    Release {
        url: String,
        #[source]
        source: ReleaseError,
    },

    #[error(
        "{url}: expected {expected_size} bytes with SHA-256 {expected}, got {size} bytes with SHA-256 {actual}"
    )]
    Checksum {
        url: String,
        expected: Sha256,
        expected_size: u64,
        actual: Sha256,
        size: u64,
    },

    #[error("parse error: {0}")]
    Parse(#[from] ParsePackageIndexError),
}

/// A repository's `InRelease`, verified and parsed: one signed snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub release: Release,
    /// SHA-256 of the `InRelease` file as served, which names the snapshot
    /// (snapshot.debian.org and friends can look it up).
    pub inrelease: Sha256,
    /// Primary fingerprints of the keys with a good signature.
    pub signers: Vec<Fingerprint>,
}

/// Client for interacting with a Debian repository.
pub struct DebianRepository<'a> {
    client: &'a Client,
    config: &'a RepositoryConfig,
    keyring: &'a Keyring,
    name: String,
}

impl<'a> DebianRepository<'a> {
    /// Create a new Debian repository client. Everything it reads must be
    /// signed by a key in `keyring`.
    pub fn new(
        client: &'a Client,
        name: &str,
        config: &'a RepositoryConfig,
        keyring: &'a Keyring,
    ) -> Self {
        Self {
            client,
            config,
            keyring,
            name: name.to_string(),
        }
    }

    /// Get the repository name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fetch `InRelease`, verify its signature and check that it describes
    /// the configured distribution and is still valid at `now`.
    ///
    /// There is no fallback to an unsigned `Release`.
    pub async fn fetch_release(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Snapshot, DebianRepositoryError> {
        let url = self.dists_url("InRelease");
        let bytes = match self.client.fetch_bytes(&url).await {
            Ok(bytes) => bytes,
            Err(ClientError::NotFound { .. }) => {
                return Err(DebianRepositoryError::Unsigned { url });
            }
            Err(e) => return Err(e.into()),
        };
        let inrelease = Sha256::of(&bytes);

        let signature = |source| DebianRepositoryError::Signature {
            url: url.clone(),
            source,
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|e| signature(VerifyError::Malformed(e.to_string())))?;
        let verified = self
            .keyring
            .verify_cleartext(text, now)
            .map_err(signature)?;

        let release: Release =
            verified
                .text
                .parse()
                .map_err(|source| DebianRepositoryError::ParseRelease {
                    url: url.clone(),
                    source,
                })?;
        release
            .check(&self.config.distribution, now)
            .map_err(|source| DebianRepositoryError::Release {
                url: url.clone(),
                source,
            })?;

        Ok(Snapshot {
            release,
            inrelease,
            signers: verified.signers,
        })
    }

    /// `<url>/dists/<distribution>/<path>`.
    fn dists_url(&self, path: &str) -> String {
        format!(
            "{}/dists/{}/{}",
            self.base_url(),
            self.config.distribution,
            path
        )
    }

    /// Get the base URL with trailing slash removed.
    fn base_url(&self) -> &str {
        self.config.url.as_str().trim_end_matches('/')
    }

    /// Fetch a file listed in the snapshot and check it against the listing.
    ///
    /// With `Acquire-By-Hash` we ask for the file by its checksum first, so
    /// a mirror updating underneath us can't hand us the next snapshot's
    /// index. Either way, what we keep is what the signed Release lists.
    async fn fetch_listed(
        &self,
        snapshot: &Snapshot,
        path: &str,
        file: IndexFile,
    ) -> Result<Vec<u8>, DebianRepositoryError> {
        let url = self.dists_url(path);
        let by_hash = snapshot.release.acquire_by_hash.then(|| {
            let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            self.dists_url(&format!("{dir}/by-hash/SHA256/{}", file.sha256))
        });

        let (url, bytes) = match by_hash {
            Some(hashed) => match self.client.fetch_bytes(&hashed).await {
                Ok(bytes) => (hashed, bytes),
                Err(ClientError::NotFound { .. }) => {
                    let bytes = self.client.fetch_bytes(&url).await?;
                    (url, bytes)
                }
                Err(e) => return Err(e.into()),
            },
            None => {
                let bytes = self.client.fetch_bytes(&url).await?;
                (url, bytes)
            }
        };

        let actual = Sha256::of(&bytes);
        let size = bytes.len() as u64;
        if actual != file.sha256 || size != file.size {
            return Err(DebianRepositoryError::Checksum {
                url,
                expected: file.sha256,
                expected_size: file.size,
                actual,
                size,
            });
        }
        Ok(bytes)
    }

    /// Fetch the `Packages` index for a component and architecture, checked
    /// against the snapshot.
    pub async fn fetch_packages(
        &self,
        snapshot: &Snapshot,
        component: &str,
        architecture: Architecture,
    ) -> Result<String, DebianRepositoryError> {
        let (path, file) = snapshot
            .release
            .packages_index(component, architecture.as_str())
            .map_err(|source| DebianRepositoryError::Release {
                url: self.dists_url("InRelease"),
                source,
            })?;
        let bytes = self.fetch_listed(snapshot, path, file).await?;

        let text = if path.ends_with(".xz") {
            Client::decompress_xz(&bytes)?
        } else if path.ends_with(".gz") {
            Client::decompress_gzip(&bytes)?
        } else {
            String::from_utf8(bytes)
                .map_err(|e| ClientError::Decompress(std::io::Error::other(e)))?
        };
        Ok(text)
    }

    /// Fetch and parse the package index for all components.
    pub async fn fetch_index(
        &self,
        snapshot: &Snapshot,
        architecture: Architecture,
    ) -> Result<PackageIndex, DebianRepositoryError> {
        let mut index = PackageIndex::new();

        for component in &self.config.components {
            let packages_content = self
                .fetch_packages(snapshot, component, architecture)
                .await?;
            let component_index: PackageIndex = packages_content.parse()?;

            for pkg in component_index.packages() {
                index.add(pkg.clone());
            }
        }

        Ok(index)
    }

    /// Construct the full download URL for a package.
    pub fn package_url(&self, filename: &str) -> String {
        format!("{}/{}", self.base_url(), filename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> RepositoryConfig {
        RepositoryConfig {
            url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
            signed_by: "keys/debian.asc".into(),
        }
    }

    #[test]
    fn test_package_url() {
        let client = Client::new().expect("Should create client");
        let config = make_config();
        let keyring = Keyring::from_bytes(include_bytes!(
            "testdata/debian-archive-bookworm-automatic.asc"
        ))
        .unwrap();
        let repo = DebianRepository::new(&client, "debian", &config, &keyring);

        let url = repo.package_url("pool/main/b/bash/bash_5.2.15-2_amd64.deb");
        assert_eq!(
            url,
            "https://deb.debian.org/debian/pool/main/b/bash/bash_5.2.15-2_amd64.deb"
        );
    }
}
