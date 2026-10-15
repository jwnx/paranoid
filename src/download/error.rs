//! Download error types.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::hash::Sha256;
use crate::http::ClientError;
use crate::lock::LockedPackage;

/// Errors that can occur downloading one package.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP error: {0}")]
    Http(#[from] ClientError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch for {package}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        package: String,
        expected: Sha256,
        actual: Sha256,
    },

    #[error("failed to create cache directory: {0}")]
    CacheDir(String),

    #[error("unknown repository: {0}")]
    UnknownRepository(String),
}

/// A package available in the local cache.
#[derive(Debug, Clone)]
pub struct Fetched<'a> {
    /// The lock entry this file satisfies.
    pub package: &'a LockedPackage,
    /// Path to the cached `.deb`.
    pub path: PathBuf,
    /// Whether it was already cached before this run.
    pub from_cache: bool,
}

/// One or more packages failed to download.
#[derive(Debug, Error)]
pub struct DownloadFailures(pub Vec<(String, DownloadError)>);

impl fmt::Display for DownloadFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_slice() {
            [(name, error)] => write!(f, "failed to download {name}: {error}"),
            failures => write!(f, "{} packages failed to download", failures.len()),
        }
    }
}
