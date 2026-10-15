//! User-facing CLI errors with clear, actionable messages.

use std::fmt;
use std::path::Path;

/// A top-level CLI error shown to the user before the process exits.
pub struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CliError {}

impl CliError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }

    pub fn manifest(e: impl fmt::Display) -> Self {
        Self::new(format!("could not load manifest: {}", e))
    }

    pub fn resolve(e: impl fmt::Display) -> Self {
        Self::new(format!("dependency resolution failed: {}", e))
    }

    pub fn lock(e: impl fmt::Display) -> Self {
        Self::new(format!("lock file error: {}", e))
    }

    pub fn download(e: impl fmt::Display) -> Self {
        Self::new(format!("download error: {}", e))
    }

    pub fn download_failures(n: usize) -> Self {
        Self::new(format!(
            "{} package download{} failed (see errors above)",
            n,
            if n == 1 { "" } else { "s" }
        ))
    }

    pub fn extract_failures(n: usize) -> Self {
        Self::new(format!(
            "{} package extraction{} failed (see errors above)",
            n,
            if n == 1 { "" } else { "s" }
        ))
    }

    pub fn oci(e: impl fmt::Display) -> Self {
        Self::new(format!("OCI image generation failed: {}", e))
    }

    pub fn io(e: impl fmt::Display) -> Self {
        Self::new(format!("IO error: {}", e))
    }

    pub fn missing_lock(path: &Path) -> Self {
        Self::new(format!(
            "lock file '{}' not found — run `paranoid lock` first",
            path.display()
        ))
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Self::new(format!("manifest configuration error: {}", msg.into()))
    }

    pub fn inspect_image(msg: impl fmt::Display) -> Self {
        Self::new(format!("could not inspect image: {}", msg))
    }
}
