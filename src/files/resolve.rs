//! Resolving a release from its publisher's index, and what a file is at a
//! given release.

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::arch::Architecture;
use crate::config::{Extract, FileSpec, Source, release_version};
use crate::hash::Sha256;
use crate::http::{Client, ClientError, Retry};
use crate::lock::Trust;

/// The most we read of an index. Go's, with every release, is about 2 MB.
const MAX_INDEX_SIZE: u64 = 64 << 20;

/// The most pages of a bucket's listing we read.
const MAX_PAGES: usize = 1000;

/// The name of the checksums file in each release directory of a bucket.
const GCS_CHECKSUMS: &str = "SHA256SUMS";

/// Where the indexes are. Tests point these at a local server.
#[derive(Debug, Clone)]
pub struct Indexes {
    /// Go's list of releases, as JSON.
    pub go_index: Url,
    /// Where Go's release files are, by file name.
    pub go_downloads: Url,
    /// Google Cloud Storage: objects at `<bucket>/<name>`, and the JSON
    /// API at `storage/v1/`.
    pub gcs: Url,
}

impl Default for Indexes {
    fn default() -> Self {
        let url = |s: &str| Url::parse(s).expect("a valid URL");
        Self {
            go_index: url("https://go.dev/dl/?mode=json&include=all"),
            go_downloads: url("https://go.dev/dl/"),
            gcs: url("https://storage.googleapis.com/"),
        }
    }
}

/// What a file is, at a release for a file from an index: where it comes
/// from and what vouches for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub url: Url,
    pub signature: Option<Url>,
    pub checksums: Option<Url>,
    pub trust: Trust,
    /// The release, by its publisher's name for it, for a file from an
    /// index.
    pub version: Option<String>,
}

/// A release an index picked.
#[derive(Debug, Clone)]
pub struct Release {
    pub expected: Expected,
    /// The SHA-256 the index lists for the file, if it lists one.
    pub sha256: Option<Sha256>,
}

/// Errors resolving a release.
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("could not fetch {url}")]
    Fetch {
        url: String,
        #[source]
        source: ClientError,
    },

    #[error("{url} is not the index we expected: {message}")]
    Parse { url: String, message: String },

    #[error("{index} has no release {wanted} for {arch}")]
    NoRelease {
        index: String,
        wanted: String,
        arch: Architecture,
    },

    #[error("there are no release files for {0}")]
    Arch(Architecture),
}

impl Indexes {
    /// What the file `spec` asks for is at `version`, which has to be
    /// `None` for a file from a URL, and a release the spec allows for one
    /// from an index. `None` if it isn't.
    pub fn expected(
        &self,
        spec: &FileSpec,
        version: Option<&str>,
        arch: Architecture,
    ) -> Option<Expected> {
        let signed_or = |trust| {
            if spec.signed_by.is_some() {
                Trust::Signed
            } else {
                trust
            }
        };
        match (spec.source(), version) {
            (Source::Url(url), None) => Some(Expected {
                url: url.clone(),
                signature: spec.signature.clone(),
                checksums: spec.checksums.clone(),
                trust: signed_or(Trust::Pinned),
                version: None,
            }),
            (Source::Go { version: wanted }, Some(version))
                if version.starts_with("go") && wanted.matches(version) =>
            {
                let url = self
                    .go_downloads
                    .join(&format!("{version}.linux-{}.tar.gz", go_arch(arch)?))
                    .ok()
                    .and_then(|url| under(url, &self.go_downloads))?;
                let signature = spec
                    .signed_by
                    .as_ref()
                    .and_then(|_| Url::parse(&format!("{url}.asc")).ok());
                Some(Expected {
                    url,
                    signature,
                    checksums: None,
                    trust: signed_or(Trust::Index),
                    version: Some(version.to_string()),
                })
            }
            (
                Source::Gcs {
                    bucket,
                    prefix,
                    version: wanted,
                    asset,
                },
                Some(version),
            ) if wanted.matches(version) => {
                let releases = self.gcs.join(&format!("{bucket}/{prefix}")).ok()?;
                let dir = under(releases.join(&format!("{version}/")).ok()?, &releases)?;
                let url = under(dir.join(&render(asset, version, arch)?).ok()?, &dir)?;
                Some(Expected {
                    url,
                    signature: None,
                    checksums: dir.join(GCS_CHECKSUMS).ok(),
                    trust: Trust::Index,
                    version: Some(version.to_string()),
                })
            }
            _ => None,
        }
    }

    /// The newest release `spec` allows for `arch`, or `keep` if the index
    /// still has it and the spec still allows it.
    pub async fn resolve(
        &self,
        client: &Client,
        retry: &Retry,
        spec: &FileSpec,
        arch: Architecture,
        keep: Option<&str>,
    ) -> Result<Release, ResolveError> {
        let fetch = |url: Url| async move {
            let (result, _) = retry
                .run(|| client.fetch_bytes_limited(url.as_str(), MAX_INDEX_SIZE))
                .await;
            result.map_err(|source| ResolveError::Fetch {
                url: url.to_string(),
                source,
            })
        };
        let parse = |url: &Url, e: serde_json::Error| ResolveError::Parse {
            url: url.to_string(),
            message: e.to_string(),
        };

        match spec.source() {
            Source::Url(_) => unreachable!("a file from a URL isn't resolved"),
            Source::Go { version: wanted } => {
                let goarch = go_arch(arch).ok_or(ResolveError::Arch(arch))?;
                let index = fetch(self.go_index.clone()).await?;
                let releases: Vec<GoRelease> =
                    serde_json::from_slice(&index).map_err(|e| parse(&self.go_index, e))?;
                // Each matching release with its Linux tarball for `arch`.
                let candidates: Vec<(&str, &GoFile)> = releases
                    .iter()
                    .filter(|r| r.stable && wanted.matches(&r.version))
                    .filter_map(|r| {
                        let file = r
                            .files
                            .iter()
                            .find(|f| f.os == "linux" && f.arch == goarch && f.kind == "archive")?;
                        Some((r.version.as_str(), file))
                    })
                    .collect();
                let (version, file) =
                    pick(&candidates, keep, |(v, _)| v).ok_or_else(|| ResolveError::NoRelease {
                        index: self.go_index.to_string(),
                        wanted: format!("go{wanted}"),
                        arch,
                    })?;
                let expected = self
                    .expected(spec, Some(version), arch)
                    .ok_or(ResolveError::Arch(arch))?;
                // The index names the file; it has to be the one we'd ask
                // for, since a later lock rebuilds its URL from the version.
                let name = expected.url.path_segments().and_then(|mut s| s.next_back());
                if name != Some(file.filename.as_str()) {
                    return Err(ResolveError::Parse {
                        url: self.go_index.to_string(),
                        message: format!(
                            "it names {} for {version}, not {}",
                            file.filename, expected.url
                        ),
                    });
                }
                let sha256 = file.sha256.parse().map_err(|e| ResolveError::Parse {
                    url: self.go_index.to_string(),
                    message: format!("{e}"),
                })?;
                Ok(Release {
                    expected,
                    sha256: Some(sha256),
                })
            }
            Source::Gcs {
                bucket,
                prefix,
                version: wanted,
                ..
            } => {
                let mut versions = Vec::new();
                let mut page: Option<String> = None;
                for pages in 1.. {
                    // A bucket with a thousand pages of releases, or one
                    // that hands back the same page, isn't one we can use.
                    if pages > MAX_PAGES {
                        return Err(ResolveError::Parse {
                            url: format!("gs://{bucket}/{prefix}"),
                            message: format!("it lists more than {MAX_PAGES} pages"),
                        });
                    }
                    let mut url = self
                        .gcs
                        .join(&format!("storage/v1/b/{bucket}/o"))
                        .expect("a valid path");
                    url.query_pairs_mut()
                        .append_pair("delimiter", "/")
                        .append_pair("prefix", prefix)
                        .append_pair("fields", "prefixes,nextPageToken");
                    if let Some(token) = &page {
                        url.query_pairs_mut().append_pair("pageToken", token);
                    }
                    let body = fetch(url.clone()).await?;
                    let listing: GcsListing =
                        serde_json::from_slice(&body).map_err(|e| parse(&url, e))?;
                    versions.extend(listing.prefixes.into_iter().filter_map(|p| {
                        let dir = p.strip_prefix(prefix)?.strip_suffix('/')?;
                        wanted.matches(dir).then(|| dir.to_string())
                    }));
                    match listing.next_page_token {
                        Some(token) if page.as_ref() != Some(&token) => page = Some(token),
                        Some(_) => {
                            return Err(ResolveError::Parse {
                                url: url.to_string(),
                                message: "it hands back the same page again".to_string(),
                            });
                        }
                        None => break,
                    }
                }
                let version = pick(&versions, keep, |v| v.as_str()).ok_or_else(|| {
                    ResolveError::NoRelease {
                        index: format!("gs://{bucket}/{prefix}"),
                        wanted: wanted.to_string(),
                        arch,
                    }
                })?;
                let expected = self
                    .expected(spec, Some(version), arch)
                    .ok_or(ResolveError::Arch(arch))?;
                Ok(Release {
                    expected,
                    sha256: None,
                })
            }
        }
    }
}

/// `keep` if it's among the candidates, otherwise the newest of them.
fn pick<'a, T>(
    candidates: &'a [T],
    keep: Option<&str>,
    version: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    if let Some(kept) = keep.and_then(|keep| candidates.iter().find(|c| version(c) == keep)) {
        return Some(kept);
    }
    candidates
        .iter()
        .filter_map(|c| Some((release_version(version(c))?, c)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, c)| c)
}

/// What `extract` names at a release, with `{version}` and `{arch}` filled
/// in, for a file from an index.
pub fn render_extract(
    spec: &FileSpec,
    version: Option<&str>,
    arch: Architecture,
) -> Result<Option<Extract>, String> {
    let (Some(extract), Some(version)) = (&spec.extract, version) else {
        return Ok(spec.extract.clone());
    };
    let rendered = render(&extract.as_str(), version, arch)
        .ok_or_else(|| format!("there are no release files for {arch}"))?;
    rendered.parse().map(Some)
}

/// A template with `{version}` (without a leading `v` or `go`) and `{arch}`
/// (as Go names it) filled in.
fn render(template: &str, version: &str, arch: Architecture) -> Option<String> {
    let number = version.trim_start_matches(|c: char| !c.is_ascii_digit());
    let arch = arch.oci_platform()?.architecture;
    Some(
        template
            .replace("{version}", number)
            .replace("{arch}", arch),
    )
}

/// What Go calls `arch` in its release file names: the GOARCH, except
/// that 32-bit ARM tarballs are for ARMv6.
fn go_arch(arch: Architecture) -> Option<&'static str> {
    match (arch, arch.oci_platform()?.architecture) {
        // ARMv6 with VFP is more than armel promises.
        (Architecture::Armel, _) => None,
        (_, "arm") => Some("armv6l"),
        (_, goarch) => Some(goarch),
    }
}

/// `url` if it's under `base`, which it can only fail to be when a name
/// from an index or the manifest reads as more than a path.
fn under(url: Url, base: &Url) -> Option<Url> {
    (url.origin() == base.origin() && url.path().starts_with(base.path())).then_some(url)
}

/// One release in Go's index.
#[derive(Debug, Deserialize)]
struct GoRelease {
    version: String,
    stable: bool,
    files: Vec<GoFile>,
}

#[derive(Debug, Deserialize)]
struct GoFile {
    filename: String,
    os: String,
    arch: String,
    sha256: String,
    kind: String,
}

/// One page of a bucket's directories.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GcsListing {
    #[serde(default)]
    prefixes: Vec<String>,
    next_page_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::http::test_server::{Reply, TestServer};

    const ARM64: Architecture = Architecture::Arm64;

    fn spec(toml: &str) -> FileSpec {
        toml::from_str(&format!("path = \"/x\"\n{toml}")).unwrap()
    }

    fn quick() -> Retry {
        Retry {
            attempts: std::num::NonZeroU32::new(1).unwrap(),
            backoff: Duration::ZERO,
        }
    }

    fn indexes(server: &TestServer) -> Indexes {
        let url = |path: &str| Url::parse(&server.url(path)).unwrap();
        Indexes {
            go_index: url("/dl/?mode=json&include=all"),
            go_downloads: url("/dl/"),
            gcs: url("/"),
        }
    }

    fn go_release(version: &str, stable: bool, arches: &[&str]) -> serde_json::Value {
        let files: Vec<_> = arches
            .iter()
            .map(|arch| {
                serde_json::json!({
                    "filename": format!("{version}.linux-{arch}.tar.gz"),
                    "os": "linux", "arch": arch, "kind": "archive",
                    "sha256": Sha256::of(format!("{version} {arch}").as_bytes()).to_string(),
                })
            })
            .chain([serde_json::json!({
                "filename": format!("{version}.src.tar.gz"),
                "os": "", "arch": "", "kind": "source",
                "sha256": Sha256::of(version.as_bytes()).to_string(),
            })])
            .collect();
        serde_json::json!({"version": version, "stable": stable, "files": files})
    }

    async fn go_server() -> TestServer {
        let index = serde_json::json!([
            go_release("go1.28rc1", false, &["arm64"]),
            go_release("go1.27.3", true, &["arm64", "armv6l"]),
            go_release("go1.27.10", true, &["amd64"]),
            go_release("go1.27.2", true, &["arm64"]),
            go_release("go1.26.10", true, &["arm64"]),
        ]);
        let body = serde_json::to_vec(&index).unwrap();
        TestServer::start(move |request| match request.path.as_str() {
            "/dl/?mode=json&include=all" => Reply::Body(body.clone()),
            _ => Reply::Status(404),
        })
        .await
    }

    async fn resolve(
        indexes: &Indexes,
        spec: &FileSpec,
        arch: Architecture,
        keep: Option<&str>,
    ) -> Result<Release, ResolveError> {
        let client = Client::new().unwrap();
        indexes.resolve(&client, &quick(), spec, arch, keep).await
    }

    #[tokio::test]
    async fn picks_the_newest_go_release_for_the_architecture() {
        let server = go_server().await;
        let indexes = indexes(&server);
        let go = spec("go = \"1.27\"");

        // 1.27.10 is newer, but has no arm64 tarball.
        let release = resolve(&indexes, &go, ARM64, None).await.unwrap();
        assert_eq!(release.expected.version.as_deref(), Some("go1.27.3"));
        assert_eq!(
            release.expected.url.as_str(),
            server.url("/dl/go1.27.3.linux-arm64.tar.gz")
        );
        assert_eq!(release.sha256, Some(Sha256::of(b"go1.27.3 arm64")));
        assert_eq!(release.expected.trust, Trust::Index);
        assert_eq!(release.expected.signature, None);

        // The one the lock has, while the spec allows it.
        let kept = resolve(&indexes, &go, ARM64, Some("go1.27.2"))
            .await
            .unwrap();
        assert_eq!(kept.expected.version.as_deref(), Some("go1.27.2"));
        let moved = resolve(&indexes, &go, ARM64, Some("go1.26.10"))
            .await
            .unwrap();
        assert_eq!(moved.expected.version.as_deref(), Some("go1.27.3"));

        // Prereleases never match, even asked for by their line.
        let err = resolve(&indexes, &spec("go = \"1.28\""), ARM64, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::NoRelease { .. }), "{err}");

        // 32-bit ARM is ARMv6 to Go.
        let arm = resolve(&indexes, &go, Architecture::Armhf, None)
            .await
            .unwrap();
        assert!(
            arm.expected
                .url
                .as_str()
                .ends_with("go1.27.3.linux-armv6l.tar.gz")
        );
    }

    #[tokio::test]
    async fn a_signed_go_release_is_checked_against_its_asc() {
        let server = go_server().await;
        let go = spec("go = \"1.27\"\nsigned_by = \"google.asc\"");
        let release = resolve(&indexes(&server), &go, ARM64, None).await.unwrap();
        assert_eq!(release.expected.trust, Trust::Signed);
        assert_eq!(
            release.expected.signature.as_ref().map(Url::as_str),
            Some(server.url("/dl/go1.27.3.linux-arm64.tar.gz.asc").as_str())
        );
    }

    #[tokio::test]
    async fn lists_a_bucket_page_by_page() {
        let server = TestServer::start(|request| {
            let page = |prefixes: &[&str], next: Option<&str>| {
                let mut body = serde_json::json!({ "prefixes": prefixes });
                if let Some(next) = next {
                    body["nextPageToken"] = next.into();
                }
                Reply::Body(serde_json::to_vec(&body).unwrap())
            };
            let path = &request.path;
            if !path.starts_with("/storage/v1/b/etcd/o?") || !path.contains("delimiter=%2F") {
                return Reply::Status(404);
            }
            if path.contains("pageToken=two") {
                page(&["v3.6.16/", "v3.7.0/"], None)
            } else {
                page(
                    &["v3.5.35/", "v3.6.9/", "v3.6.10-rc.0/", "v3.6.15/"],
                    Some("two"),
                )
            }
        })
        .await;
        let etcd = spec(
            "gcs = \"etcd\"\nversion = \"3.6\"\n\
             asset = \"etcd-v{version}-linux-{arch}.tar.gz\"",
        );
        let indexes = indexes(&server);
        let release = resolve(&indexes, &etcd, ARM64, None).await.unwrap();
        let expected = release.expected;
        assert_eq!(expected.version.as_deref(), Some("v3.6.16"));
        assert_eq!(
            expected.url.as_str(),
            server.url("/etcd/v3.6.16/etcd-v3.6.16-linux-arm64.tar.gz")
        );
        assert_eq!(
            expected.checksums.as_ref().map(Url::as_str),
            Some(server.url("/etcd/v3.6.16/SHA256SUMS").as_str())
        );
        assert_eq!(expected.trust, Trust::Index);
        assert_eq!(release.sha256, None);

        let kept = resolve(&indexes, &etcd, ARM64, Some("v3.6.9"))
            .await
            .unwrap();
        assert_eq!(kept.expected.version.as_deref(), Some("v3.6.9"));
    }

    #[test]
    fn what_a_file_is_at_a_release() {
        let indexes = Indexes::default();
        let etcd = spec(
            "gcs = \"etcd\"\nprefix = \"releases/\"\nversion = \"3.6\"\n\
             asset = \"etcd-v{version}-linux-{arch}.tar.gz\"\n\
             extract = \"etcd-v{version}-linux-{arch}/etcdctl\"",
        );
        let at = indexes.expected(&etcd, Some("v3.6.16"), ARM64).unwrap();
        assert_eq!(
            at.url.as_str(),
            "https://storage.googleapis.com/etcd/releases/v3.6.16/etcd-v3.6.16-linux-arm64.tar.gz"
        );
        // Only at a release the spec allows.
        assert!(indexes.expected(&etcd, Some("v3.7.0"), ARM64).is_none());
        assert!(indexes.expected(&etcd, None, ARM64).is_none());
        assert_eq!(
            render_extract(&etcd, Some("v3.6.16"), ARM64).unwrap(),
            Some(Extract::Member("etcd-v3.6.16-linux-arm64/etcdctl".into()))
        );

        let go = spec("go = \"1.27.3\"");
        let at = indexes
            .expected(&go, Some("go1.27.3"), Architecture::Amd64)
            .unwrap();
        assert_eq!(
            at.url.as_str(),
            "https://go.dev/dl/go1.27.3.linux-amd64.tar.gz"
        );
        assert!(
            indexes
                .expected(&go, Some("go1.27.2"), Architecture::Amd64)
                .is_none()
        );
    }

    #[tokio::test]
    async fn names_from_an_index_stay_names() {
        // A Go index naming a release `//1.27.99`, which would make it a
        // host, and a file with no name.
        let mut odd = go_release("go1.27.3", true, &["arm64"]);
        odd["files"][0]["filename"] = "".into();
        let index = serde_json::json!([go_release("//1.27.99", true, &["arm64"]), odd]);
        let body = serde_json::to_vec(&index).unwrap();
        let server = TestServer::start(move |_| Reply::Body(body.clone())).await;
        let err = resolve(&indexes(&server), &spec("go = \"1.27\""), ARM64, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("it names  for go1.27.3"), "{err}");

        // Bucket directories that would read as a query, a fragment or a
        // path aren't releases.
        let server = TestServer::start(|_| {
            let body = serde_json::json!({
                "prefixes": ["?3.6.99/", "#3.6.98/", "\\3.6.97/", "\t3.6.96/", "v3.6.1/"],
            });
            Reply::Body(serde_json::to_vec(&body).unwrap())
        })
        .await;
        let etcd = spec("gcs = \"etcd\"\nversion = \"3.6\"\nasset = \"etcd.tar.gz\"");
        let release = resolve(&indexes(&server), &etcd, ARM64, None)
            .await
            .unwrap();
        assert_eq!(release.expected.version.as_deref(), Some("v3.6.1"));
    }

    #[tokio::test]
    async fn a_bucket_that_pages_forever_is_refused() {
        let server = TestServer::start(|_| {
            let body = serde_json::json!({"prefixes": ["v3.6.1/"], "nextPageToken": "again"});
            Reply::Body(serde_json::to_vec(&body).unwrap())
        })
        .await;
        let etcd = spec("gcs = \"etcd\"\nversion = \"3.6\"\nasset = \"etcd.tar.gz\"");
        let err = resolve(&indexes(&server), &etcd, ARM64, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("same page again"), "{err}");
    }

    #[test]
    fn go_has_no_tarball_for_armel() {
        let go = spec("go = \"1.27\"");
        let indexes = Indexes::default();
        assert!(
            indexes
                .expected(&go, Some("go1.27.3"), Architecture::Armel)
                .is_none()
        );
        assert!(
            indexes
                .expected(&go, Some("go1.27.3"), Architecture::Armhf)
                .is_some()
        );
    }
}
