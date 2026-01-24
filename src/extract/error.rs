//! Extraction error types.

use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur during extraction operations.
#[derive(Debug, Error)]
pub enum ExtractionError {
    /// Generic IO error without path context.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to create a directory.
    #[error("failed to create directory '{}': {source}", path.display())]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to write a file.
    #[error("failed to write file '{}': {source}", path.display())]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to create a symlink.
    #[error("failed to create symlink '{}': {source}", path.display())]
    CreateSymlink {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Failed to create a hard link.
    #[error("failed to create hard link '{}': {source}", path.display())]
    CreateHardlink {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Invalid archive format.
    #[error("invalid archive: {0}")]
    InvalidArchive(String),

    /// Unsupported compression format.
    #[error("unsupported compression format")]
    UnsupportedCompression,

    /// Path traversal attempt detected.
    #[error("path traversal: {reason}")]
    PathTraversal {
        /// The path that attempted traversal.
        path: PathBuf,
        /// Description of the traversal attempt.
        reason: String,
    },

    /// Unsupported entry type in archive.
    #[error("unsupported entry type: {0}")]
    UnsupportedEntryType(&'static str),

    #[error("extraction cancelled")]
    Cancelled,
}

/// Type of archive entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link with target path.
    Symlink { target: PathBuf },
    /// Hard link with target path.
    Hardlink { target: PathBuf },
}

/// A successfully extracted file.
#[derive(Debug, Clone)]
pub struct ExtractedFile {
    /// Path where the file was extracted.
    pub path: PathBuf,
    /// Type of entry.
    pub entry_type: EntryType,
    /// File mode (Unix permissions).
    pub mode: u32,
    /// Size in bytes (0 for directories and links).
    pub size: u64,
}

/// A successfully extracted package.
#[derive(Debug, Clone)]
pub struct ExtractedPackage {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Directory where package was extracted.
    pub extract_dir: PathBuf,
    /// List of extracted files.
    pub files: Vec<ExtractedFile>,
    /// Total bytes extracted.
    pub total_bytes: u64,
}

/// A failed package extraction.
#[derive(Debug)]
pub struct ExtractionFailure {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// The error that occurred.
    pub error: ExtractionError,
}

/// Result of a batch extraction operation.
#[derive(Debug)]
#[must_use = "extraction results should be checked for failures"]
pub struct ExtractionResult {
    /// Successfully extracted packages.
    pub succeeded: Vec<ExtractedPackage>,
    /// Failed extractions.
    pub failed: Vec<ExtractionFailure>,
}

impl ExtractionResult {
    /// Create a new empty extraction result.
    pub const fn new() -> Self {
        Self {
            succeeded: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Check if all extractions succeeded.
    #[must_use]
    pub fn all_succeeded(&self) -> bool {
        self.failed.is_empty()
    }

    /// Get the total number of packages (succeeded + failed).
    #[must_use]
    pub fn total(&self) -> usize {
        self.succeeded.len() + self.failed.len()
    }
}

impl Default for ExtractionResult {
    fn default() -> Self {
        Self::new()
    }
}
