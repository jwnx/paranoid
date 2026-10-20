//! Fetching locked packages into a content-addressable cache.
//!
//! Downloads run with bounded concurrency, are verified against the lock's
//! SHA-256 before being cached, and cache hits skip the network. Packages
//! come from the manifest's stores first, by hash, then from their
//! repository and its mirrors. A source that doesn't have a package is
//! skipped for the next one, and one that's down is retried first.

mod cache;
mod error;
mod service;

pub use cache::{PackageCache, content_path};
pub use error::{
    DownloadError, DownloadFailures, Fetched, Mismatch, Origin, SourceFailure, WrongBytes,
};
pub use service::{DownloadConfig, DownloadService, Sources};

#[cfg(test)]
pub(crate) mod test_utils {
    use crate::arch::Architecture;
    use crate::hash::Sha256;
    use crate::lock::LockedPackage;

    pub fn make_test_package(name: &str, sha256: Sha256) -> LockedPackage {
        LockedPackage {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            architecture: Architecture::Amd64,
            sha256,
            filename: format!("pool/main/{}/{}_1.0.0_amd64.deb", &name[..1], name),
            size: 1000,
            repository: "test".to_string(),
        }
    }
}
