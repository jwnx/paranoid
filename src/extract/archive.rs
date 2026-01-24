//! Archive trait and common types.

use std::path::{Path, PathBuf};

use super::error::{EntryType, ExtractedFile, ExtractionError};

/// Metadata about an archive entry.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Path within the archive.
    pub path: PathBuf,
    /// Type of entry.
    pub entry_type: EntryType,
    /// Size in bytes.
    pub size: u64,
    /// File mode (Unix permissions).
    pub mode: u32,
}

/// Options for archive extraction operations.
///
/// These options control low-level extraction behavior for individual archives.
/// For service-level configuration, see `ExtractionConfig`.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Whether to preserve Unix permissions.
    pub preserve_permissions: bool,
    /// Whether to create symbolic links (if false, skips them).
    pub preserve_symlinks: bool,
    /// Whether to overwrite existing files.
    pub overwrite: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            preserve_permissions: true,
            preserve_symlinks: true,
            overwrite: false,
        }
    }
}

/// Trait for archive formats that can be extracted.
pub trait Archive {
    /// List entries in the archive without extracting.
    #[must_use = "this returns the entries without modifying the archive"]
    fn entries(&self) -> Result<Vec<ArchiveEntry>, ExtractionError>;

    /// Extract all entries to destination directory.
    #[must_use = "extraction results should be checked for errors"]
    fn extract_to(
        &self,
        dest: &Path,
        options: &ExtractOptions,
    ) -> Result<Vec<ExtractedFile>, ExtractionError>;
}
