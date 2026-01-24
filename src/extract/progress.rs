//! Progress reporting for extraction.

use std::io::Write;

use crate::download::DownloadedPackage;

use super::error::ExtractionError;

pub trait ExtractionProgress: Send + Sync {
    /// Called when an extraction batch starts.
    fn on_start(&self, _total_packages: usize) {}

    /// Called when a package extraction starts.
    fn on_package_start(&self, _package: &DownloadedPackage) {}

    /// Called when a package extraction completes successfully.
    fn on_package_complete(&self, _package: &DownloadedPackage, _files_extracted: usize) {}

    /// Called when a package extraction fails.
    fn on_package_error(&self, _package: &DownloadedPackage, _error: &ExtractionError) {}

    /// Called when the entire extraction batch completes.
    fn on_complete(&self, _succeeded: usize, _failed: usize) {}

    /// Check if the extraction should be cancelled.
    ///
    /// Called before starting each package. If this returns `true`,
    /// remaining packages will be skipped.
    fn should_cancel(&self) -> bool {
        false
    }
}

/// No-op progress reporter.
pub struct NoProgress;

impl ExtractionProgress for NoProgress {}

/// Simple progress reporter that writes to stderr.
pub struct StderrProgress;

impl ExtractionProgress for StderrProgress {
    fn on_start(&self, total_packages: usize) {
        eprintln!("Extracting {} packages...", total_packages);
    }

    fn on_package_start(&self, package: &DownloadedPackage) {
        eprint!("  {} {}... ", package.name, package.version);
        // Ignore flush errors - nothing useful we can do with stderr failures
        let _ = std::io::stderr().flush();
    }

    fn on_package_complete(&self, _package: &DownloadedPackage, files_extracted: usize) {
        eprintln!("done ({} files)", files_extracted);
    }

    fn on_package_error(&self, _package: &DownloadedPackage, error: &ExtractionError) {
        eprintln!("FAILED: {}", error);
    }

    fn on_complete(&self, succeeded: usize, failed: usize) {
        if failed == 0 {
            eprintln!("Extracted {} packages successfully.", succeeded);
        } else {
            eprintln!("Extracted {} packages, {} failed.", succeeded, failed);
        }
    }
}
