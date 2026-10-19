//! Download service for parallel package downloads.

use std::num::NonZeroUsize;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;

use crate::config::RepositoryConfig;
use crate::hash::Sha256;
use crate::http::{Client, Retry};
use crate::lock::LockedPackage;
use crate::report::{Event, Phase, Reporter};

use super::cache::PackageCache;
use super::error::{DownloadError, DownloadFailures, Fetched, SourceFailure, WrongBytes};

/// Download options.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum number of concurrent downloads.
    pub jobs: NonZeroUsize,
    /// Re-download even when a package is cached.
    pub force: bool,
    /// How hard to try each source before moving on to the next.
    pub retry: Retry,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            jobs: NonZeroUsize::new(4).unwrap(),
            force: false,
            retry: Retry::default(),
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

    /// Fetch a package from the first source that has it: its repository,
    /// then its mirrors.
    ///
    /// A source that's down, or doesn't have the package, isn't a reason to
    /// give up: a transient failure is retried, and then we move on to the
    /// next source. That's safe because whatever a source serves has to
    /// match the lock, and a source that serves anything else stops the
    /// download at once rather than being skipped.
    async fn fetch(
        &self,
        package: &LockedPackage,
        repo_config: &RepositoryConfig,
    ) -> Result<Vec<u8>, DownloadError> {
        let mut failures = Vec::new();
        for url in Self::download_urls(package, repo_config) {
            match self
                .config
                .retry
                .run(|| self.client.fetch_bytes(&url))
                .await
            {
                (Ok(data), _) => {
                    let actual = Sha256::of(&data);
                    if actual != package.sha256 {
                        return Err(DownloadError::WrongBytes(Box::new(WrongBytes {
                            url,
                            package: package.name.clone(),
                            expected: package.sha256,
                            actual,
                        })));
                    }
                    return Ok(data);
                }
                (Err(error), attempts) => failures.push(SourceFailure {
                    url,
                    error,
                    attempts,
                }),
            }
        }
        Err(DownloadError::Unavailable(failures))
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
    use crate::http::test_server::{Reply, TestServer};

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.jobs.get(), 4);
        assert!(!config.force);
        assert_eq!(config.retry.attempts.get(), 3);
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

    /// A service caching into `dir`, retrying without waiting.
    fn service(dir: &std::path::Path) -> DownloadService {
        let cache = PackageCache::with_base_dir(dir.to_path_buf()).unwrap();
        let config = DownloadConfig {
            retry: Retry {
                attempts: std::num::NonZeroU32::new(3).unwrap(),
                backoff: std::time::Duration::ZERO,
            },
            ..DownloadConfig::default()
        };
        DownloadService::with_cache(cache, config).unwrap()
    }

    const DEB: &[u8] = b"the real bash";

    fn bash() -> LockedPackage {
        let mut package = make_test_package("bash", Sha256::of(DEB));
        package.size = DEB.len() as u64;
        package
    }

    /// The pool path of `bash()` under a base URL.
    const POOL: &str = "/pool/main/b/bash_1.0.0_amd64.deb";

    /// A URL nothing listens on.
    async fn refused() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }

    async fn fetch(sources: &[&str]) -> (Result<Vec<u8>, DownloadError>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = repository(sources[0], None, &sources[1..]);
        let result = service(dir.path()).fetch(&bash(), &repo).await;
        (result, dir)
    }

    #[tokio::test]
    async fn moves_on_from_a_source_that_is_down() {
        let down = TestServer::start(|_| Reply::Status(503)).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let (result, _dir) = fetch(&[&down.url("/"), &refused().await, &up.url("/")]).await;
        assert_eq!(result.unwrap(), DEB);
        assert_eq!(down.requests(), vec![format!("GET {POOL}"); 3]);
        assert_eq!(up.requests(), [format!("GET {POOL}")]);
    }

    #[tokio::test]
    async fn moves_on_at_once_from_a_source_that_says_no() {
        let gone = TestServer::start(|_| Reply::Status(404)).await;
        let forbidden = TestServer::start(|_| Reply::Status(403)).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let (result, _dir) = fetch(&[&gone.url("/"), &forbidden.url("/"), &up.url("/")]).await;
        assert_eq!(result.unwrap(), DEB);
        assert_eq!(gone.requests().len(), 1);
        assert_eq!(forbidden.requests().len(), 1);
    }

    #[tokio::test]
    async fn never_moves_on_from_the_wrong_bytes() {
        let tampered = TestServer::start(|_| Reply::Body(b"evil bash".to_vec())).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let (result, _dir) = fetch(&[&tampered.url("/"), &up.url("/")]).await;
        let error = result.unwrap_err();
        assert!(
            error.to_string().starts_with(&format!(
                "{} served the wrong bytes for bash",
                tampered.url(POOL)
            )),
            "{error}"
        );
        assert!(up.requests().is_empty());
    }

    #[tokio::test]
    async fn says_what_every_source_answered() {
        let down = TestServer::start(|_| Reply::Status(502)).await;
        let gone = TestServer::start(|_| Reply::Status(404)).await;
        let refused = refused().await;
        let (result, _dir) = fetch(&[&down.url("/"), &gone.url("/"), &refused]).await;
        let error = result.unwrap_err().to_string();
        let lines: Vec<&str> = error.lines().collect();
        assert_eq!(lines[0], "no source could serve it:");
        assert_eq!(
            lines[1],
            format!("  {}: 502 Bad Gateway (3 attempts)", down.url(POOL))
        );
        assert_eq!(lines[2], format!("  {}: 404 Not Found", gone.url(POOL)));
        assert!(
            lines[3].starts_with(&format!("  {refused}{POOL}: "))
                && lines[3].ends_with("(3 attempts)"),
            "{}",
            lines[3]
        );
        assert_eq!(lines.len(), 4);
    }

    #[tokio::test]
    async fn caches_what_it_fetched() {
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let dir = tempfile::TempDir::new().unwrap();
        let service = service(dir.path());
        let repo = repository(&up.url("/"), None, &[]);
        let package = bash();

        let first = service.download(&package, Some(&repo)).await.unwrap();
        assert!(!first.from_cache);
        let second = service.download(&package, Some(&repo)).await.unwrap();
        assert!(second.from_cache);
        assert_eq!(std::fs::read(second.path).unwrap(), DEB);
        assert_eq!(up.requests().len(), 1);
    }
}
