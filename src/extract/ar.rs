//! Unix ar archive support.
//!
//! This module parses the ar archive format used by .deb packages.
//! The ar format is simple:
//! - 8-byte magic: "!<arch>\n"
//! - Sequence of members, each with:
//!   - 60-byte header
//!   - File content (padded to even length)

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::extract::path::validate_path;

use super::archive::{Archive, ArchiveEntry, ExtractOptions};
use super::error::{EntryType, ExtractedFile, ExtractionError};

/// Magic bytes for ar archive.
const AR_MAGIC: &[u8] = b"!<arch>\n";
/// Size of ar member header.
const HEADER_SIZE: usize = 60;

/// A member (file) within an ar archive.
#[derive(Debug, Clone)]
pub struct ArMember {
    /// Member name (may be truncated in archive).
    pub name: String,
    /// File mode.
    pub mode: u32,
    /// File size.
    pub size: u64,
    /// Offset of file content in the archive data.
    pub(crate) offset: usize,
}

#[derive(Clone)]
pub struct ArArchive {
    data: Arc<[u8]>,
    members: Vec<ArMember>,
}

impl ArArchive {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Result<Self, ExtractionError> {
        let data: Arc<[u8]> = data.into();

        // Check magic (8-byte "!<arch>\n" signature)
        if data.len() < AR_MAGIC.len() || &data[..AR_MAGIC.len()] != AR_MAGIC {
            return Err(ExtractionError::InvalidArchive(
                "not an ar archive (invalid magic)".to_string(),
            ));
        }

        let mut members = Vec::new();
        let mut pos = AR_MAGIC.len();

        while pos + HEADER_SIZE <= data.len() {
            let header = &data[pos..pos + HEADER_SIZE];

            // Header format (60 bytes total):
            // name[16] mtime[12] uid[6] gid[6] mode[8] size[10] magic[2]
            let name = Self::parse_name(&header[0..16])?;
            let mode = Self::parse_octal(&header[40..48])?;
            let size = Self::parse_decimal(&header[48..58])?;

            // Each member header ends with "`\n" (0x60 0x0a)
            if &header[58..60] != b"`\n" {
                return Err(ExtractionError::InvalidArchive(
                    "invalid ar member header magic".to_string(),
                ));
            }

            let content_offset = pos + HEADER_SIZE;

            // Convert size to usize with overflow check (matters on 32-bit platforms)
            let size_usize = usize::try_from(size).map_err(|_| {
                ExtractionError::InvalidArchive("ar member size too large for platform".to_string())
            })?;

            // Verify we have enough data (with overflow check)
            let content_end = content_offset.checked_add(size_usize).ok_or_else(|| {
                ExtractionError::InvalidArchive("ar member offset overflow".to_string())
            })?;

            if content_end > data.len() {
                return Err(ExtractionError::InvalidArchive(
                    "ar member extends beyond archive".to_string(),
                ));
            }

            members.push(ArMember {
                name,
                mode,
                size,
                offset: content_offset,
            });

            // Move to next member (aligned to 2 bytes)
            pos = content_end;
            if pos % 2 == 1 {
                pos += 1;
            }
        }

        Ok(Self { data, members })
    }

    // String slices are to be considered utf8, but still.
    fn parse_utf8(bytes: &[u8]) -> Result<&str, ExtractionError> {
        std::str::from_utf8(bytes).map_err(|_| {
            ExtractionError::InvalidArchive("invalid UTF-8 in ar member name".to_string())
        })
    }

    fn parse_name(bytes: &[u8]) -> Result<String, ExtractionError> {
        let s = Self::parse_utf8(bytes)?;

        // Name is space-padded and may end with /
        let name = s.trim_end().trim_end_matches('/');

        Ok(name.to_string())
    }

    fn parse_octal(bytes: &[u8]) -> Result<u32, ExtractionError> {
        let s = Self::parse_utf8(bytes)?.trim();

        if s.is_empty() {
            return Ok(0);
        }

        u32::from_str_radix(s, 8).map_err(|_| {
            ExtractionError::InvalidArchive(format!("invalid octal in ar header: {}", s))
        })
    }

    fn parse_decimal(bytes: &[u8]) -> Result<u64, ExtractionError> {
        let s = Self::parse_utf8(bytes)?.trim();

        if s.is_empty() {
            return Ok(0);
        }

        s.parse().map_err(|_| {
            ExtractionError::InvalidArchive(format!("invalid decimal in ar header: {}", s))
        })
    }

    pub fn members(&self) -> &[ArMember] {
        &self.members
    }

    pub fn find_member<F>(&self, predicate: F) -> Option<(&ArMember, &[u8])>
    where
        F: Fn(&ArMember) -> bool,
    {
        self.members.iter().find(|m| predicate(m)).map(|m| {
            let start = m.offset;
            let end = start + m.size as usize;
            (m, &self.data[start..end])
        })
    }
}

impl Archive for ArArchive {
    fn entries(&self) -> Result<Vec<ArchiveEntry>, ExtractionError> {
        Ok(self
            .members
            .iter()
            .map(|m| ArchiveEntry {
                path: m.name.clone().into(),
                entry_type: EntryType::Regular,
                size: m.size,
                mode: m.mode,
            })
            .collect())
    }

    fn extract_to(
        &self,
        dest: &Path,
        options: &ExtractOptions,
    ) -> Result<Vec<ExtractedFile>, ExtractionError> {
        fs::create_dir_all(dest).map_err(|e| ExtractionError::CreateDirectory {
            path: dest.to_path_buf(),
            source: e,
        })?;
        let mut extracted = Vec::new();

        for member in &self.members {
            let full_path = validate_path(Path::new(&member.name), dest)?;

            // Check overwrite
            if full_path.exists() && !options.overwrite {
                continue;
            }

            // Get content
            let content = &self.data[member.offset..member.offset + member.size as usize];

            // Write file
            let mut file = File::create(&full_path).map_err(|e| ExtractionError::WriteFile {
                path: full_path.clone(),
                source: e,
            })?;
            file.write_all(content)
                .map_err(|e| ExtractionError::WriteFile {
                    path: full_path.clone(),
                    source: e,
                })?;

            #[cfg(unix)]
            if options.preserve_permissions {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&full_path, fs::Permissions::from_mode(member.mode))?;
            }

            extracted.push(ExtractedFile {
                path: full_path,
                entry_type: EntryType::Regular,
                mode: member.mode,
                size: member.size,
            });
        }

        Ok(extracted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Create a minimal ar archive for testing
    fn make_test_archive() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"!<arch>\n");

        // Member: "test.txt" with content "hello"
        // Header: name[16] mtime[12] uid[6] gid[6] mode[8] size[10] magic[2]
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "test.txt/", // name
            "0",         // mtime
            "0",         // uid
            "0",         // gid
            "644",       // mode (octal)
            "5",         // size
        );
        assert_eq!(header.len(), 60);
        data.extend_from_slice(header.as_bytes());
        data.extend_from_slice(b"hello");
        data.push(b'\n'); // padding to even length

        data
    }

    #[test]
    fn test_parse_ar() {
        let data = make_test_archive();
        let ar = ArArchive::new(data).unwrap();
        assert_eq!(ar.members().len(), 1);
        assert_eq!(ar.members()[0].name, "test.txt");
        assert_eq!(ar.members()[0].size, 5);
    }

    #[test]
    fn test_find_member() {
        let data = make_test_archive();
        let ar = ArArchive::new(data).unwrap();
        let (member, content) = ar.find_member(|m| m.name == "test.txt").unwrap();
        assert_eq!(member.name, "test.txt");
        assert_eq!(content, b"hello");
    }

    #[test]
    fn test_invalid_magic() {
        let data = b"not an ar archive".to_vec();
        assert!(ArArchive::new(data).is_err());
    }
}
