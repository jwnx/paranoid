//! Download service for parallel package downloads.

use std::collections::HashSet;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Mutex;

use futures::stream::{self, StreamExt};
use indexmap::IndexMap;

use url::Url;

use crate::config::{Manifest, RepositoryConfig};
use crate::hash::Sha256;
use crate::http::{Client, ClientError, Retry};
use crate::lock::LockedPackage;
use crate::report::{Event, Phase, Reporter};

use super::cache::{PackageCache, content_path};
use super::error::{
    DownloadError, DownloadFailures, Fetched, Mismatch, Origin, SourceFailure, WrongBytes,
};

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

/// Where packages can come from.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// Content-addressed stores, tried first, in order.
    pub stores: &'a [Url],
    /// Each package's repository, by name, with its mirrors.
    pub repositories: &'a IndexMap<String, RepositoryConfig>,
}

impl<'a> Sources<'a> {
    /// The stores and repositories of a manifest.
    pub fn of(manifest: &'a Manifest) -> Self {
        Self {
            stores: &manifest.stores,
            repositories: &manifest.repositories,
        }
    }
}

/// One place to fetch one package from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    /// The store, repository or mirror, by its base URL.
    base: String,
    /// The package under it.
    url: String,
    origin: Origin,
}

/// Downloads packages into a [`PackageCache`].
pub struct DownloadService {
    client: Client,
    cache: PackageCache,
    config: DownloadConfig,
    /// Sources that failed a package for a transient reason, retries and
    /// all. Later packages ask them last, and only once, so a store that's
    /// down costs the packages in flight their retries, not every package.
    down: Mutex<HashSet<String>>,
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
            down: Mutex::default(),
        })
    }

    /// Make every package in the lock available in the cache.
    ///
    /// Results come back in lock order. All packages are attempted; if any
    /// fail, every failure is returned together.
    pub async fn download_all<'a>(
        &self,
        packages: &[&'a LockedPackage],
        sources: Sources<'_>,
        reporter: &dyn Reporter,
    ) -> Result<Vec<Fetched<'a>>, DownloadFailures> {
        let total_bytes = packages.iter().map(|p| p.size).sum();
        reporter.report(Event::Begin {
            phase: Phase::Download,
            total: total_bytes,
        });

        let downloads = packages.iter().map(|&package| async move {
            let result = self.download(package, sources).await;
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
        sources: Sources<'_>,
    ) -> Result<Fetched<'a>, DownloadError> {
        if !self.config.force
            && let Some(path) = self.cache.get(package)
        {
            return Ok(Fetched {
                package,
                path,
                origin: Origin::Cache,
            });
        }

        let (data, origin) = self.fetch(package, sources).await?;
        let path = self.cache.store(package, &data)?;

        Ok(Fetched {
            package,
            path,
            origin,
        })
    }

    /// Fetch a package from the first source that has it: the stores, its
    /// repository, then the repository's mirrors.
    ///
    /// A source that's down, or doesn't have the package, isn't a reason to
    /// give up: a transient failure is retried, and then we move on to the
    /// next source. That's safe because whatever a source serves has to
    /// match the lock, and a source that serves anything else stops the
    /// download at once rather than being skipped.
    async fn fetch(
        &self,
        package: &LockedPackage,
        sources: Sources<'_>,
    ) -> Result<(Vec<u8>, Origin), DownloadError> {
        // Sources that were down for an earlier package go last, with one
        // attempt each, so skipping them can reorder the sources but never
        // fail a package that one of them would have served.
        let (up, down): (Vec<_>, Vec<_>) = {
            let down = self.down.lock().unwrap();
            Self::download_urls(package, sources)?
                .into_iter()
                .partition(|source| !down.contains(&source.base))
        };
        let once = match up.is_empty() {
            // Nowhere else to go: they get their retries as usual.
            true => self.config.retry,
            false => Retry {
                attempts: NonZeroU32::MIN,
                ..self.config.retry
            },
        };
        let attempts = up
            .into_iter()
            .map(|source| (source, self.config.retry))
            .chain(down.into_iter().map(|source| (source, once)));

        let mut failures = Vec::new();
        for (Source { base, url, origin }, retry) in attempts {
            let wrong = |url, mismatch| {
                DownloadError::WrongBytes(Box::new(WrongBytes {
                    url,
                    package: package.name.clone(),
                    mismatch,
                }))
            };
            match retry
                .run(|| self.client.fetch_bytes_limited(&url, package.size))
                .await
            {
                (Ok(data), _) => {
                    let actual = Sha256::of(&data);
                    if actual != package.sha256 {
                        let expected = package.sha256;
                        return Err(wrong(url, Mismatch::Hash { expected, actual }));
                    }
                    self.down.lock().unwrap().remove(&base);
                    return Ok((data, origin));
                }
                (Err(ClientError::TooLarge { .. }), _) => {
                    let size = package.size;
                    return Err(wrong(url, Mismatch::TooLarge { size }));
                }
                (Err(error), attempts) => {
                    if error.is_transient() {
                        self.down.lock().unwrap().insert(base);
                    }
                    failures.push(SourceFailure {
                        url,
                        error,
                        attempts,
                    });
                }
            }
        }
        Err(DownloadError::Unavailable(failures))
    }

    /// Where a package can be downloaded from, in the order to try.
    ///
    /// The stores come first. Any of them may be wrong, and so may the
    /// repository; it's the lock's hash that decides, so asking a store
    /// first costs nothing in safety, and keeps an outage upstream away
    /// from every package a store has.
    fn download_urls(
        package: &LockedPackage,
        sources: Sources<'_>,
    ) -> Result<Vec<Source>, DownloadError> {
        let repo_config = sources
            .repositories
            .get(&package.repository)
            .ok_or_else(|| DownloadError::UnknownRepository(package.repository.clone()))?;
        let base = |url: &Url| url.as_str().trim_end_matches('/').to_string();
        let stores = sources.stores.iter().map(|store| {
            let base = base(store);
            Source {
                url: format!("{base}/{}", content_path(&package.sha256)),
                base,
                origin: Origin::Store,
            }
        });
        let repository = std::iter::once(repo_config.base_url())
            .chain(repo_config.mirrors.iter().map(base))
            .map(|base| Source {
                url: format!("{base}/{}", package.filename),
                base,
                origin: Origin::Repository,
            });
        Ok(stores.chain(repository).collect())
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

    /// Where `bash` would be fetched from, with these stores and its
    /// repository `test`.
    fn urls_with(stores: &[&str], repo: &RepositoryConfig) -> Vec<String> {
        let package = make_test_package("bash", Sha256::of(b"bash"));
        let stores: Vec<Url> = stores.iter().map(|s| s.parse().unwrap()).collect();
        let repositories = IndexMap::from([("test".to_string(), repo.clone())]);
        let sources = Sources {
            stores: &stores,
            repositories: &repositories,
        };
        DownloadService::download_urls(&package, sources)
            .unwrap()
            .into_iter()
            .map(|source| source.url)
            .collect()
    }

    fn urls(repo: &RepositoryConfig) -> Vec<String> {
        urls_with(&[], repo)
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

    #[test]
    fn stores_are_tried_first_by_hash() {
        let repo = repository(
            "https://deb.debian.org/debian",
            None,
            &["https://example.org"],
        );
        let hash = Sha256::of(b"bash").to_string();
        assert_eq!(
            urls_with(
                &["https://bucket.example.org/debs/", "http://cache.lan"],
                &repo
            ),
            [
                format!(
                    "https://bucket.example.org/debs/sha256/{}/{hash}.deb",
                    &hash[..2]
                ),
                format!("http://cache.lan/sha256/{}/{hash}.deb", &hash[..2]),
                "https://deb.debian.org/debian/pool/main/b/bash_1.0.0_amd64.deb".to_string(),
                "https://example.org/pool/main/b/bash_1.0.0_amd64.deb".to_string(),
            ]
        );
    }

    #[test]
    fn a_package_needs_its_repository_even_with_a_store() {
        let package = make_test_package("bash", Sha256::of(b"bash"));
        let stores = ["https://bucket.example.org/debs".parse().unwrap()];
        let sources = Sources {
            stores: &stores,
            repositories: &IndexMap::new(),
        };
        assert!(matches!(
            DownloadService::download_urls(&package, sources),
            Err(DownloadError::UnknownRepository(_))
        ));
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

    /// The store path of `bash()`.
    fn stored() -> String {
        format!("/{}", content_path(&Sha256::of(DEB)))
    }

    /// Fetch `bash()` from these stores, then a repository and its mirrors.
    async fn fetch_from(
        stores: &[&str],
        repository_and_mirrors: &[&str],
    ) -> Result<(Vec<u8>, Origin), DownloadError> {
        let dir = tempfile::TempDir::new().unwrap();
        let [url, mirrors @ ..] = repository_and_mirrors else {
            panic!("no repository");
        };
        let repositories = IndexMap::from([("test".to_string(), repository(url, None, mirrors))]);
        let stores: Vec<Url> = stores.iter().map(|s| s.parse().unwrap()).collect();
        let sources = Sources {
            stores: &stores,
            repositories: &repositories,
        };
        service(dir.path()).fetch(&bash(), sources).await
    }

    async fn fetch(repository_and_mirrors: &[&str]) -> Result<Vec<u8>, DownloadError> {
        fetch_from(&[], repository_and_mirrors)
            .await
            .map(|(data, _)| data)
    }

    #[tokio::test]
    async fn moves_on_from_a_source_that_is_down() {
        let down = TestServer::start(|_| Reply::Status(503)).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let result = fetch(&[&down.url("/"), &refused().await, &up.url("/")]).await;
        assert_eq!(result.unwrap(), DEB);
        assert_eq!(down.requests(), vec![format!("GET {POOL}"); 3]);
        assert_eq!(up.requests(), [format!("GET {POOL}")]);
    }

    #[tokio::test]
    async fn moves_on_at_once_from_a_source_that_says_no() {
        let gone = TestServer::start(|_| Reply::Status(404)).await;
        let forbidden = TestServer::start(|_| Reply::Status(403)).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let result = fetch(&[&gone.url("/"), &forbidden.url("/"), &up.url("/")]).await;
        assert_eq!(result.unwrap(), DEB);
        assert_eq!(gone.requests().len(), 1);
        assert_eq!(forbidden.requests().len(), 1);
    }

    #[tokio::test]
    async fn never_moves_on_from_the_wrong_bytes() {
        let tampered = TestServer::start(|_| Reply::Body(b"evil bash".to_vec())).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let result = fetch(&[&tampered.url("/"), &up.url("/")]).await;
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
    async fn never_reads_more_than_the_lock_says() {
        let endless = TestServer::start(|_| Reply::Body(vec![b'x'; 1 << 20])).await;
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let error = fetch(&[&endless.url("/"), &up.url("/")]).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} served the wrong bytes for bash: more than the {} bytes in the lock",
                endless.url(POOL),
                DEB.len()
            )
        );
        assert!(up.requests().is_empty());
    }

    #[tokio::test]
    async fn says_what_every_source_answered() {
        let down = TestServer::start(|_| Reply::Status(502)).await;
        let gone = TestServer::start(|_| Reply::Status(404)).await;
        let refused = refused().await;
        let result = fetch(&[&down.url("/"), &gone.url("/"), &refused]).await;
        let error = result.unwrap_err().to_string();
        let lines: Vec<&str> = error.lines().collect();
        assert_eq!(lines[0], "no source could serve it:");
        assert_eq!(
            lines[1],
            format!("  {}: 502 Bad Gateway (3 attempts)", down.url(POOL))
        );
        assert_eq!(lines[2], format!("  {}: 404 Not Found", gone.url(POOL)));
        assert!(
            lines[3].starts_with(&format!("  {refused}{POOL}: could not connect: "))
                && lines[3].ends_with("(3 attempts)"),
            "{}",
            lines[3]
        );
        assert_eq!(lines.len(), 4);
    }

    #[tokio::test]
    async fn a_store_spares_the_repository() {
        let store = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let repo = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let result = fetch_from(&[&store.url("/debs")], &[&repo.url("/")]).await;
        assert_eq!(result.unwrap(), (DEB.to_vec(), Origin::Store));
        assert_eq!(store.requests(), [format!("GET /debs{}", stored())]);
        assert!(repo.requests().is_empty());
    }

    #[tokio::test]
    async fn the_repository_covers_for_a_store() {
        let missing = TestServer::start(|_| Reply::Status(404)).await;
        let down = TestServer::start(|_| Reply::Status(500)).await;
        let repo = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let result = fetch_from(
            &[&missing.url("/"), &down.url("/"), &refused().await],
            &[&repo.url("/")],
        )
        .await;
        assert_eq!(result.unwrap(), (DEB.to_vec(), Origin::Repository));
        assert_eq!(missing.requests().len(), 1);
        assert_eq!(down.requests().len(), 3);
    }

    #[tokio::test]
    async fn a_store_with_the_wrong_bytes_stops_the_download() {
        let store = TestServer::start(|_| Reply::Body(b"evil bash".to_vec())).await;
        let repo = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let error = fetch_from(&[&store.url("/")], &[&repo.url("/")])
            .await
            .unwrap_err();
        assert!(matches!(error, DownloadError::WrongBytes(_)), "{error}");
        assert!(repo.requests().is_empty());
    }

    #[tokio::test]
    async fn puts_a_source_that_is_down_last() {
        let store = TestServer::start(|_| Reply::Status(503)).await;
        let repo = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let dir = tempfile::TempDir::new().unwrap();
        let service = service(dir.path());
        let stores = [store.url("/").parse().unwrap()];
        let repositories =
            IndexMap::from([("test".to_string(), repository(&repo.url("/"), None, &[]))]);
        let sources = Sources {
            stores: &stores,
            repositories: &repositories,
        };

        let mut other = bash();
        other.filename = "pool/main/o/other.deb".to_string();
        assert!(service.fetch(&bash(), sources).await.is_ok());
        assert!(service.fetch(&other, sources).await.is_ok());
        assert_eq!(store.requests().len(), 3);
        assert_eq!(repo.requests().len(), 2);

        // With nowhere else to go, a down source gets its retries.
        let repositories =
            IndexMap::from([("test".to_string(), repository(&store.url("/"), None, &[]))]);
        let sources = Sources {
            stores: &[],
            repositories: &repositories,
        };
        let error = service.fetch(&bash(), sources).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "no source could serve it:\n  {}: 503 Service Unavailable (3 attempts)",
                store.url(POOL)
            )
        );
        assert_eq!(store.requests().len(), 6);
    }

    #[tokio::test]
    async fn asks_a_down_source_last_but_still_asks() {
        // Down for the first package's three attempts, then back.
        let store = TestServer::start(|request| match request.count {
            1..=3 => Reply::Status(503),
            _ => Reply::Body(DEB.to_vec()),
        })
        .await;
        let repo = TestServer::start(|request| match request.path.as_str() {
            POOL => Reply::Body(DEB.to_vec()),
            _ => Reply::Status(404),
        })
        .await;
        let dir = tempfile::TempDir::new().unwrap();
        let service = service(dir.path());
        let stores = [store.url("/").parse().unwrap()];
        let repositories =
            IndexMap::from([("test".to_string(), repository(&repo.url("/"), None, &[]))]);
        let sources = Sources {
            stores: &stores,
            repositories: &repositories,
        };
        assert_eq!(
            service.fetch(&bash(), sources).await.unwrap().1,
            Origin::Repository
        );

        // Gone from the repository: the store, asked last, has it.
        let mut dropped = bash();
        dropped.filename = "pool/main/b/dropped.deb".to_string();
        assert_eq!(
            service.fetch(&dropped, sources).await.unwrap().1,
            Origin::Store
        );
        let requests = repo.requests();
        assert_eq!(requests.last().unwrap(), "GET /pool/main/b/dropped.deb");
        assert_eq!(store.requests().len(), 4);

        // And back in the order.
        assert_eq!(
            service.fetch(&bash(), sources).await.unwrap().1,
            Origin::Store
        );
    }

    #[tokio::test]
    async fn caches_what_it_fetched() {
        let up = TestServer::start(|_| Reply::Body(DEB.to_vec())).await;
        let dir = tempfile::TempDir::new().unwrap();
        let service = service(dir.path());
        let repositories =
            IndexMap::from([("test".to_string(), repository(&up.url("/"), None, &[]))]);
        let sources = Sources {
            stores: &[],
            repositories: &repositories,
        };
        let package = bash();

        let first = service.download(&package, sources).await.unwrap();
        assert_eq!(first.origin, Origin::Repository);
        let second = service.download(&package, sources).await.unwrap();
        assert_eq!(second.origin, Origin::Cache);
        assert_eq!(std::fs::read(second.path).unwrap(), DEB);
        assert_eq!(up.requests().len(), 1);
    }
}
