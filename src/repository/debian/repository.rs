//! Debian repository client.
//!
//! Handles fetching and parsing Debian repository metadata.

use thiserror::Error;

use crate::config::RepositoryConfig;
use crate::http::{Client, ClientError};

use super::Architecture;
use super::ParsePackageIndexError;
use super::package_index::PackageIndex;

/// Errors that can occur when interacting with a Debian repository.
#[derive(Debug, Error)]
pub enum DebianRepositoryError {
    #[error("HTTP error: {0}")]
    Http(#[from] ClientError),

    #[error("parse error: {0}")]
    Parse(#[from] ParsePackageIndexError),
}

/// Client for interacting with a Debian repository.
pub struct DebianRepository<'a> {
    client: &'a Client,
    config: &'a RepositoryConfig,
    name: String,
}

impl<'a> DebianRepository<'a> {
    /// Create a new Debian repository client.
    pub fn new(client: &'a Client, name: &str, config: &'a RepositoryConfig) -> Self {
        Self {
            client,
            config,
            name: name.to_string(),
        }
    }

    /// Get the repository name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fetch the InRelease or Release file.
    ///
    /// Tries InRelease first (signed), falls back to Release (unsigned).
    pub async fn fetch_release(&self) -> Result<String, DebianRepositoryError> {
        let base_url = self.base_url();
        let inrelease_url = format!("{}/dists/{}/InRelease", base_url, self.config.distribution);

        match self.client.fetch_text(&inrelease_url).await {
            Ok(content) => Ok(content),
            Err(ClientError::NotFound { .. }) => {
                // Fall back to unsigned Release file
                let release_url =
                    format!("{}/dists/{}/Release", base_url, self.config.distribution);
                self.client
                    .fetch_text(&release_url)
                    .await
                    .map_err(Into::into)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Get the base URL with trailing slash removed.
    fn base_url(&self) -> &str {
        self.config.url.as_str().trim_end_matches('/')
    }

    /// Fetch the Packages index for a specific component and architecture.
    ///
    /// Tries compressed versions first (.xz, .gz), falls back to uncompressed.
    pub async fn fetch_packages(
        &self,
        component: &str,
        architecture: Architecture,
    ) -> Result<String, DebianRepositoryError> {
        let base_path = format!(
            "{}/dists/{}/{}/binary-{}",
            self.base_url(),
            self.config.distribution,
            component,
            architecture
        );

        // Try xz first (most common for modern Debian)
        let xz_url = format!("{}/Packages.xz", base_path);
        match self.client.fetch_xz(&xz_url).await {
            Ok(content) => return Ok(content),
            Err(ClientError::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }

        // Try gzip
        let gz_url = format!("{}/Packages.gz", base_path);
        match self.client.fetch_gzip(&gz_url).await {
            Ok(content) => return Ok(content),
            Err(ClientError::NotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }

        // Fall back to uncompressed
        let url = format!("{}/Packages", base_path);
        self.client.fetch_text(&url).await.map_err(Into::into)
    }

    /// Fetch and parse the package index for all components.
    pub async fn fetch_index(
        &self,
        architecture: Architecture,
    ) -> Result<PackageIndex, DebianRepositoryError> {
        let mut index = PackageIndex::new();

        for component in &self.config.components {
            let packages_content = self.fetch_packages(component, architecture).await?;
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
        }
    }

    #[test]
    fn test_package_url() {
        let client = Client::new().expect("Should create client");
        let config = make_config();
        let repo = DebianRepository::new(&client, "debian", &config);

        let url = repo.package_url("pool/main/b/bash/bash_5.2.15-2_amd64.deb");
        assert_eq!(
            url,
            "https://deb.debian.org/debian/pool/main/b/bash/bash_5.2.15-2_amd64.deb"
        );
    }
}
