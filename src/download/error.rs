//! Download error types.

use std::path::PathBuf;

use thiserror::Error;

use crate::http::ClientError;

/// Errors that can occur during download operations.
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("HTTP error: {0}")]
    Http(#[from] ClientError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch for {package}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        package: String,
        expected: String,
        actual: String,
    },

    #[error("failed to create cache directory: {0}")]
    CacheDir(String),

    #[error("unknown repository: {0}")]
    UnknownRepository(String),
}

/// A successfully downloaded package.
#[derive(Debug, Clone)]
pub struct DownloadedPackage {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Path to the downloaded .deb file.
    pub path: PathBuf,
    /// Whether the package was served from cache.
    pub from_cache: bool,
}

/// A failed package download.
#[derive(Debug)]
pub struct DownloadFailure {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// The error that occurred.
    pub error: DownloadError,
}

/// Result of a batch download operation.
#[derive(Debug)]
#[must_use = "download results should be checked for failures"]
pub struct DownloadResult {
    /// Successfully downloaded packages.
    pub succeeded: Vec<DownloadedPackage>,
    /// Failed downloads.
    pub failed: Vec<DownloadFailure>,
}

impl DownloadResult {
    /// Create a new empty download result.
    pub fn new() -> Self {
        Self {
            succeeded: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Check if all downloads succeeded.
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }

    /// Get the total number of packages (succeeded + failed).
    pub fn total(&self) -> usize {
        self.succeeded.len() + self.failed.len()
    }
}

impl Default for DownloadResult {
    fn default() -> Self {
        Self::new()
    }
}
