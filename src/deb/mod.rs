//! Reading Debian binary packages (`.deb`).
//!
//! A `.deb` is an ar archive holding, in order:
//! - `debian-binary`: the format version, `2.0\n`
//! - `control.tar[.gz|.xz|.zst]`: package metadata and maintainer scripts
//! - `data.tar[.gz|.xz|.zst]`: the files to install
//!
//! We never write either to disk: callers get a decompressing reader over
//! `data.tar` and feed it into a [`crate::rootfs::Rootfs`], and read the
//! metadata dpkg would record from `control.tar` ([`DebArchive::control`]).
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

    #[error("unsupported compression: {0}")]
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

/// Member-name suffix to compression, the formats dpkg accepts for
/// `control.tar` and `data.tar`.
const TAR_SUFFIXES: &[(&str, Compression)] = &[
    (".tar", Compression::None),
    (".tar.gz", Compression::Gzip),
    (".tar.xz", Compression::Xz),
    (".tar.zst", Compression::Zstd),
];

/// The metadata files in `control.tar` that dpkg keeps in its database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlFiles {
    /// The `control` stanza.
    pub control: String,
    /// `md5sums`: `<md5>  <path>` for each file, if the package ships it.
    pub md5sums: Option<Vec<u8>>,
    /// `conffiles`: one absolute path per line, if the package has any.
    pub conffiles: Option<String>,
    /// Maintainer scripts and the files they read (`postinst`,
    /// `templates`, `triggers`, ...), by name, in archive order.
    pub scripts: Vec<(String, Vec<u8>)>,
}

/// The `control.tar` files besides `control`, `md5sums` and `conffiles`
/// that dpkg keeps in `/var/lib/dpkg/info`, and whether each one is a script.
pub const MAINTAINER_FILES: &[(&str, bool)] = &[
    ("preinst", true),
    ("postinst", true),
    ("prerm", true),
    ("postrm", true),
    ("config", true),
    ("templates", false),
    ("triggers", false),
    ("shlibs", false),
    ("symbols", false),
];

/// A parsed `.deb`, borrowing the package bytes.
pub struct DebArchive<'a> {
    control_tar: (&'a [u8], Compression),
    data_tar: (&'a [u8], Compression),
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

        Ok(Self {
            control_tar: tar_member(&ar, "control")?,
            data_tar: tar_member(&ar, "data")?,
        })
    }

    /// A reader over the decompressed `data.tar`.
    pub fn data_tar(&self) -> Result<Box<dyn Read + 'a>, DebError> {
        decompress(self.data_tar)
    }

    /// The `control`, `md5sums` and `conffiles` files from `control.tar`.
    pub fn control(&self) -> Result<ControlFiles, DebError> {
        let mut files = ControlFiles::default();
        let mut control = None;
        for entry in tar::Archive::new(decompress(self.control_tar)?).entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().into_owned();
            let mut data = Vec::new();
            match path.trim_start_matches("./") {
                "control" => {
                    entry.read_to_end(&mut data)?;
                    control =
                        Some(String::from_utf8(data).map_err(|_| {
                            DebError::Invalid("control file is not UTF-8".to_string())
                        })?);
                }
                "md5sums" => {
                    entry.read_to_end(&mut data)?;
                    files.md5sums = Some(data);
                }
                "conffiles" => {
                    entry.read_to_end(&mut data)?;
                    files.conffiles = Some(String::from_utf8_lossy(&data).into_owned());
                }
                name if MAINTAINER_FILES.iter().any(|(file, _)| *file == name) => {
                    entry.read_to_end(&mut data)?;
                    files.scripts.push((name.to_string(), data));
                }
                _ => {}
            }
        }
        files.control =
            control.ok_or_else(|| DebError::Invalid("missing control file".to_string()))?;
        Ok(files)
    }
}

/// Find `<stem>.tar[.<compression>]` in a `.deb`.
fn tar_member<'a>(ar: &ArArchive<'a>, stem: &str) -> Result<(&'a [u8], Compression), DebError> {
    let (member, bytes) = ar
        .find_member(|m| m.name.starts_with(&format!("{stem}.tar")))
        .ok_or_else(|| DebError::Invalid(format!("missing {stem}.tar")))?;
    let compression = TAR_SUFFIXES
        .iter()
        .find(|(suffix, _)| member.name == format!("{stem}{suffix}"))
        .map(|&(_, c)| c)
        .ok_or_else(|| DebError::UnsupportedCompression(member.name.clone()))?;
    Ok((bytes, compression))
}

fn decompress<'a>(
    (raw, compression): (&'a [u8], Compression),
) -> Result<Box<dyn Read + 'a>, DebError> {
    Ok(match compression {
        Compression::None => Box::new(raw),
        Compression::Gzip => Box::new(GzDecoder::new(raw)),
        Compression::Xz => Box::new(XzDecoder::new(raw)),
        Compression::Zstd => Box::new(zstd::stream::Decoder::new(raw)?),
    })
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

    /// An uncompressed tar of `(path, contents)` regular files.
    pub fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// A `.deb` with the given `control.tar` and `data.tar`, uncompressed.
    pub fn deb_with_control(control_tar: &[u8], data_tar: &[u8]) -> Vec<u8> {
        ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar", control_tar),
            ("data.tar", data_tar),
        ])
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
    use super::test_utils::{ar, deb, deb_with_control, tar};
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
        let bytes = ar(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar", &[]),
            ("data.tar.bz2", b"x"),
        ]);
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

    #[test]
    fn reads_the_control_files_dpkg_keeps() {
        let control = tar(&[
            ("./control", b"Package: hello\nVersion: 1\n"),
            ("./md5sums", b"abc  usr/bin/hello\n"),
            ("./conffiles", b"/etc/hello.conf\n"),
            ("./postinst", b"#!/bin/sh\n"),
        ]);
        let bytes = deb_with_control(&control, &[]);
        let files = DebArchive::new(&bytes).unwrap().control().unwrap();
        assert_eq!(files.control, "Package: hello\nVersion: 1\n");
        assert_eq!(files.md5sums.as_deref(), Some(&b"abc  usr/bin/hello\n"[..]));
        assert_eq!(files.conffiles.as_deref(), Some("/etc/hello.conf\n"));
        assert_eq!(
            files.scripts,
            [("postinst".to_string(), b"#!/bin/sh\n".to_vec())]
        );
    }

    #[test]
    fn control_without_a_control_file_is_invalid() {
        let bytes = deb_with_control(&tar(&[("./md5sums", b"")]), &[]);
        assert!(matches!(
            DebArchive::new(&bytes).unwrap().control(),
            Err(DebError::Invalid(_))
        ));
    }
}
