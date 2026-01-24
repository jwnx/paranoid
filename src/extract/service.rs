//! Extraction service for parallel package extraction.

use std::path::Path;

use futures::stream::{self, StreamExt};

use crate::download::DownloadedPackage;

use super::archive::{Archive, ExtractOptions};
use super::deb::DebArchive;
use super::error::{
    ExtractedFile, ExtractedPackage, ExtractionError, ExtractionFailure, ExtractionResult,
};
use super::progress::ExtractionProgress;

/// How to organize extracted files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractionMode {
    /// Extract each package to its own subdirectory.
    #[default]
    Separate,
    /// Merge all packages into a single directory tree.
    Merged,
}

/// Configuration for the extraction service.
///
/// Use the builder methods for ergonomic construction:
/// ```
/// use paranoid::extract::ExtractionConfig;
///
/// let config = ExtractionConfig::new()
///     .with_max_concurrent(8)
///     .with_preserve_symlinks(false);
/// ```
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    /// Maximum number of concurrent extractions.
    max_concurrent: usize,
    /// How to organize extracted files.
    mode: ExtractionMode,
    /// Whether to preserve Unix permissions.
    preserve_permissions: bool,
    /// Whether to create symbolic links.
    preserve_symlinks: bool,
}

impl ExtractionConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of concurrent extractions.
    pub fn with_max_concurrent(mut self, n: usize) -> Self {
        self.max_concurrent = n;
        self
    }

    /// Set how to organize extracted files.
    pub fn with_mode(mut self, mode: ExtractionMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set whether to preserve Unix permissions.
    pub fn with_preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Set whether to create symbolic links.
    pub fn with_preserve_symlinks(mut self, preserve: bool) -> Self {
        self.preserve_symlinks = preserve;
        self
    }

    /// Get the maximum number of concurrent extractions.
    #[must_use]
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Get the extraction mode.
    #[must_use]
    pub fn mode(&self) -> ExtractionMode {
        self.mode
    }

    /// Get whether permissions are preserved.
    #[must_use]
    pub fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Get whether symlinks are preserved.
    #[must_use]
    pub fn preserve_symlinks(&self) -> bool {
        self.preserve_symlinks
    }
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            mode: ExtractionMode::Separate,
            preserve_permissions: true,
            preserve_symlinks: true,
        }
    }
}

/// Service for extracting packages in parallel.
pub struct ExtractionService {
    config: ExtractionConfig,
}

impl ExtractionService {
    pub fn new() -> Self {
        Self::with_config(ExtractionConfig::default())
    }

    pub fn with_config(config: ExtractionConfig) -> Self {
        Self { config }
    }

    /// Extract all downloaded packages.
    ///
    /// Uses bounded parallelism and reports progress via the provided reporter.
    #[must_use = "extraction results should be checked for failures"]
    pub async fn extract_all<P: ExtractionProgress>(
        &self,
        packages: &[DownloadedPackage],
        destination: &Path,
        progress: &P,
    ) -> ExtractionResult {
        progress.on_start(packages.len());

        if let Err(e) = tokio::fs::create_dir_all(destination).await {
            // If we can't create the destination, fail all packages
            let mut result = ExtractionResult::new();
            let error_msg = e.to_string();
            for pkg in packages {
                result.failed.push(ExtractionFailure {
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    error: ExtractionError::Io(std::io::Error::new(e.kind(), error_msg.clone())),
                });
            }
            progress.on_complete(0, packages.len());
            return result;
        }

        let extract_futures = packages
            .iter()
            .map(|package| self.extract_package(package, destination, progress));

        let results: Vec<Result<ExtractedPackage, ExtractionFailure>> =
            stream::iter(extract_futures)
                .buffer_unordered(self.config.max_concurrent())
                .collect()
                .await;

        let mut extraction_result = ExtractionResult::new();
        for result in results {
            match result {
                Ok(pkg) => extraction_result.succeeded.push(pkg),
                Err(failure) => extraction_result.failed.push(failure),
            }
        }

        progress.on_complete(
            extraction_result.succeeded.len(),
            extraction_result.failed.len(),
        );

        extraction_result
    }

    async fn extract_package<P: ExtractionProgress>(
        &self,
        package: &DownloadedPackage,
        destination: &Path,
        progress: &P,
    ) -> Result<ExtractedPackage, ExtractionFailure> {
        // Check for cancellation before starting
        if progress.should_cancel() {
            return Err(ExtractionFailure {
                name: package.name.clone(),
                version: package.version.clone(),
                error: ExtractionError::Cancelled,
            });
        }

        progress.on_package_start(package);

        let result = self.extract_package_inner(package, destination).await;

        match &result {
            Ok(extracted) => {
                progress.on_package_complete(package, extracted.files.len());
            }
            Err(error) => {
                progress.on_package_error(package, error);
            }
        }

        result.map_err(|error| ExtractionFailure {
            name: package.name.clone(),
            version: package.version.clone(),
            error,
        })
    }

    /// Inner extraction logic without progress reporting.
    async fn extract_package_inner(
        &self,
        package: &DownloadedPackage,
        destination: &Path,
    ) -> Result<ExtractedPackage, ExtractionError> {
        // Read the package file
        let data = tokio::fs::read(&package.path).await?;

        // Parse as deb (data is moved into the archive)
        let deb = DebArchive::new(data)?;

        // Determine extraction directory
        let extract_dir = match self.config.mode() {
            ExtractionMode::Separate => {
                destination.join(format!("{}-{}", package.name, package.version))
            }
            ExtractionMode::Merged => destination.to_path_buf(),
        };
        let result_dir = extract_dir.clone();

        // Create extraction options
        let options = ExtractOptions {
            preserve_permissions: self.config.preserve_permissions(),
            preserve_symlinks: self.config.preserve_symlinks(),
            overwrite: self.config.mode() == ExtractionMode::Merged,
        };

        // Extract (blocking operation, run in spawn_blocking)
        let files = tokio::task::spawn_blocking(move || deb.extract_to(&extract_dir, &options))
            .await
            .map_err(|e| ExtractionError::Io(std::io::Error::other(e)))??;

        let total_bytes = files.iter().map(|f| f.size).sum();

        Ok(ExtractedPackage {
            name: package.name.clone(),
            version: package.version.clone(),
            extract_dir: result_dir,
            files,
            total_bytes,
        })
    }

    /// Extract a single archive of any type.
    pub fn extract_archive<A: Archive>(
        &self,
        archive: &A,
        destination: &Path,
    ) -> Result<Vec<ExtractedFile>, ExtractionError> {
        let options = ExtractOptions {
            preserve_permissions: self.config.preserve_permissions(),
            preserve_symlinks: self.config.preserve_symlinks(),
            overwrite: false,
        };

        archive.extract_to(destination, &options)
    }

    /// Get the configuration.
    pub fn config(&self) -> &ExtractionConfig {
        &self.config
    }
}

impl Default for ExtractionService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extraction_config_default() {
        let config = ExtractionConfig::default();
        assert_eq!(config.max_concurrent(), 4);
        assert_eq!(config.mode(), ExtractionMode::Separate);
        assert!(config.preserve_permissions());
        assert!(config.preserve_symlinks());
    }

    #[test]
    fn test_extraction_config_builder() {
        let config = ExtractionConfig::new()
            .with_max_concurrent(8)
            .with_mode(ExtractionMode::Merged)
            .with_preserve_symlinks(false);
        assert_eq!(config.max_concurrent(), 8);
        assert_eq!(config.mode(), ExtractionMode::Merged);
        assert!(config.preserve_permissions());
        assert!(!config.preserve_symlinks());
    }

    #[test]
    fn test_extraction_result_default() {
        let result = ExtractionResult::default();
        assert!(result.all_succeeded());
        assert_eq!(result.total(), 0);
    }
}
