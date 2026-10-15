//! Download service for parallel package downloads.

use std::num::NonZeroUsize;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;

use crate::config::RepositoryConfig;
use crate::http::Client;
use crate::lock::{LockFile, LockedPackage};
use crate::report::{Event, Phase, Reporter};

use super::cache::PackageCache;
use super::error::{DownloadError, DownloadFailures, Fetched};

/// Download options.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum number of concurrent downloads.
    pub jobs: NonZeroUsize,
    /// Re-download even when a package is cached.
    pub force: bool,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            jobs: NonZeroUsize::new(4).unwrap(),
            force: false,
        }
    }
}

/// Downloads packages into a [`PackageCache`].
pub struct DownloadService {
    client: Client,
    cache: PackageCache,
    config: DownloadConfig,
}

impl DownloadService {
    /// Create a download service using the default cache directory.
    pub fn new(config: DownloadConfig) -> Result<Self, DownloadError> {
        Self::with_cache(PackageCache::new()?, config)
    }

    /// Create a download service with a specific cache.
    pub fn with_cache(cache: PackageCache, config: DownloadConfig) -> Result<Self, DownloadError> {
        Ok(Self {
            client: Client::new()?,
            cache,
            config,
        })
    }

    /// Make every package in the lock available in the cache.
    ///
    /// Results come back in lock order. All packages are attempted; if any
    /// fail, every failure is returned together.
    pub async fn download_all<'a>(
        &self,
        lock_file: &'a LockFile,
        repositories: &IndexMap<String, RepositoryConfig>,
        reporter: &dyn Reporter,
    ) -> Result<Vec<Fetched<'a>>, DownloadFailures> {
        let total_bytes = lock_file.packages.iter().map(|p| p.size).sum();
        reporter.report(Event::Begin {
            phase: Phase::Download,
            total: total_bytes,
        });

        let downloads = lock_file.packages.iter().map(|package| async move {
            let result = self
                .download(package, repositories.get(&package.repository))
                .await;
            match &result {
                Ok(_) => reporter.report(Event::Advance {
                    phase: Phase::Download,
                    units: package.size,
                    item: &package.name,
                }),
                Err(error) => reporter.report(Event::Failed {
                    phase: Phase::Download,
                    item: &package.name,
                    error,
                }),
            }
            result.map_err(|error| (package.name.clone(), error))
        });

        let results: Vec<_> = stream::iter(downloads)
            .buffered(self.config.jobs.get())
            .collect()
            .await;

        reporter.report(Event::End {
            phase: Phase::Download,
            summary: total_bytes,
        });

        let (fetched, failures): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
        if failures.is_empty() {
            Ok(fetched.into_iter().map(Result::unwrap).collect())
        } else {
            Err(DownloadFailures(
                failures.into_iter().map(Result::unwrap_err).collect(),
            ))
        }
    }

    /// Make one package available in the cache.
    async fn download<'a>(
        &self,
        package: &'a LockedPackage,
        repo_config: Option<&RepositoryConfig>,
    ) -> Result<Fetched<'a>, DownloadError> {
        if !self.config.force
            && let Some(path) = self.cache.get(package)
        {
            return Ok(Fetched {
                package,
                path,
                from_cache: true,
            });
        }

        let repo_config = repo_config
            .ok_or_else(|| DownloadError::UnknownRepository(package.repository.clone()))?;
        let url = Self::build_download_url(package, repo_config);
        let data = self.client.fetch_bytes(&url).await?;
        let path = self.cache.store(package, &data)?;

        Ok(Fetched {
            package,
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
        assert_eq!(config.jobs.get(), 4);
        assert!(!config.force);
    }

    #[test]
    fn test_build_download_url() {
        let package = make_test_package("bash", crate::hash::Sha256::of(b"bash"));
        let repo_config = RepositoryConfig {
            url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
            signed_by: "keys/debian.asc".into(),
        };

        let url = DownloadService::build_download_url(&package, &repo_config);
        assert_eq!(
            url,
            "https://deb.debian.org/debian/pool/main/b/bash_1.0.0_amd64.deb"
        );
    }

    #[test]
    fn test_build_download_url_trailing_slash() {
        let package = make_test_package("bash", crate::hash::Sha256::of(b"bash"));
        let repo_config = RepositoryConfig {
            url: url::Url::parse("https://deb.debian.org/debian/").unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
            signed_by: "keys/debian.asc".into(),
        };

        let url = DownloadService::build_download_url(&package, &repo_config);
        // Should not have double slashes
        assert!(!url.contains("//pool"));
    }
}
