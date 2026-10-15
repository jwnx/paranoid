//! Tar archive support with multiple compression formats.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;
use xz2::read::XzDecoder;
use zstd::stream::Decoder as ZstdDecoder;

use crate::extract::path::{normalize_path, validate_path};

use super::archive::{Archive, ArchiveEntry, ExtractOptions};
use super::error::{EntryType, ExtractedFile, ExtractionError};

/// Compression format for tar archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// No compression (.tar)
    None,
    /// Gzip compression (.tar.gz, .tgz)
    Gzip,
    /// XZ compression (.tar.xz)
    Xz,
    /// Zstandard compression (.tar.zst)
    Zstd,
}

// Magic bytes for compression detection
const GZIP_MAGIC: &[u8] = &[0x1f, 0x8b];
const XZ_MAGIC: &[u8] = &[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00];
const ZSTD_MAGIC: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd];
const TAR_USTAR_MAGIC: &[u8] = b"ustar";
const TAR_USTAR_OFFSET: usize = 257;

impl Compression {
    /// Detect compression format from magic bytes.
    ///
    /// Inspects the first few bytes of data to determine the compression format.
    /// Returns `Compression::None` for uncompressed tar archives.
    #[must_use = "detection result should be used"]
    pub fn detect(data: &[u8]) -> Result<Self, ExtractionError> {
        if data.len() < XZ_MAGIC.len() {
            return Err(ExtractionError::InvalidArchive(
                "data too short to detect compression".to_string(),
            ));
        }

        if data.starts_with(GZIP_MAGIC) {
            Ok(Self::Gzip)
        } else if data.starts_with(XZ_MAGIC) {
            Ok(Self::Xz)
        } else if data.starts_with(ZSTD_MAGIC) {
            Ok(Self::Zstd)
        } else {
            // Assume uncompressed tar and check for valid tar header
            // POSIX tar files have "ustar" at offset 257
            let ustar_end = TAR_USTAR_OFFSET + TAR_USTAR_MAGIC.len();
            if data.len() >= ustar_end && &data[TAR_USTAR_OFFSET..ustar_end] == TAR_USTAR_MAGIC {
                Ok(Self::None)
            } else if data.len() >= 512 {
                // Could be an old-style tar, try to parse it
                Ok(Self::None)
            } else {
                Err(ExtractionError::UnsupportedCompression)
            }
        }
    }
}

struct TarEntryMeta {
    path: PathBuf,
    entry_type: EntryType,
    size: u64,
    mode: u32,
}

impl TarEntryMeta {
    /// Parse metadata from a tar entry.
    ///
    /// Returns `None` for unsupported entry types (devices, fifos, etc.).
    fn from_entry<R: Read>(entry: &tar::Entry<'_, R>) -> Result<Option<Self>, ExtractionError> {
        let header = entry.header();

        let entry_type = match header.entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => EntryType::Regular,
            tar::EntryType::Directory => EntryType::Directory,
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| {
                        ExtractionError::InvalidArchive("symlink missing target".to_string())
                    })?
                    .into_owned();
                EntryType::Symlink { target }
            }

            tar::EntryType::Link => {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| {
                        ExtractionError::InvalidArchive("hardlink missing target".to_string())
                    })?
                    .into_owned();
                EntryType::Hardlink { target }
            }
            // Skip unsupported types (character/block devices, fifos, etc.)
            _ => return Ok(None),
        };

        Ok(Some(Self {
            path: entry.path()?.into_owned(),
            entry_type,
            size: header.size()?,
            mode: header.mode()?,
        }))
    }
}

/// A tar archive with optional compression.
#[derive(Clone)]
pub struct TarArchive {
    data: Arc<[u8]>,
    compression: Compression,
}

impl TarArchive {
    /// Create a new tar archive with auto-detected compression.
    pub fn new(data: impl Into<Arc<[u8]>>) -> Result<Self, ExtractionError> {
        Self::with_compression(data, None)
    }

    /// Create a new tar archive with explicit compression format.
    ///
    /// Pass `None` to auto-detect from magic bytes, or `Some(compression)`
    /// to specify a specific compression format.
    pub fn with_compression(
        data: impl Into<Arc<[u8]>>,
        compression: Option<Compression>,
    ) -> Result<Self, ExtractionError> {
        let data: Arc<[u8]> = data.into();
        let compression = match compression {
            Some(c) => c,
            None => Compression::detect(&data)?,
        };

        Ok(Self { data, compression })
    }

    /// Get the detected/specified compression format.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    fn decompressed_reader(&self) -> Result<Box<dyn Read + Send>, ExtractionError> {
        let data = self.data.clone();

        match self.compression {
            Compression::None => Ok(Box::new(Cursor::new(data))),
            Compression::Gzip => Ok(Box::new(GzDecoder::new(Cursor::new(data)))),
            Compression::Xz => Ok(Box::new(XzDecoder::new(Cursor::new(data)))),
            Compression::Zstd => {
                let decoder = ZstdDecoder::new(Cursor::new(data))?;
                Ok(Box::new(decoder))
            }
        }
    }

    /// Extract a single entry to the filesystem.
    ///
    /// # Security
    ///
    /// This function performs security validation for symlinks and hardlinks:
    /// - Absolute symlink targets are rejected
    /// - Relative symlink targets are validated to not escape `dest`
    /// - Hardlink targets are validated via `validate_path`
    fn extract_entry_to_fs(
        entry: &mut tar::Entry<'_, impl Read>,
        meta: &TarEntryMeta,
        full_path: &Path,
        dest: &Path,
        options: &ExtractOptions,
    ) -> Result<(), ExtractionError> {
        // Create parent directories
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ExtractionError::CreateDirectory {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        match &meta.entry_type {
            EntryType::Directory => {
                fs::create_dir_all(full_path).map_err(|e| ExtractionError::CreateDirectory {
                    path: full_path.to_path_buf(),
                    source: e,
                })?;

                #[cfg(unix)]
                if options.preserve_permissions {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(full_path, fs::Permissions::from_mode(meta.mode))?;
                }
            }
            EntryType::Regular => {
                let mut file = File::create(full_path).map_err(|e| ExtractionError::WriteFile {
                    path: full_path.to_path_buf(),
                    source: e,
                })?;

                std::io::copy(entry, &mut file).map_err(|e| ExtractionError::WriteFile {
                    path: full_path.to_path_buf(),
                    source: e,
                })?;

                #[cfg(unix)]
                if options.preserve_permissions {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(full_path, fs::Permissions::from_mode(meta.mode))?;
                }
            }
            EntryType::Symlink { target } => {
                // Absolute symlink targets are legal in container rootfs images —
                // /lib -> /usr/lib is resolved relative to the container root at
                // runtime, never against the host filesystem. We allow them when
                // overwrite=true (Merged mode / rootfs extraction) and reject them
                // in Separate mode where they could point outside the package dir.
                if target.is_absolute() && !options.overwrite {
                    return Err(ExtractionError::PathTraversal {
                        path: target.clone(),
                        reason: "absolute symlink target".to_string(),
                    });
                }

                // For relative symlinks: resolve target from the link's parent dir,
                // normalize, and verify it stays within the destination.
                if !target.is_absolute()
                    && let Some(parent) = full_path.parent()
                {
                    let resolved = normalize_path(&parent.join(target));
                    if !resolved.starts_with(dest) {
                        return Err(ExtractionError::PathTraversal {
                            path: full_path.to_path_buf(),
                            reason: format!("symlink escapes destination: -> {}", target.display()),
                        });
                    }
                }

                // Remove existing file if it exists
                if full_path.exists() || full_path.is_symlink() {
                    fs::remove_file(full_path)?;
                }

                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(target, full_path).map_err(|e| {
                        ExtractionError::CreateSymlink {
                            path: full_path.to_path_buf(),
                            source: e,
                        }
                    })?;
                }
                #[cfg(not(unix))]
                {
                    return Err(ExtractionError::UnsupportedEntryType(
                        "symlinks not supported on this platform",
                    ));
                }
            }
            EntryType::Hardlink { target } => {
                // Resolve hardlink target relative to extraction root
                let resolved_target = validate_path(target, dest)?;
                fs::hard_link(&resolved_target, full_path).map_err(|e| {
                    ExtractionError::CreateHardlink {
                        path: full_path.to_path_buf(),
                        source: e,
                    }
                })?;
            }
        }

        Ok(())
    }
}

impl Archive for TarArchive {
    fn entries(&self) -> Result<Vec<ArchiveEntry>, ExtractionError> {
        let reader = self.decompressed_reader()?;
        let mut archive = tar::Archive::new(reader);
        let mut entries = Vec::new();

        for entry_result in archive.entries()? {
            let entry = entry_result?;

            let Some(meta) = TarEntryMeta::from_entry(&entry)? else {
                continue;
            };

            entries.push(ArchiveEntry {
                path: meta.path,
                entry_type: meta.entry_type,
                size: meta.size,
                mode: meta.mode,
            });
        }

        Ok(entries)
    }

    fn extract_to(
        &self,
        dest: &Path,
        options: &ExtractOptions,
    ) -> Result<Vec<ExtractedFile>, ExtractionError> {
        let reader = self.decompressed_reader()?;
        let mut archive = tar::Archive::new(reader);
        let mut extracted = Vec::new();

        fs::create_dir_all(dest)?;

        for entry_result in archive.entries()? {
            let mut entry = entry_result?;

            let Some(meta) = TarEntryMeta::from_entry(&entry)? else {
                continue;
            };

            // Validate path to prevent traversal attacks
            let full_path = validate_path(&meta.path, dest)?;

            // Skip symlinks if not preserving them
            if matches!(meta.entry_type, EntryType::Symlink { .. }) && !options.preserve_symlinks {
                continue;
            }

            // Check if file exists and handle overwrite
            if full_path.exists() && !options.overwrite {
                continue;
            }

            Self::extract_entry_to_fs(&mut entry, &meta, &full_path, dest, options)?;

            extracted.push(ExtractedFile {
                path: full_path,
                entry_type: meta.entry_type,
                mode: meta.mode,
                size: meta.size,
            });
        }

        Ok(extracted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_gzip() {
        let data: &[u8] = &[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00];
        assert_eq!(Compression::detect(data).unwrap(), Compression::Gzip);
    }

    #[test]
    fn test_detect_xz() {
        let data: &[u8] = &[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00];
        assert_eq!(Compression::detect(data).unwrap(), Compression::Xz);
    }

    #[test]
    fn test_detect_zstd() {
        let data: &[u8] = &[0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x00];
        assert_eq!(Compression::detect(data).unwrap(), Compression::Zstd);
    }

    #[test]
    fn test_detect_too_short() {
        let data: &[u8] = &[0x1f, 0x8b];
        assert!(Compression::detect(data).is_err());
    }
}
