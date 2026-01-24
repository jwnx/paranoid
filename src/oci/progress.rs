//! Progress reporting for OCI image generation.

use std::io::Write;

/// Trait for reporting OCI image generation progress.
///
/// All methods have default no-op implementations, allowing implementors
/// to only override the methods they care about.
pub trait OciProgress: Send + Sync {
    /// Called when image generation starts.
    fn on_start(&self) {}

    /// Called when layer creation starts.
    fn on_layer_start(&self) {}

    /// Called when layer creation completes.
    fn on_layer_complete(&self, _size: u64) {}

    /// Called when config generation starts.
    fn on_config_start(&self) {}

    /// Called when config generation completes.
    fn on_config_complete(&self) {}

    /// Called when manifest generation starts.
    fn on_manifest_start(&self) {}

    /// Called when manifest generation completes.
    fn on_manifest_complete(&self) {}

    /// Called when writing blobs to disk.
    fn on_writing_blobs(&self) {}

    /// Called when creating the final tarball.
    fn on_creating_tarball(&self) {}

    /// Called when image generation completes successfully.
    fn on_complete(&self, _digest: &str, _total_size: u64) {}

    /// Called when an error occurs.
    fn on_error(&self, _error: &str) {}
}

/// No-op progress reporter.
pub struct NoProgress;

impl OciProgress for NoProgress {}

/// Simple progress reporter that writes to stderr.
pub struct StderrProgress;

impl OciProgress for StderrProgress {
    fn on_start(&self) {
        eprint!("Generating OCI image... ");
        let _ = std::io::stderr().flush();
    }

    fn on_layer_start(&self) {
        eprint!("creating layer... ");
        let _ = std::io::stderr().flush();
    }

    fn on_layer_complete(&self, size: u64) {
        let size_mb = size as f64 / 1_048_576.0;
        eprint!("{:.2} MB... ", size_mb);
        let _ = std::io::stderr().flush();
    }

    fn on_config_start(&self) {
        eprint!("config... ");
        let _ = std::io::stderr().flush();
    }

    fn on_config_complete(&self) {
        // Config is fast, no need to print completion
    }

    fn on_manifest_start(&self) {
        eprint!("manifest... ");
        let _ = std::io::stderr().flush();
    }

    fn on_manifest_complete(&self) {
        // Manifest is fast, no need to print completion
    }

    fn on_writing_blobs(&self) {
        eprint!("writing blobs... ");
        let _ = std::io::stderr().flush();
    }

    fn on_creating_tarball(&self) {
        eprint!("creating tarball... ");
        let _ = std::io::stderr().flush();
    }

    fn on_complete(&self, digest: &str, total_size: u64) {
        let size_mb = total_size as f64 / 1_048_576.0;
        eprintln!("done!");
        eprintln!("  Digest: {}", digest);
        eprintln!("  Size: {:.2} MB", size_mb);
    }

    fn on_error(&self, error: &str) {
        eprintln!("FAILED: {}", error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_progress_compiles() {
        let progress = NoProgress;
        progress.on_start();
        progress.on_layer_start();
        progress.on_layer_complete(1000);
        progress.on_config_start();
        progress.on_config_complete();
        progress.on_manifest_start();
        progress.on_manifest_complete();
        progress.on_writing_blobs();
        progress.on_complete("sha256:abc", 5000);
        progress.on_error("test error");
    }
}
