//! OCI image generation error types.

use std::path::PathBuf;

use thiserror::Error;

use super::digest::Digest;

/// Errors that can occur during OCI image generation.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum OciError {
    /// Failed to create a temporary directory.
    #[error("failed to create temporary directory: {source}")]
    CreateTempDir { source: std::io::Error },

    /// Failed to read source directory.
    #[error("failed to read source directory '{}': {source}", path.display())]
    ReadSourceDir {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to create output directory.
    #[error("failed to create output directory '{}': {source}", path.display())]
    CreateOutputDir {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to write file.
    #[error("failed to write file '{}': {source}", path.display())]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to serialize JSON.
    #[error("failed to serialize JSON: {0}")]
    JsonSerialization(#[from] serde_json::Error),

    /// Source directory does not exist.
    #[error("source directory does not exist: {}", path.display())]
    SourceNotFound { path: PathBuf },

    /// Source path is not a directory.
    #[error("source path is not a directory: {}", path.display())]
    NotADirectory { path: PathBuf },

    /// Failed to create tar layer.
    #[error("failed to create tar layer: {0}")]
    LayerCreation(String),

    /// Failed to compress layer.
    #[error("compression failed: {0}")]
    Compression(String),

    /// Failed to compute relative path.
    #[error("failed to compute relative path for '{}'", path.display())]
    RelativePath { path: PathBuf },
}

/// Result of a successful OCI image generation.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[must_use = "this contains the result of image generation"]
pub struct OciImageResult {
    /// Path to the generated image (directory or tarball).
    pub path: PathBuf,
    /// SHA256 digest of the image manifest.
    pub manifest_digest: Digest,
    /// SHA256 digest of the image config.
    pub config_digest: Digest,
    /// SHA256 digest of the compressed layer.
    pub layer_digest: Digest,
    /// Total size of all blobs in bytes.
    pub total_size: u64,
}
