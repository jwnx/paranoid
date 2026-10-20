//! Filling a store: uploading the packages of a lock that a store doesn't
//! have yet.
//!
//! A store holds packages by their SHA-256, in the local cache's layout
//! (see [`content_path`]), and `build` reads it before any repository. A
//! store can be an HTTP server that takes `PUT`s, such as an S3 bucket, or
//! a local directory to sync somewhere with other tools.

use std::fmt;
use std::num::NonZeroUsize;
use std::path::Path;

use futures::stream::{self, StreamExt};
use thiserror::Error;
use url::Url;

use crate::config::check_base_url;
use crate::download::{DownloadError, Fetched, PackageCache, all_or_errors, content_path};
use crate::http::{Client, ClientError, Credentials, EMPTY_PAYLOAD, Head, Retry};
use crate::lock::LockedPackage;
use crate::report::{Event, Phase, Reporter};

/// The media type of a `.deb`.
const DEB_TYPE: &str = "application/vnd.debian.binary-package";

/// Where packages are pushed to.
pub enum Store {
    /// A directory laid out like the cache.
    Dir(PackageCache),
    /// An HTTP server that takes `PUT`s, under a base URL.
    Http {
        client: Client,
        /// Without a trailing slash.
        base: String,
        /// Signs every request for S3 when set.
        credentials: Option<Credentials>,
        retry: Retry,
    },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("{0}")]
    Target(String),

    #[error(transparent)]
    Http(#[from] ClientError),

    #[error(transparent)]
    Package(#[from] DownloadError),
}

/// One or more packages failed to push.
#[derive(Debug, Error)]
pub struct PushFailures(pub Vec<(String, StoreError)>);

impl fmt::Display for PushFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_slice() {
            [(name, error)] => write!(f, "failed to push {name}: {error}"),
            // Usually they all failed the same way, as when the store
            // refuses us, so the first one says enough.
            [(name, error), ..] => write!(
                f,
                "{} packages failed to push, the first {name}: {error}",
                self.0.len()
            ),
            [] => unreachable!("a failure has at least one package"),
        }
    }
}

/// What a push did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushReport {
    pub pushed: usize,
    pub bytes: u64,
}

impl Store {
    /// The store at `target`: an `http` or `https` URL, or an existing
    /// directory. Requests to a URL are signed for S3 when the environment
    /// has AWS credentials (see [`Credentials::from_env`]).
    pub fn open(target: &str, retry: Retry) -> Result<Self, StoreError> {
        match Url::parse(target) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                Self::http(&url, Credentials::from_env(), retry)
            }
            _ if Path::new(target).is_dir() => {
                Ok(Self::Dir(PackageCache::with_base_dir(target.into())?))
            }
            _ => Err(StoreError::Target(format!(
                "'{target}' is neither an http(s) URL nor a directory; for an S3 bucket, \
                 use its https URL"
            ))),
        }
    }

    /// The store under `url`, signing requests with `credentials`.
    pub fn http(
        url: &Url,
        credentials: Option<Credentials>,
        retry: Retry,
    ) -> Result<Self, StoreError> {
        check_base_url(url).map_err(StoreError::Target)?;
        Ok(Self::Http {
            client: Client::new()?,
            base: url.as_str().trim_end_matches('/').to_string(),
            credentials,
            retry,
        })
    }

    /// Whether this is the store at `url`.
    pub fn is_at(&self, url: &Url) -> bool {
        match self {
            Self::Http { base, .. } => base == url.as_str().trim_end_matches('/'),
            Self::Dir(_) => false,
        }
    }

    /// Whether requests are signed.
    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            Self::Http {
                credentials: Some(_),
                ..
            }
        )
    }

    /// The packages the store doesn't have, in the order given. A package
    /// of the wrong size counts as missing, so pushing repairs it, and so
    /// does a 403: S3 answers that for a missing key unless we may list
    /// the bucket. If we really may not write, the upload says so.
    pub async fn missing<'a>(
        &self,
        packages: &[&'a LockedPackage],
        jobs: NonZeroUsize,
    ) -> Result<Vec<&'a LockedPackage>, PushFailures> {
        let checks = packages.iter().map(|&package| async move {
            match self.has(package).await {
                Ok(true) => Ok(None),
                Ok(false) => Ok(Some(package)),
                Err(error) => Err((package.name.clone(), error)),
            }
        });
        let results: Vec<_> = stream::iter(checks).buffered(jobs.get()).collect().await;
        all_or_errors(results)
            .map(|missing| missing.into_iter().flatten().collect())
            .map_err(PushFailures)
    }

    /// Upload packages from the local cache, checking each against the
    /// lock first, so a store only ever gets what the lock says.
    pub async fn push(
        &self,
        packages: &[Fetched<'_>],
        jobs: NonZeroUsize,
        reporter: &dyn Reporter,
    ) -> Result<PushReport, PushFailures> {
        let bytes = packages.iter().map(|f| f.package.size).sum();
        reporter.report(Event::Begin {
            phase: Phase::Push,
            total: bytes,
        });
        let uploads = packages.iter().map(|fetched| async move {
            let package = fetched.package;
            let result = match PackageCache::read(&fetched.path, package) {
                Ok(data) => self.put(package, &data).await,
                Err(e) => Err(e.into()),
            };
            match &result {
                Ok(()) => reporter.report(Event::Advance {
                    phase: Phase::Push,
                    units: package.size,
                    item: &package.name,
                }),
                Err(error) => reporter.report(Event::Failed {
                    phase: Phase::Push,
                    item: &package.name,
                    error,
                }),
            }
            result.map_err(|error| (package.name.clone(), error))
        });
        let results: Vec<_> = stream::iter(uploads).buffered(jobs.get()).collect().await;
        reporter.report(Event::End {
            phase: Phase::Push,
            summary: bytes,
        });
        all_or_errors(results).map_err(PushFailures)?;
        Ok(PushReport {
            pushed: packages.len(),
            bytes,
        })
    }

    async fn has(&self, package: &LockedPackage) -> Result<bool, StoreError> {
        let (client, credentials, retry, url) = match self {
            Self::Dir(cache) => return Ok(cache.get(package).is_some()),
            Self::Http {
                client,
                credentials,
                retry,
                base,
            } => (
                client,
                credentials.as_ref(),
                retry,
                object_url(base, package),
            ),
        };
        let url = &url;
        let (head, _) = retry
            .run(|| {
                let headers = sign(credentials, "HEAD", url, &[], EMPTY_PAYLOAD);
                async move { client.head(url.as_str(), &headers).await }
            })
            .await;
        match head {
            // Without a length we take the store's word for it.
            Ok(Head::Present { length }) => Ok(length.is_none_or(|l| l == package.size)),
            Ok(Head::Missing) => Ok(false),
            Err(ClientError::Status { status, .. }) if status == reqwest::StatusCode::FORBIDDEN => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn put(&self, package: &LockedPackage, data: &[u8]) -> Result<(), StoreError> {
        let (client, credentials, retry, url) = match self {
            Self::Dir(cache) => {
                cache.store(package, data)?;
                return Ok(());
            }
            Self::Http {
                client,
                credentials,
                retry,
                base,
            } => (
                client,
                credentials.as_ref(),
                retry,
                object_url(base, package),
            ),
        };
        // S3 checks the body against the hash we sign, so a bucket never
        // keeps a corrupted upload.
        let hash = package.sha256.to_string();
        let url = &url;
        let (result, _) = retry
            .run(|| {
                let content_type = [("content-type", DEB_TYPE)];
                let mut headers = sign(credentials, "PUT", url, &content_type, &hash);
                headers.push(("content-type".to_string(), DEB_TYPE.to_string()));
                async move { client.put(url.as_str(), &headers, data).await }
            })
            .await;
        Ok(result?)
    }
}

impl fmt::Display for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir(cache) => write!(f, "{}", cache.base_dir().display()),
            Self::Http { base, .. } => f.write_str(base),
        }
    }
}

/// Where a package lives under a store's base URL.
fn object_url(base: &str, package: &LockedPackage) -> Url {
    Url::parse(&format!("{base}/{}", content_path(&package.sha256)))
        .expect("a valid base URL with a path appended is valid")
}

/// The headers that sign a request, if there are credentials.
fn sign(
    credentials: Option<&Credentials>,
    method: &str,
    url: &Url,
    headers: &[(&str, &str)],
    payload_sha256: &str,
) -> Vec<(String, String)> {
    match credentials {
        Some(credentials) => {
            credentials.sign(method, url, headers, payload_sha256, chrono::Utc::now())
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::download::Origin;
    use crate::download::test_utils::make_test_package;
    use crate::hash::Sha256;
    use crate::http::test_server::{Reply, TestServer};

    const JOBS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

    fn quick() -> Retry {
        Retry {
            attempts: NonZeroU32::new(3).unwrap(),
            backoff: Duration::ZERO,
        }
    }

    /// Packages with these contents, in a cache under `dir`.
    fn cached<'a>(dir: &std::path::Path, packages: &'a [LockedPackage]) -> Vec<Fetched<'a>> {
        let cache = PackageCache::with_base_dir(dir.to_path_buf()).unwrap();
        packages
            .iter()
            .map(|package| Fetched {
                package,
                path: cache.path_for(package),
                origin: Origin::Cache,
            })
            .collect()
    }

    fn package(name: &str, data: &[u8], dir: &std::path::Path) -> LockedPackage {
        let mut package = make_test_package(name, Sha256::of(data));
        package.size = data.len() as u64;
        PackageCache::with_base_dir(dir.to_path_buf())
            .unwrap()
            .store(&package, data)
            .unwrap();
        package
    }

    /// Objects by path.
    type Objects = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// A bucket in memory: `HEAD` and `GET` what was `PUT`.
    async fn bucket(objects: Objects) -> TestServer {
        TestServer::start(move |request| {
            let mut objects = objects.lock().unwrap();
            let found = objects.iter().find(|(p, _)| *p == request.path).cloned();
            match (request.method.as_str(), found) {
                ("PUT", _) => {
                    objects.retain(|(p, _)| *p != request.path);
                    objects.push((request.path.clone(), request.body.clone()));
                    Reply::Status(200)
                }
                (_, Some((_, body))) => Reply::Body(body),
                (_, None) => Reply::Status(404),
            }
        })
        .await
    }

    #[tokio::test]
    async fn pushes_only_what_the_store_lacks() {
        let dir = tempfile::TempDir::new().unwrap();
        let packages = [
            package("bash", b"bash", dir.path()),
            package("dash", b"dash", dir.path()),
            package("zsh", b"zsh", dir.path()),
        ];
        let path = |p: &LockedPackage| format!("/debs/{}", content_path(&p.sha256));
        let objects = Arc::new(Mutex::new(vec![
            (path(&packages[0]), b"bash".to_vec()),
            // Truncated, so pushed again.
            (path(&packages[2]), b"zs".to_vec()),
        ]));
        let server = bucket(objects.clone()).await;
        let url = server.url("/debs/").parse().unwrap();
        let store = Store::http(&url, None, quick()).unwrap();

        let all: Vec<&LockedPackage> = packages.iter().collect();
        let missing = store.missing(&all, JOBS).await.unwrap();
        let names: Vec<&str> = missing.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["dash", "zsh"]);

        let fetched = cached(dir.path(), &packages[1..]);
        let report = store.push(&fetched, JOBS, &()).await.unwrap();
        assert_eq!(
            report,
            PushReport {
                pushed: 2,
                bytes: 7
            }
        );
        let put: Vec<_> = server
            .received()
            .into_iter()
            .filter(|r| r.method == "PUT")
            .collect();
        assert_eq!(put.len(), 2);
        assert_eq!(put[0].header("content-type"), Some(DEB_TYPE));
        assert!(store.missing(&all, JOBS).await.unwrap().is_empty());
        assert!(
            objects
                .lock()
                .unwrap()
                .contains(&(path(&packages[2]), b"zsh".to_vec()))
        );
    }

    #[tokio::test]
    async fn signs_for_s3() {
        let dir = tempfile::TempDir::new().unwrap();
        let packages = [package("bash", b"bash", dir.path())];
        let server = bucket(Arc::default()).await;
        let credentials = Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "secret".to_string(),
            session_token: None,
            region: "auto".to_string(),
        };
        let url = server.url("/debs").parse().unwrap();
        let store = Store::http(&url, Some(credentials), quick()).unwrap();
        assert!(store.is_signed());

        let all: Vec<&LockedPackage> = packages.iter().collect();
        store.missing(&all, JOBS).await.unwrap();
        store
            .push(&cached(dir.path(), &packages), JOBS, &())
            .await
            .unwrap();
        let [head, put] = server.received().try_into().unwrap();
        for request in [&head, &put] {
            let authorization = request.header("authorization").unwrap();
            assert!(
                authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"),
                "{authorization}"
            );
            assert!(authorization.contains("/auto/s3/aws4_request"));
        }
        assert_eq!(head.header("x-amz-content-sha256"), Some(EMPTY_PAYLOAD));
        assert_eq!(
            put.header("x-amz-content-sha256"),
            Some(packages[0].sha256.to_string().as_str())
        );
        assert!(
            put.header("authorization")
                .unwrap()
                .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date,")
        );
    }

    #[tokio::test]
    async fn never_pushes_what_the_lock_does_not_say() {
        let dir = tempfile::TempDir::new().unwrap();
        let packages = [package("bash", b"bash", dir.path())];
        let fetched = cached(dir.path(), &packages);
        std::fs::write(&fetched[0].path, b"evil").unwrap();
        let server = bucket(Arc::default()).await;
        let store = Store::http(&server.url("/").parse().unwrap(), None, quick()).unwrap();

        let error = store.push(&fetched, JOBS, &()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("failed to push bash: checksum mismatch"),
            "{error}"
        );
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn reports_what_the_store_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let packages = [
            package("bash", b"bash", dir.path()),
            package("dash", b"dash", dir.path()),
        ];
        let server = TestServer::start(|request| match request.method.as_str() {
            "HEAD" => Reply::Status(500),
            _ => Reply::Status(403),
        })
        .await;
        let store = Store::http(&server.url("/").parse().unwrap(), None, quick()).unwrap();

        let all: Vec<&LockedPackage> = packages[..1].iter().collect();
        let error = store.missing(&all, JOBS).await.unwrap_err().to_string();
        assert!(
            error.ends_with("answered 500 Internal Server Error"),
            "{error}"
        );
        let error = store
            .push(&cached(dir.path(), &packages), JOBS, &())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("2 packages failed to push, the first bash: http://")
                && error.ends_with("answered 403 Forbidden"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn pushes_into_a_directory() {
        let cache = tempfile::TempDir::new().unwrap();
        let packages = [package("bash", b"bash", cache.path())];
        let target = tempfile::TempDir::new().unwrap();
        let store = Store::open(target.path().to_str().unwrap(), quick()).unwrap();
        assert!(!store.is_signed());

        let all: Vec<&LockedPackage> = packages.iter().collect();
        assert_eq!(store.missing(&all, JOBS).await.unwrap().len(), 1);
        store
            .push(&cached(cache.path(), &packages), JOBS, &())
            .await
            .unwrap();
        assert!(store.missing(&all, JOBS).await.unwrap().is_empty());
        let stored = target.path().join(content_path(&packages[0].sha256));
        assert_eq!(std::fs::read(stored).unwrap(), b"bash");
    }

    #[test]
    fn opens_urls_and_directories() {
        let open = |target: &str| Store::open(target, quick()).map(|s| s.to_string());
        assert_eq!(
            open("https://bucket.example.org/debs/").unwrap(),
            "https://bucket.example.org/debs"
        );
        let err = open("s3://bucket/debs").unwrap_err().to_string();
        assert!(err.contains("use its https URL"), "{err}");
        let err = open("https://bucket.example.org/debs?x=1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("has a query or fragment"), "{err}");

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_str().unwrap();
        assert_eq!(open(path).unwrap(), path);
        let typo = dir.path().join("stroe");
        let err = open(typo.to_str().unwrap()).unwrap_err().to_string();
        assert!(
            err.contains("neither an http(s) URL nor a directory"),
            "{err}"
        );
        assert!(!typo.exists());
    }

    #[test]
    fn knows_where_it_is() {
        let store = Store::open("https://bucket.example.org/debs", quick()).unwrap();
        assert!(store.is_at(&"https://bucket.example.org/debs/".parse().unwrap()));
        assert!(!store.is_at(&"https://bucket.example.org/other".parse().unwrap()));
    }

    #[tokio::test]
    async fn takes_a_403_for_missing_and_no_length_for_present() {
        let dir = tempfile::TempDir::new().unwrap();
        let packages = [
            package("bash", b"bash", dir.path()),
            package("dash", b"dash", dir.path()),
        ];
        let forbidden = format!("/{}", content_path(&packages[0].sha256));
        let server = TestServer::start(move |request| match request.path == forbidden {
            true => Reply::Status(403),
            false => Reply::Status(200),
        })
        .await;
        let store = Store::http(&server.url("/").parse().unwrap(), None, quick()).unwrap();
        let all: Vec<&LockedPackage> = packages.iter().collect();
        let missing = store.missing(&all, JOBS).await.unwrap();
        let names: Vec<&str> = missing.iter().map(|p| p.name.as_str()).collect();
        // The test server always sends a length, of 0 here, so `dash` is
        // the wrong size; see `Head` for the case without one.
        assert_eq!(names, ["bash", "dash"]);
    }
}
