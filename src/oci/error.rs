//! OCI image generation error types.

use std::path::PathBuf;

use thiserror::Error;

use super::digest::Digest;

/// Errors that can occur during OCI image generation.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum OciError {
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

    /// Failed to read an existing image.
    #[error("failed to read image '{}'", path.display())]
    ReadImage {
        path: PathBuf,
        source: std::io::Error,
    },

    /// An existing image is malformed.
    #[error("invalid image: {0}")]
    InvalidImage(String),

    /// Failed to serialize JSON.
    #[error("failed to serialize JSON: {0}")]
    JsonSerialization(#[from] serde_json::Error),

    /// Failed to create tar layer.
    #[error("failed to create tar layer: {0}")]
    LayerCreation(String),

    /// Failed to compress layer.
    #[error("compression failed: {0}")]
    Compression(String),
}

/// Result of a successful OCI image generation.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[must_use = "this contains the result of image generation"]
pub struct OciImageResult {
    /// Path to the generated image (directory or tarball).
    pub path: PathBuf,
    /// The image reference recorded in the index, `name:tag` or `tag`.
    pub reference: String,
    /// SHA256 digest of the image manifest.
    pub manifest_digest: Digest,
    /// SHA256 digest of the image config.
    pub config_digest: Digest,
    /// SHA256 digest of the compressed layer.
    pub layer_digest: Digest,
    /// Total size of all blobs in bytes.
    pub total_size: u64,
}
