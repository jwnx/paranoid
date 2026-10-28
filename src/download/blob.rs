//! What the lock pins by its hash, and where to fetch it.

use url::Url;

use crate::hash::Sha256;
use crate::lock::{LockedFile, LockedPackage};

use super::{DownloadError, Sources};

/// Something the lock pins by its SHA-256 and size, which the cache, the
/// stores and the download service handle alike.
pub trait Blob {
    /// The file name extension it gets in content-addressed storage.
    const EXTENSION: &'static str;
    /// Its media type, for uploads to a store.
    const MEDIA_TYPE: &'static str;

    /// Its name in the lock.
    fn name(&self) -> &str;
    fn sha256(&self) -> &Sha256;
    fn size(&self) -> u64;

    /// Where to fetch it from after the stores, in the order to try, as
    /// (base URL, URL) pairs. A source whose base is down for one blob is
    /// asked last for the next.
    fn upstream(&self, sources: Sources<'_>) -> Result<Vec<(String, String)>, DownloadError>;
}

impl Blob for LockedFile {
    const EXTENSION: &'static str = "";
    const MEDIA_TYPE: &'static str = "application/octet-stream";

    fn name(&self) -> &str {
        &self.name
    }

    fn sha256(&self) -> &Sha256 {
        &self.sha256
    }

    fn size(&self) -> u64 {
        self.size
    }

    /// The file's own URL. Its server is its base, so a server that's down
    /// for one file is asked last for the next.
    fn upstream(&self, _: Sources<'_>) -> Result<Vec<(String, String)>, DownloadError> {
        Ok(vec![(
            self.url.origin().ascii_serialization(),
            self.url.to_string(),
        )])
    }
}

/// A URL without a trailing slash, to join paths to.
pub(crate) fn base(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_string()
}

impl Blob for LockedPackage {
    const EXTENSION: &'static str = ".deb";
    const MEDIA_TYPE: &'static str = "application/vnd.debian.binary-package";

    fn name(&self) -> &str {
        &self.name
    }

    fn sha256(&self) -> &Sha256 {
        &self.sha256
    }

    fn size(&self) -> u64 {
        self.size
    }

    /// The package's repository, then the repository's mirrors.
    fn upstream(&self, sources: Sources<'_>) -> Result<Vec<(String, String)>, DownloadError> {
        let repo = sources
            .repositories
            .get(&self.repository)
            .ok_or_else(|| DownloadError::UnknownRepository(self.repository.clone()))?;
        Ok(std::iter::once(repo.base_url())
            .chain(repo.mirrors.iter().map(base))
            .map(|base| {
                let url = format!("{base}/{}", self.filename);
                (base, url)
            })
            .collect())
    }
}
