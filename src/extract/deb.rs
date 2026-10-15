//! Debian package (.deb) archive support.
//!
//! A .deb file is an ar archive containing:
//! - debian-binary: Version string (usually "2.0\n")
//! - control.tar.* : Package metadata (control file, scripts, etc.)
//! - data.tar.*    : Package files to install
//!
//! Ref.:

use std::path::Path;
use std::sync::Arc;

use super::ar::ArArchive;
use super::archive::{Archive, ArchiveEntry, ExtractOptions};
use super::error::{ExtractedFile, ExtractionError};
use super::tar::TarArchive;

#[derive(Clone)]
pub struct DebArchive {
    data_tar: TarArchive,
}

impl TryFrom<ArArchive> for DebArchive {
    type Error = ExtractionError;

    fn try_from(ar: ArArchive) -> Result<Self, Self::Error> {
        ar.find_member(|m| m.name == "debian-binary")
            .ok_or_else(|| {
                ExtractionError::InvalidArchive(
                    "not a deb package (missing debian-binary)".to_string(),
                )
            })?;

        let (_, data) = ar
            .find_member(|m| m.name.starts_with("data.tar"))
            .ok_or_else(|| {
                ExtractionError::InvalidArchive("not a deb package (missing data.tar)".to_string())
            })?;

        Ok(Self {
            data_tar: TarArchive::new(data)?,
        })
    }
}

impl DebArchive {
    /// Create a new deb archive from bytes.
    pub fn new(data: impl Into<Arc<[u8]>>) -> Result<Self, ExtractionError> {
        let ar = ArArchive::new(data)?;
        ar.try_into()
    }
}

impl Archive for DebArchive {
    fn entries(&self) -> Result<Vec<ArchiveEntry>, ExtractionError> {
        self.data_tar.entries()
    }

    fn extract_to(
        &self,
        dest: &Path,
        options: &ExtractOptions,
    ) -> Result<Vec<ExtractedFile>, ExtractionError> {
        self.data_tar.extract_to(dest, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid() {
        // Empty data
        assert!(DebArchive::new(Vec::new()).is_err());

        // Valid ar but not a deb
        let mut data = Vec::new();
        data.extend_from_slice(b"!<arch>\n");
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "notdeb/", "0", "0", "0", "644", "5",
        );
        data.extend_from_slice(header.as_bytes());
        data.extend_from_slice(b"hello");
        data.push(b'\n');

        assert!(DebArchive::new(data).is_err());
    }
}
