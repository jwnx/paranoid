//! Locking files: checking each against its pin or its signatures, and
//! recording its hash and size.

use chrono::{DateTime, Utc};
use thiserror::Error;
use url::Url;

use crate::config::{Manifest, Source};
use crate::download::{DownloadError, DownloadFailures, DownloadService, PackageCache, Sources};
use crate::hash::Sha256;
use crate::http::{Client, ClientError, Retry};
use crate::lock::{LockFile, LockedFile};
use crate::report::Reporter;

use super::resolve::{Expected, Indexes, ResolveError};
use crate::repository::debian::{
    Fingerprint, KeyTime, Keyring, KeyringError, Signed as SignedAs, VerifyError,
};

/// The most `lock` reads of a file before it knows the file's size.
pub const MAX_FILE_SIZE: u64 = 4 << 30;

/// The most `lock` reads of a signature or a checksums file.
const MAX_SIGNED_SIZE: u64 = 1 << 20;

/// Errors locking the manifest's files.
#[derive(Debug, Error)]
pub enum LockError {
    #[error("could not set up an HTTP client")]
    Http(#[source] ClientError),

    #[error(transparent)]
    Download(#[from] DownloadFailures),

    #[error("file '{file}': {source}")]
    File {
        file: String,
        #[source]
        source: FileError,
    },
}

/// Why one file couldn't be locked.
#[derive(Debug, Error)]
pub enum FileError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),

    #[error("could not fetch {url}")]
    Fetch {
        url: String,
        #[source]
        source: ClientError,
    },

    #[error("could not load its signed_by keys")]
    Keyring(#[source] KeyringError),

    #[error("{url} is not signed by a key in its signed_by")]
    Verify {
        url: String,
        #[source]
        source: VerifyError,
    },

    #[error("{checksums} lists no '{name}'")]
    NotListed { checksums: String, name: String },

    #[error("{checksums} lists '{name}' more than once, with different hashes")]
    ListedTwice { checksums: String, name: String },

    #[error("its signed SHA-256 is {signed}, but {by} says {other}")]
    Mismatch {
        signed: Sha256,
        other: Sha256,
        by: String,
    },

    #[error("could not cache it")]
    Cache(#[source] DownloadError),
}

/// How `lock` treats what the existing lock records.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Check every file again, instead of keeping an entry that still
    /// stands. A file from an index stays at its release.
    pub recheck: bool,
    /// Move every file from an index to the newest release its spec allows.
    pub upgrade: bool,
}

/// Lock the manifest's files, in manifest order, with the publishers'
/// indexes where they usually are.
pub async fn lock(
    manifest: &Manifest,
    existing: Option<&LockFile>,
    options: Options,
    downloads: &DownloadService,
    reporter: &dyn Reporter,
) -> Result<Vec<LockedFile>, LockError> {
    lock_with(
        manifest,
        existing,
        options,
        &Indexes::default(),
        downloads,
        reporter,
    )
    .await
}

/// Lock the manifest's files, in manifest order.
///
/// A file from an index is first resolved to a release: the one the
/// existing lock has while the spec still allows it, unless upgrading,
/// otherwise the newest one it allows. Then:
///
/// - a pinned file is checked against its `sha256`;
/// - a signed one is checked against its signature, or the hash a signed
///   checksums file lists for it, with keys judged as of when they signed,
///   and against `sha256`, or the hash its index lists, if there is one;
/// - one from an index is checked against the hash the index lists.
///
/// The lock records its hash and size. A file the existing lock already
/// records the same way (the same release and URLs, and the same pin, or a
/// signer the manifest still trusts) keeps its entry. One the cache has
/// isn't downloaded again, and one that is goes into the cache for `build`.
pub async fn lock_with(
    manifest: &Manifest,
    existing: Option<&LockFile>,
    options: Options,
    indexes: &Indexes,
    downloads: &DownloadService,
    reporter: &dyn Reporter,
) -> Result<Vec<LockedFile>, LockError> {
    let fetcher = Fetcher {
        client: Client::new().map_err(LockError::Http)?,
        retry: Retry::default(),
        now: Utc::now(),
    };
    let arch = manifest.architecture;
    let cache = downloads.cache();

    let mut files = Vec::new();
    // Files whose hash we know but whose bytes we haven't seen.
    let mut missing = Vec::new();
    for (id, spec) in &manifest.files {
        let in_file = |source| LockError::File {
            file: id.clone(),
            source,
        };
        let keyring = spec
            .signed_by
            .as_ref()
            .map(|path| Keyring::load(&manifest.resolve_path(path)))
            .transpose()
            .map_err(|e| in_file(FileError::Keyring(e)))?;
        let previous = existing.and_then(|lock| lock.files.iter().find(|f| &f.name == id));
        let keep = previous
            .filter(|_| !options.upgrade)
            .and_then(|old| old.version.as_deref());

        // What the file is at the release it has, if it still stands.
        let resolved = !matches!(spec.source(), Source::Url(_));
        let at_kept = if resolved {
            keep.and_then(|version| indexes.expected(spec, Some(version), arch))
        } else {
            indexes.expected(spec, None, arch)
        };
        if !options.recheck
            && let (Some(old), Some(expected)) = (previous, &at_kept)
            && still_stands(old, expected, spec.sha256, keyring.as_ref())
        {
            files.push(old.clone());
            continue;
        }

        // The file as it should be, with the hash its pin or index gives.
        let (expected, listed, by) = if resolved {
            let release = indexes
                .resolve(&fetcher.client, &fetcher.retry, spec, arch, keep)
                .await
                .map_err(|e| in_file(e.into()))?;
            let by = release.expected.url.to_string();
            (release.expected, release.sha256, by)
        } else {
            let expected = at_kept.expect("a file from a URL is what it says");
            (expected, spec.sha256, "the manifest".to_string())
        };
        let mut file = LockedFile {
            name: id.clone(),
            url: expected.url.clone(),
            sha256: Sha256::of(b""),
            size: MAX_FILE_SIZE,
            trust: expected.trust,
            signed_by: Vec::new(),
            signature: expected.signature.clone(),
            checksums: expected.checksums.clone(),
            version: expected.version.clone(),
        };
        match (&keyring, listed) {
            (None, Some(sha256)) => file.sha256 = sha256,
            (None, None) => {
                // From an index that lists hashes in a checksums file.
                let checksums = expected
                    .checksums
                    .as_ref()
                    .expect("an index lists the hash");
                file.sha256 = fetcher
                    .listed(checksums, &expected.url)
                    .await
                    .map_err(in_file)?;
            }
            (Some(keyring), listed) => {
                let signed = fetcher.signed(&expected, keyring).await.map_err(in_file)?;
                if let Some(other) = listed
                    && other != signed.sha256
                {
                    let signed = signed.sha256;
                    return Err(in_file(FileError::Mismatch { signed, other, by }));
                }
                file.sha256 = signed.sha256;
                file.signed_by = signed.signers;
                if let Some(data) = signed.data {
                    file.size = data.len() as u64;
                    cache
                        .store(&file, &data)
                        .map_err(|e| in_file(FileError::Cache(e)))?;
                    files.push(file);
                    continue;
                }
            }
        }
        match PackageCache::read(&cache.path_for(&file), &file) {
            Ok(data) => file.size = data.len() as u64,
            Err(_) => missing.push(files.len()),
        }
        files.push(file);
    }

    let wanted: Vec<LockedFile> = missing.iter().map(|&i| files[i].clone()).collect();
    let wanted: Vec<&LockedFile> = wanted.iter().collect();
    let fetched = downloads
        .download_all(&wanted, Sources::of(manifest), reporter)
        .await?;
    for (&i, fetched) in missing.iter().zip(fetched) {
        // The download was checked against the hash on its way into the
        // cache, so its length is the file's size.
        files[i].size = std::fs::metadata(&fetched.path)
            .map(|m| m.len())
            .map_err(|e| DownloadFailures(vec![(files[i].name.clone(), e.into())]))?;
    }
    Ok(files)
}

/// Whether a file's entry in the existing lock is still what the manifest
/// asks for, so it needn't be checked again.
fn still_stands(
    old: &LockedFile,
    expected: &Expected,
    pinned: Option<Sha256>,
    keyring: Option<&Keyring>,
) -> bool {
    let as_expected = old.url == expected.url
        && old.signature == expected.signature
        && old.checksums == expected.checksums
        && old.version == expected.version
        && old.trust == expected.trust;
    let pinned = pinned.is_none_or(|sha256| old.sha256 == sha256);
    let vouched = match keyring {
        None => true,
        Some(keyring) => {
            let trusted = keyring.vouching();
            old.signed_by.iter().any(|fp| trusted.contains(fp))
        }
    };
    as_expected && pinned && vouched
}

/// A signed file's hash, its signers, and its bytes if checking the
/// signature took them.
struct Signed {
    sha256: Sha256,
    signers: Vec<Fingerprint>,
    data: Option<Vec<u8>>,
}

/// Fetches and checks signatures.
struct Fetcher {
    client: Client,
    retry: Retry,
    now: DateTime<Utc>,
}

impl Fetcher {
    /// Check a signed file: through its checksums file if it has one,
    /// otherwise through a signature over the file itself.
    async fn signed(&self, file: &Expected, keyring: &Keyring) -> Result<Signed, FileError> {
        let Some(checksums) = &file.checksums else {
            let signature = file.signature.as_ref().expect("a signed file has one");
            let sig = self.fetch(signature, MAX_SIGNED_SIZE).await?;
            let data = self.fetch(&file.url, MAX_FILE_SIZE).await?;
            let signers = self.verify(keyring, &sig, &data, SignedAs::Bytes, &file.url)?;
            return Ok(Signed {
                sha256: Sha256::of(&data),
                signers,
                data: Some(data),
            });
        };

        let sums = self.fetch(checksums, MAX_SIGNED_SIZE).await?;
        let (text, signers) = match &file.signature {
            Some(signature) => {
                let sig = self.fetch(signature, MAX_SIGNED_SIZE).await?;
                let signers = self.verify(keyring, &sig, &sums, SignedAs::Text, checksums)?;
                (String::from_utf8_lossy(&sums).into_owned(), signers)
            }
            None => {
                let verify = |source| FileError::Verify {
                    url: checksums.to_string(),
                    source,
                };
                let sums = String::from_utf8(sums)
                    .map_err(|e| verify(VerifyError::Malformed(format!("not UTF-8: {e}"))))?;
                let verified = keyring
                    .verify_cleartext_with(&sums, self.now, KeyTime::Signing)
                    .map_err(verify)?;
                (verified.text, verified.signers)
            }
        };
        Ok(Signed {
            sha256: lookup(&text, checksums, &file.url)?,
            signers,
            data: None,
        })
    }

    /// The hash an unsigned checksums file lists for `url`.
    async fn listed(&self, checksums: &Url, url: &Url) -> Result<Sha256, FileError> {
        let sums = self.fetch(checksums, MAX_SIGNED_SIZE).await?;
        lookup(&String::from_utf8_lossy(&sums), checksums, url)
    }

    async fn fetch(&self, url: &Url, limit: u64) -> Result<Vec<u8>, FileError> {
        let (result, _) = self
            .retry
            .run(|| self.client.fetch_bytes_limited(url.as_str(), limit))
            .await;
        result.map_err(|source| FileError::Fetch {
            url: url.to_string(),
            source,
        })
    }

    fn verify(
        &self,
        keyring: &Keyring,
        signature: &[u8],
        data: &[u8],
        signed: SignedAs,
        url: &Url,
    ) -> Result<Vec<Fingerprint>, FileError> {
        keyring
            .verify_detached(signature, data, signed, self.now, KeyTime::Signing)
            .map_err(|source| FileError::Verify {
                url: url.to_string(),
                source,
            })
    }
}

/// The hash `checksums`, whose text is `text`, lists for the file at
/// `url`, by the last part of its path.
fn lookup(text: &str, checksums: &Url, url: &Url) -> Result<Sha256, FileError> {
    let name = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default();
    listed(text, name).map_err(|listed| match listed {
        Listed::Missing => FileError::NotListed {
            checksums: checksums.to_string(),
            name: name.to_string(),
        },
        Listed::Twice => FileError::ListedTwice {
            checksums: checksums.to_string(),
            name: name.to_string(),
        },
    })
}

/// Why a checksums file has no hash for a name.
#[derive(Debug, PartialEq, Eq)]
enum Listed {
    Missing,
    Twice,
}

/// The SHA-256 a checksums file lists for `name`, as `sha256sum` writes
/// it: `<hash>  <name>`, or `<hash> *<name>` in binary mode. Lines for
/// other names, comments and anything else are skipped.
fn listed(text: &str, name: &str) -> Result<Sha256, Listed> {
    let mut found = None;
    for line in text.lines() {
        let Some((hash, listed)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let listed = listed.trim_start();
        let listed = listed.strip_prefix('*').unwrap_or(listed);
        let listed = listed.strip_prefix("./").unwrap_or(listed);
        if listed.trim_end() != name {
            continue;
        }
        let Ok(hash) = hash.parse::<Sha256>() else {
            continue;
        };
        match found {
            Some(other) if other != hash => return Err(Listed::Twice),
            _ => found = Some(hash),
        }
    }
    found.ok_or(Listed::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_name_in_a_checksums_file() {
        let a = Sha256::of(b"a");
        let b = Sha256::of(b"b");
        let text = format!("# comment\n{a}  tool-1.0.tar.gz\n{b} *other.tar.gz\n{b}  ./b.tar\n");
        assert_eq!(listed(&text, "tool-1.0.tar.gz"), Ok(a));
        assert_eq!(listed(&text, "other.tar.gz"), Ok(b));
        assert_eq!(listed(&text, "b.tar"), Ok(b));
        assert_eq!(listed(&text, "tool"), Err(Listed::Missing));

        let twice = format!("{a}  x\n{b}  x\n");
        assert_eq!(listed(&twice, "x"), Err(Listed::Twice));
        let same = format!("{a}  x\n{a}  x\n");
        assert_eq!(listed(&same, "x"), Ok(a));
    }

    mod signed {
        use std::path::Path;

        use super::*;
        use crate::download::DownloadConfig;
        use crate::http::test_server::{Reply, TestServer};
        use crate::lock::Trust;

        const DATA: &str = "../repository/debian/testdata/files";
        const RELEASE_FP: &str = "7B6CFBF9384A130F6053229331B1E3B66D0E9BF6";

        fn testdata(name: &str) -> Vec<u8> {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/files")
                .join(DATA);
            std::fs::read(dir.join(name)).unwrap()
        }

        async fn server() -> TestServer {
            TestServer::start(|request| {
                let name = request.path.trim_start_matches('/');
                match name {
                    "tool" | "tool.asc" | "SHA256SUMS" | "SHA256SUMS.asc" | "SHA256SUMS.sig" => {
                        Reply::Body(testdata(name))
                    }
                    _ => Reply::Status(404),
                }
            })
            .await
        }

        /// A manifest in `dir` with one file, `tool`, from `server`, and
        /// `keys` as its signed_by.
        fn manifest(dir: &Path, server: &TestServer, keys: &str, extra: &str) -> Manifest {
            std::fs::write(dir.join("keys.asc"), testdata(keys)).unwrap();
            let toml = format!(
                "[repositories.debian]\nurl = \"https://deb.debian.org/debian\"\n\
                 distribution = \"bookworm\"\ncomponents = [\"main\"]\n\
                 signed_by = \"keys.asc\"\n\n\
                 [files.tool]\nurl = \"{}\"\npath = \"/usr/bin/tool\"\n\
                 signed_by = \"keys.asc\"\n{extra}",
                server.url("/tool")
            );
            Manifest::from_file_content(&dir.join("paranoid.toml"), &toml).unwrap()
        }

        fn downloads(dir: &Path) -> DownloadService {
            let cache = PackageCache::with_base_dir(dir.join("cache")).unwrap();
            DownloadService::with_cache(cache, DownloadConfig::default()).unwrap()
        }

        async fn lock_with(
            manifest: &Manifest,
            existing: Option<&LockFile>,
            dir: &Path,
        ) -> Result<Vec<LockedFile>, LockError> {
            lock(manifest, existing, Options::default(), &downloads(dir), &()).await
        }

        fn checked(files: &[LockedFile]) -> &LockedFile {
            let file = &files[0];
            assert_eq!(file.trust, Trust::Signed);
            assert_eq!(file.sha256, Sha256::of(&testdata("tool")));
            assert_eq!(file.size, testdata("tool").len() as u64);
            let signers: Vec<_> = file.signed_by.iter().map(ToString::to_string).collect();
            assert_eq!(signers, [RELEASE_FP]);
            file
        }

        #[tokio::test]
        async fn checks_a_signature_over_the_file() {
            let dir = tempfile::tempdir().unwrap();
            let server = server().await;
            let signature = format!("signature = \"{}\"\n", server.url("/tool.asc"));
            let manifest = manifest(dir.path(), &server, "release.asc", &signature);
            let files = lock_with(&manifest, None, dir.path()).await.unwrap();
            let file = checked(&files);
            assert_eq!(
                file.signature.as_ref().map(Url::as_str),
                Some(server.url("/tool.asc").as_str())
            );
            // It's in the cache for build.
            let cache = PackageCache::with_base_dir(dir.path().join("cache")).unwrap();
            assert!(cache.get(file).is_some());
        }

        #[tokio::test]
        async fn takes_the_hash_from_a_signed_checksums_file() {
            for extra in [
                "checksums = \"{base}/SHA256SUMS.asc\"\n",
                "checksums = \"{base}/SHA256SUMS\"\nsignature = \"{base}/SHA256SUMS.sig\"\n",
            ] {
                let dir = tempfile::tempdir().unwrap();
                let server = server().await;
                let extra = extra.replace("{base}", &server.url(""));
                let manifest = manifest(dir.path(), &server, "release.asc", &extra);
                let files = lock_with(&manifest, None, dir.path()).await.unwrap();
                checked(&files);
            }
        }

        #[tokio::test]
        async fn refuses_a_signature_from_a_stranger() {
            let dir = tempfile::tempdir().unwrap();
            let server = server().await;
            let signature = format!("signature = \"{}\"\n", server.url("/tool.asc"));
            let manifest = manifest(dir.path(), &server, "stranger.asc", &signature);
            let err = lock_with(&manifest, None, dir.path()).await.unwrap_err();
            let LockError::File { file, source } = err else {
                panic!("{err}");
            };
            assert_eq!(file, "tool");
            assert!(matches!(source, FileError::Verify { .. }), "{source}");
        }

        #[tokio::test]
        async fn a_pin_has_to_agree_with_the_signature() {
            let dir = tempfile::tempdir().unwrap();
            let server = server().await;
            let extra = format!(
                "checksums = \"{}\"\nsha256 = \"{}\"\n",
                server.url("/SHA256SUMS.asc"),
                Sha256::of(b"something else")
            );
            let manifest = manifest(dir.path(), &server, "release.asc", &extra);
            let err = lock_with(&manifest, None, dir.path()).await.unwrap_err();
            assert!(err.to_string().contains("but the manifest says"), "{err}");
        }

        #[tokio::test]
        async fn keeps_an_entry_its_signer_still_vouches_for() {
            let dir = tempfile::tempdir().unwrap();
            let server = server().await;
            let checksums = format!("checksums = \"{}\"\n", server.url("/SHA256SUMS.asc"));
            let manifest = manifest(dir.path(), &server, "release.asc", &checksums);
            let files = lock_with(&manifest, None, dir.path()).await.unwrap();
            let asked = server.received().len();

            let mut existing = LockFile {
                config_hash: String::new(),
                repositories: Vec::new(),
                packages: Vec::new(),
                files: files.clone(),
                images: Vec::new(),
            };
            let again = lock_with(&manifest, Some(&existing), dir.path())
                .await
                .unwrap();
            assert_eq!(again, files);
            assert_eq!(server.received().len(), asked);

            // A signer the manifest no longer trusts is checked again.
            existing.files[0].signed_by = vec![];
            lock_with(&manifest, Some(&existing), dir.path())
                .await
                .unwrap();
            assert!(server.received().len() > asked);
        }
    }
}
