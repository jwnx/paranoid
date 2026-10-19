//! Download error types.

use std::error::Error as _;
use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use crate::hash::Sha256;
use crate::http::ClientError;
use crate::lock::LockedPackage;
use crate::report::render_chain;

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

    #[error(transparent)]
    WrongBytes(Box<WrongBytes>),

    /// No source could serve the package. Each one's answer is kept, so a
    /// failure behind a later 404 doesn't go unseen.
    #[error("no source could serve it:{}", SourceFailure::list(.0))]
    Unavailable(Vec<SourceFailure>),

    #[error("failed to create cache directory: {0}")]
    CacheDir(String),

    #[error("unknown repository: {0}")]
    UnknownRepository(String),
}

/// A source answered with something other than the package. That's never
/// a reason to try the next one: the source is broken or tampered with, and
/// someone should know.
#[derive(Debug, Error)]
#[error("{url} served the wrong bytes for {package}: expected sha256 {expected}, got {actual}")]
pub struct WrongBytes {
    pub url: String,
    pub package: String,
    pub expected: Sha256,
    pub actual: Sha256,
}

/// What one source answered, after its retries.
#[derive(Debug)]
pub struct SourceFailure {
    pub url: String,
    pub error: ClientError,
    pub attempts: u32,
}

impl SourceFailure {
    fn list(failures: &[SourceFailure]) -> String {
        failures.iter().map(|f| format!("\n  {f}")).collect()
    }
}

impl fmt::Display for SourceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // reqwest's own message repeats the URL, so we say what went wrong
        // underneath it.
        let reason = match &self.error {
            ClientError::NotFound { .. } => "404 Not Found".to_string(),
            ClientError::Status { status, .. } => status.to_string(),
            ClientError::Http(e) if e.is_timeout() => "timed out".to_string(),
            ClientError::Http(e) => match e.source() {
                Some(cause) => render_chain(cause),
                None => e.to_string(),
            },
            other => render_chain(other),
        };
        write!(f, "{}: {reason}", self.url)?;
        if self.attempts > 1 {
            write!(f, " ({} attempts)", self.attempts)?;
        }
        Ok(())
    }
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
