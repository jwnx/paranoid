//! Progress reporting for downloads.

use std::io::Write;

use crate::lock::LockedPackage;

use super::DownloadError;

/// Trait for reporting download progress.
///
/// All methods have default no-op implementations, allowing implementors
/// to only override the methods they care about.
pub trait DownloadProgress: Send + Sync {
    /// Called when a download batch starts.
    fn on_start(&self, _total_packages: usize, _total_bytes: u64) {}

    /// Called when a package download starts.
    fn on_package_start(&self, _package: &LockedPackage) {}

    /// Called when a package download completes successfully.
    fn on_package_complete(&self, _package: &LockedPackage, _from_cache: bool) {}

    /// Called when a package download fails.
    fn on_package_error(&self, _package: &LockedPackage, _error: &DownloadError) {}

    /// Called when the entire download batch completes.
    fn on_complete(&self, _succeeded: usize, _failed: usize) {}
}

/// No-op progress reporter.
pub struct NoProgress;

impl DownloadProgress for NoProgress {}

/// Simple progress reporter that writes to stderr.
pub struct StderrProgress;

impl DownloadProgress for StderrProgress {
    fn on_start(&self, total_packages: usize, total_bytes: u64) {
        let size_mb = total_bytes as f64 / 1_048_576.0;
        eprintln!(
            "Downloading {} packages ({:.2} MB)...",
            total_packages, size_mb
        );
    }

    fn on_package_start(&self, package: &LockedPackage) {
        eprint!("  {} {}... ", package.name, package.version);
        let _ = std::io::stderr().flush();
    }

    fn on_package_complete(&self, _package: &LockedPackage, from_cache: bool) {
        if from_cache {
            eprintln!("cached");
        } else {
            eprintln!("done");
        }
    }

    fn on_package_error(&self, _package: &LockedPackage, error: &DownloadError) {
        eprintln!("FAILED: {}", error);
    }

    fn on_complete(&self, succeeded: usize, failed: usize) {
        if failed == 0 {
            eprintln!("Downloaded {} packages successfully.", succeeded);
        } else {
            eprintln!(
                "Downloaded {} packages, {} failed.",
                succeeded, failed
            );
        }
    }
}
