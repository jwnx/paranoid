//! Download service for parallel package downloads.

use std::num::NonZeroUsize;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;

use crate::config::RepositoryConfig;
use crate::http::{Client, ClientError};
use crate::lock::LockedPackage;
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
        packages: &[&'a LockedPackage],
        repositories: &IndexMap<String, RepositoryConfig>,
        reporter: &dyn Reporter,
    ) -> Result<Vec<Fetched<'a>>, DownloadFailures> {
        let total_bytes = packages.iter().map(|p| p.size).sum();
        reporter.report(Event::Begin {
            phase: Phase::Download,
            total: total_bytes,
        });

        let downloads = packages.iter().map(|&package| async move {
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
        let data = self.fetch(package, repo_config).await?;
        let path = self.cache.store(package, &data)?;

        Ok(Fetched {
            package,
            path,
            from_cache: false,
        })
    }

    /// Fetch a package from its repository, or from the first of its
    /// mirrors that still has it. Only a 404 moves on to the next one: any
    /// other failure is a real error, and whatever we get is checked
    /// against the lock either way.
    async fn fetch(
        &self,
        package: &LockedPackage,
        repo_config: &RepositoryConfig,
    ) -> Result<Vec<u8>, DownloadError> {
        let mut urls = Self::download_urls(package, repo_config)
            .into_iter()
            .peekable();
        while let Some(url) = urls.next() {
            match self.client.fetch_bytes(&url).await {
                Err(ClientError::NotFound { .. }) if urls.peek().is_some() => continue,
                result => return Ok(result?),
            }
        }
        unreachable!("a repository always has a URL")
    }

    /// Where a package can be downloaded from, in the order to try.
    fn download_urls(package: &LockedPackage, repo_config: &RepositoryConfig) -> Vec<String> {
        let mirrors = repo_config
            .mirrors
            .iter()
            .map(|m| m.as_str().trim_end_matches('/').to_string());
        std::iter::once(repo_config.base_url())
            .chain(mirrors)
            .map(|base| format!("{base}/{}", package.filename))
            .collect()
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

    fn repository(url: &str, snapshot: Option<&str>, mirrors: &[&str]) -> RepositoryConfig {
        RepositoryConfig {
            url: url::Url::parse(url).unwrap(),
            distribution: "bookworm".to_string(),
            components: vec!["main".to_string()],
            signed_by: "keys/debian.asc".into(),
            snapshot: snapshot.map(|s| s.parse().unwrap()),
            priority: None,
            mirrors: mirrors
                .iter()
                .map(|m| url::Url::parse(m).unwrap())
                .collect(),
        }
    }

    fn urls(repo: &RepositoryConfig) -> Vec<String> {
        let package = make_test_package("bash", crate::hash::Sha256::of(b"bash"));
        DownloadService::download_urls(&package, repo)
    }

    #[test]
    fn downloads_from_the_repository() {
        let expected = ["https://deb.debian.org/debian/pool/main/b/bash_1.0.0_amd64.deb"];
        assert_eq!(
            urls(&repository("https://deb.debian.org/debian", None, &[])),
            expected
        );
        assert_eq!(
            urls(&repository("https://deb.debian.org/debian/", None, &[])),
            expected
        );
    }

    #[test]
    fn downloads_from_the_snapshot() {
        let repo = repository(
            "https://snapshot.debian.org/archive/debian/",
            Some("20261005T000000Z"),
            &[],
        );
        assert_eq!(
            urls(&repo),
            [
                "https://snapshot.debian.org/archive/debian/20261005T000000Z/pool/main/b/bash_1.0.0_amd64.deb"
            ]
        );
    }

    #[test]
    fn mirrors_are_tried_after_the_repository_in_order() {
        let repo = repository(
            "https://apt.postgresql.org/pub/repos/apt",
            None,
            &[
                "https://apt-archive.postgresql.org/pub/repos/apt/",
                "https://example.org/",
            ],
        );
        assert_eq!(
            urls(&repo),
            [
                "https://apt.postgresql.org/pub/repos/apt/pool/main/b/bash_1.0.0_amd64.deb",
                "https://apt-archive.postgresql.org/pub/repos/apt/pool/main/b/bash_1.0.0_amd64.deb",
                "https://example.org/pool/main/b/bash_1.0.0_amd64.deb",
            ]
        );
    }
}
