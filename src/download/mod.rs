//! Download manager for fetching packages.
//!
//! This module provides:
//! - Parallel HTTP downloads with bounded concurrency
//! - SHA256 verification before caching
//! - Content-addressable cache (`~/.cache/paranoid/sha256/`)
//! - Progress reporting via trait
//! - Cache hits (skip already-downloaded packages)

mod cache;
mod error;
mod progress;
mod service;

pub use cache::PackageCache;
pub use error::{DownloadError, DownloadFailure, DownloadResult, DownloadedPackage};
pub use progress::{DownloadProgress, NoProgress, StderrProgress};
pub use service::{DownloadConfig, DownloadService};

#[cfg(test)]
pub(crate) mod test_utils {
    use crate::arch::Architecture;
    use crate::lock::{LockedPackage, Sha256Hash};

    pub fn make_test_package(name: &str, sha256: &str) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            architecture: Architecture::Amd64,
            // Use from_trusted for test data (may not be valid 64-char hex)
            sha256: Sha256Hash::from_trusted(sha256),
            filename: format!("pool/main/{}/{}_1.0.0_amd64.deb", &name[..1], name),
            size: 1000,
            repository: "test".to_string(),
            direct: true,
        }
    }
}
