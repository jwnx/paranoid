//! Reading Debian binary packages (`.deb`).
//!
//! A `.deb` is an ar archive holding, in order:
//! - `debian-binary`: the format version, `2.0\n`
//! - `control.tar[.gz|.xz|.zst]`: package metadata and maintainer scripts
//! - `data.tar[.gz|.xz|.zst]`: the files to install
//!
//! We only ever need `data.tar`, and we never write it to disk: callers get
//! a decompressing reader and feed it into a [`crate::rootfs::Rootfs`].
//!
//! Ref.: deb(5), <https://manpages.debian.org/bookworm/dpkg-dev/deb.5.en.html>

mod ar;

use std::io::{self, Read};

use flate2::read::GzDecoder;
use thiserror::Error;
use xz2::read::XzDecoder;

pub use ar::{ArArchive, ArMember};

/// Errors reading a `.deb`.
#[derive(Debug, Error)]
pub enum DebError {
    #[error("invalid package archive: {0}")]
    Invalid(String),

    #[error("unsupported data.tar compression: {0}")]
    UnsupportedCompression(String),

    #[error("failed to read package data")]
    Io(#[from] io::Error),
}

/// How a `data.tar` member is compressed, decided by its name like dpkg does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Xz,
    Zstd,
}

/// Member-name suffix to compression, the formats dpkg accepts for `data.tar`.
const DATA_TAR_MEMBERS: &[(&str, Compression)] = &[
    ("data.tar", Compression::None),
    ("data.tar.gz", Compression::Gzip),
    ("data.tar.xz", Compression::Xz),
    ("data.tar.zst", Compression::Zstd),
];

/// A parsed `.deb`, borrowing the package bytes.
pub struct DebArchive<'a> {
    data_tar: &'a [u8],
    compression: Compression,
}

impl<'a> DebArchive<'a> {
    /// Parse a `.deb`, checking its format version and locating `data.tar`.
    pub fn new(bytes: &'a [u8]) -> Result<Self, DebError> {
        let ar = ArArchive::new(bytes)?;

        let (_, version) = ar
            .find_member(|m| m.name == "debian-binary")
            .ok_or_else(|| DebError::Invalid("missing debian-binary".to_string()))?;
        if !version.starts_with(b"2.") {
            return Err(DebError::Invalid(format!(
                "unsupported format version {:?}",
                String::from_utf8_lossy(version).trim()
            )));
        }

        let (member, data_tar) = ar
            .find_member(|m| m.name.starts_with("data.tar"))
            .ok_or_else(|| DebError::Invalid("missing data.tar".to_string()))?;
        let compression = DATA_TAR_MEMBERS
            .iter()
            .find(|(name, _)| *name == member.name)
            .map(|&(_, c)| c)
            .ok_or_else(|| DebError::UnsupportedCompression(member.name.clone()))?;

        Ok(Self {
            data_tar,
            compression,
        })
    }

    /// A reader over the decompressed `data.tar`.
    pub fn data_tar(&self) -> Result<Box<dyn Read + 'a>, DebError> {
        let raw = self.data_tar;
        Ok(match self.compression {
            Compression::None => Box::new(raw),
            Compression::Gzip => Box::new(GzDecoder::new(raw)),
            Compression::Xz => Box::new(XzDecoder::new(raw)),
            Compression::Zstd => Box::new(zstd::stream::Decoder::new(raw)?),
        })
    }
}

#[cfg(test)]
pub(crate) mod test_utils {
    /// Build an ar archive from `(name, contents)` members.
    pub fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = b"!<arch>\n".to_vec();
        for (name, contents) in members {
            let header = format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                format!("{name}/"),
                "0",
                "0",
                "0",
                "644",
                contents.len(),
            );
            out.extend_from_slice(header.as_bytes());
            out.extend_from_slice(contents);
            if contents.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        out
    }

    /// Wrap an uncompressed `data.tar` into a minimal `.deb`.
    pub fn deb(data_tar: &[u8]) -> Vec<u8> {
        ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar", &[]),
            ("data.tar", data_tar),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::{ar, deb};
    use super::*;

    #[test]
    fn rejects_non_ar_input() {
        assert!(DebArchive::new(b"").is_err());
        assert!(DebArchive::new(b"not an ar archive").is_err());
    }

    #[test]
    fn rejects_ar_without_debian_binary() {
        let bytes = ar(&[("notdeb", b"hello")]);
        assert!(matches!(DebArchive::new(&bytes), Err(DebError::Invalid(_))));
    }

    #[test]
    fn rejects_unknown_format_version() {
        let bytes = ar(&[("debian-binary", b"3.0\n"), ("data.tar", &[])]);
        assert!(matches!(DebArchive::new(&bytes), Err(DebError::Invalid(_))));
    }

    #[test]
    fn rejects_unsupported_compression() {
        let bytes = ar(&[("debian-binary", b"2.0\n"), ("data.tar.bz2", b"x")]);
        assert!(matches!(
            DebArchive::new(&bytes),
            Err(DebError::UnsupportedCompression(_))
        ));
    }

    #[test]
    fn reads_uncompressed_data_tar() {
        let bytes = deb(b"tar bytes");
        let mut out = Vec::new();
        DebArchive::new(&bytes)
            .unwrap()
            .data_tar()
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"tar bytes");
    }
}
