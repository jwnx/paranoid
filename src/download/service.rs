//! Download service for parallel package downloads.

use std::num::NonZeroUsize;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;

use crate::config::RepositoryConfig;
use crate::http::Client;
use crate::lock::{LockFile, LockedPackage};

use super::cache::PackageCache;
use super::error::{DownloadError, DownloadFailure, DownloadResult, DownloadedPackage};
use super::progress::DownloadProgress;

/// Configuration for the download service.
///
/// Use the builder methods for ergonomic construction:
/// ```
/// use std::num::NonZeroUsize;
/// use paranoid::download::DownloadConfig;
///
/// let config = DownloadConfig::new()
///     .with_max_concurrent(NonZeroUsize::new(8).unwrap())
///     .with_force_download(true);
/// ```
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum number of concurrent downloads.
    max_concurrent: NonZeroUsize,
    /// Force re-download even if cached.
    force_download: bool,
}

impl DownloadConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of concurrent downloads.
    pub fn with_max_concurrent(mut self, n: NonZeroUsize) -> Self {
        self.max_concurrent = n;
        self
    }

    /// Set whether to force re-download even if cached.
    pub fn with_force_download(mut self, force: bool) -> Self {
        self.force_download = force;
        self
    }

    /// Get the maximum number of concurrent downloads.
    #[must_use]
    pub fn max_concurrent(&self) -> NonZeroUsize {
        self.max_concurrent
    }

    /// Get whether to force re-download even if cached.
    #[must_use]
    pub fn force_download(&self) -> bool {
        self.force_download
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_concurrent: NonZeroUsize::new(4).unwrap(),
            force_download: false,
        }
    }
}

/// Service for downloading packages in parallel.
pub struct DownloadService {
    client: Client,
    cache: PackageCache,
    config: DownloadConfig,
}

impl DownloadService {
    /// Create a new download service with default configuration.
    pub fn new() -> Result<Self, DownloadError> {
        Self::with_config(DownloadConfig::default())
    }

    /// Create a new download service with custom configuration.
    pub fn with_config(config: DownloadConfig) -> Result<Self, DownloadError> {
        let client = Client::new()?;
        let cache = PackageCache::new()?;
        Ok(Self {
            client,
            cache,
            config,
        })
    }

    /// Create a new download service with custom cache directory.
    pub fn with_cache(cache: PackageCache, config: DownloadConfig) -> Result<Self, DownloadError> {
        let client = Client::new()?;
        Ok(Self {
            client,
            cache,
            config,
        })
    }

    /// Download all packages from a lock file.
    ///
    /// Uses bounded parallelism and reports progress via the provided reporter.
    #[must_use = "download results should be checked for failures"]
    pub async fn download_all<P: DownloadProgress>(
        &self,
        lock_file: &LockFile,
        repositories: &IndexMap<String, RepositoryConfig>,
        progress: &P,
    ) -> DownloadResult {
        let total_bytes: u64 = lock_file.packages.iter().map(|p| p.size).sum();
        progress.on_start(lock_file.packages.len(), total_bytes);

        // Create download tasks
        let download_futures = lock_file.packages.iter().map(|package| {
            let repo_config = repositories.get(&package.repository);
            self.download_package(package, repo_config, progress)
        });

        // Execute with bounded concurrency
        let results: Vec<Result<DownloadedPackage, DownloadFailure>> =
            stream::iter(download_futures)
                .buffer_unordered(self.config.max_concurrent().get())
                .collect()
                .await;

        // Partition results
        let mut download_result = DownloadResult::new();
        for result in results {
            match result {
                Ok(pkg) => download_result.succeeded.push(pkg),
                Err(failure) => download_result.failed.push(failure),
            }
        }

        progress.on_complete(
            download_result.succeeded.len(),
            download_result.failed.len(),
        );
        download_result
    }

    /// Download a single package.
    async fn download_package<P: DownloadProgress>(
        &self,
        package: &LockedPackage,
        repo_config: Option<&RepositoryConfig>,
        progress: &P,
    ) -> Result<DownloadedPackage, DownloadFailure> {
        progress.on_package_start(package);

        let result = self.download_package_inner(package, repo_config).await;

        match &result {
            Ok(downloaded) => {
                progress.on_package_complete(package, downloaded.from_cache);
            }
            Err(error) => {
                progress.on_package_error(package, error);
            }
        }

        result.map_err(|error| DownloadFailure {
            name: package.name.clone(),
            version: package.version.clone(),
            error,
        })
    }

    /// Inner download logic without progress reporting.
    async fn download_package_inner(
        &self,
        package: &LockedPackage,
        repo_config: Option<&RepositoryConfig>,
    ) -> Result<DownloadedPackage, DownloadError> {
        // Check cache first (unless force_download is set)
        if !self.config.force_download()
            && let Some(cached_path) = self.cache.get(package)
        {
            return Ok(DownloadedPackage {
                name: package.name.clone(),
                version: package.version.clone(),
                path: cached_path,
                from_cache: true,
            });
        }

        // Get repository config (required)
        let repo_config = repo_config
            .ok_or_else(|| DownloadError::UnknownRepository(package.repository.clone()))?;

        // Build download URL
        let url = Self::build_download_url(package, repo_config);

        // Download the package
        let data = self.client.fetch_bytes(&url).await?;

        // Store in cache (which verifies checksum)
        let path = self.cache.store(package, &data)?;

        Ok(DownloadedPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            path,
            from_cache: false,
        })
    }

    /// Build the download URL for a package.
    fn build_download_url(package: &LockedPackage, repo_config: &RepositoryConfig) -> String {
        let base_url = repo_config.url.as_str().trim_end_matches('/');
        format!("{}/{}", base_url, package.filename)
    }

    /// Get a reference to the cache.
    pub fn cache(&self) -> &PackageCache {
        &self.cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::test_utils::make_test_package;

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.max_concurrent().get(), 4);
        assert!(!config.force_download());
    }

    #[test]
    fn test_download_config_builder() {
        let config = DownloadConfig::new()
            .with_max_concurrent(NonZeroUsize::new(8).unwrap())
            .with_force_download(true);
        assert_eq!(config.max_concurrent().get(), 8);
        assert!(config.force_download());
    }

    #[test]
    fn test_build_download_url() {
        let package = make_test_package("bash", "abc123");
        let repo_config = RepositoryConfig {
            url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
        };

        let url = DownloadService::build_download_url(&package, &repo_config);
        assert_eq!(
            url,
            "https://deb.debian.org/debian/pool/main/b/bash_1.0.0_amd64.deb"
        );
    }

    #[test]
    fn test_build_download_url_trailing_slash() {
        let package = make_test_package("bash", "abc123");
        let repo_config = RepositoryConfig {
            url: url::Url::parse("https://deb.debian.org/debian/").unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
        };

        let url = DownloadService::build_download_url(&package, &repo_config);
        // Should not have double slashes
        assert!(!url.contains("//pool"));
    }
}
