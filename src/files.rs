//! Files fetched from a URL rather than a repository (`[files.<id>]`).
//!
//! `lock` checks each file against the hash the manifest pins and records
//! its size; from then on a file is a blob like a package: `build` fetches
//! it from the stores or its URL, checks it against the lock, and places it
//! in the image after the packages.

use std::io::{self, Read};

use flate2::read::GzDecoder;
use thiserror::Error;
use xz2::read::XzDecoder;

use crate::config::{Extract, FileSpec, Manifest};
use crate::download::{DownloadFailures, DownloadService, PackageCache, Sources};
use crate::lock::{LockedFile, Trust};
use crate::report::Reporter;
use crate::rootfs::{Rootfs, RootfsError};

/// The most `lock` reads of a file before it knows the file's size.
pub const MAX_FILE_SIZE: u64 = 4 << 30;

/// Lock the manifest's files, in manifest order: check each against the
/// hash the manifest pins, and record its size. A file the cache has isn't
/// downloaded again, and one that's downloaded goes into the cache for
/// `build`.
pub async fn lock(
    manifest: &Manifest,
    downloads: &DownloadService,
    reporter: &dyn Reporter,
) -> Result<Vec<LockedFile>, DownloadFailures> {
    let mut files: Vec<LockedFile> = manifest
        .files
        .iter()
        .map(|(id, spec)| LockedFile {
            name: id.clone(),
            url: spec.url.clone(),
            sha256: spec.sha256,
            size: MAX_FILE_SIZE,
            trust: Trust::Pinned,
        })
        .collect();

    let cache = downloads.cache();
    let cached = |file: &LockedFile| {
        let data = PackageCache::read(&cache.path_for(file), file).ok()?;
        Some(data.len() as u64)
    };
    let mut missing = Vec::new();
    for (i, file) in files.iter_mut().enumerate() {
        match cached(file) {
            Some(size) => file.size = size,
            None => missing.push(i),
        }
    }

    let wanted: Vec<LockedFile> = missing.iter().map(|&i| files[i].clone()).collect();
    let wanted: Vec<&LockedFile> = wanted.iter().collect();
    let fetched = downloads
        .download_all(&wanted, Sources::of(manifest), reporter)
        .await?;
    for (&i, fetched) in missing.iter().zip(fetched) {
        // The download was checked against the hash on its way into the
        // cache, so its length is the file's size.
        files[i].size = std::fs::metadata(&fetched.path)
            .map(|m| m.len())
            .map_err(|e| DownloadFailures(vec![(files[i].name.clone(), e.into())]))?;
    }
    Ok(files)
}

/// Errors placing a file in an image.
#[derive(Debug, Error)]
pub enum PlaceError {
    #[error("failed to read the archive")]
    Io(#[from] io::Error),

    #[error("the archive has no '{0}'")]
    Missing(String),

    #[error("'{0}' in the archive is not a regular file")]
    NotAFile(String),

    #[error("'{0}' in the archive is not a directory")]
    NotADirectory(String),

    #[error("hard link '{link}' in the archive points to '{target}', outside '{dir}'")]
    LinkOutside {
        link: String,
        target: String,
        dir: String,
    },

    #[error(transparent)]
    Rootfs(#[from] RootfsError),
}

/// The most we allocate up front for an archive member, whatever its
/// header says.
const PREALLOCATE: u64 = 64 << 20;

/// Default permissions of a download placed as it is.
const DEFAULT_MODE: u32 = 0o644;

/// Place a file in the rootfs, owned by root, as `spec` says: the download
/// itself at `path`, or what `extract` names in it. It may add to
/// directories that are there, but not replace anything, so it can't
/// clobber a package's file without anyone noticing.
pub fn place(rootfs: &mut Rootfs, spec: &FileSpec, data: &[u8]) -> Result<(), PlaceError> {
    let path = spec.path.relative();
    let mut out = tar::Builder::new(Vec::new());
    match &spec.extract {
        None => append(
            &mut out,
            tar::EntryType::Regular,
            path,
            spec.mode.map_or(DEFAULT_MODE, |m| m.0),
            data,
        )?,
        Some(Extract::Member(member)) => {
            let (mode, contents) = find_member(data, member)?;
            let mode = spec.mode.map_or(mode, |m| m.0);
            append(&mut out, tar::EntryType::Regular, path, mode, &contents)?;
        }
        Some(Extract::Dir(dir)) => extract_dir(data, dir, path, &mut out)?,
    }
    rootfs.add_tar(out.into_inner()?.as_slice(), path)?;
    Ok(())
}

/// One member's permissions and contents.
fn find_member(data: &[u8], member: &str) -> Result<(u32, Vec<u8>), PlaceError> {
    let mut archive = tar::Archive::new(decompress(data)?);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if archive_path(&entry)? != member {
            continue;
        }
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Continuous
        ) {
            return Err(PlaceError::NotAFile(member.to_string()));
        }
        let mode = entry.header().mode()? & 0o7777;
        let mut contents = Vec::with_capacity(entry.size().min(PREALLOCATE) as usize);
        entry.read_to_end(&mut contents)?;
        return Ok((mode, contents));
    }
    Err(PlaceError::Missing(member.to_string()))
}

/// Copy everything under `dir` in the archive to under `path`, keeping
/// modes, symlinks and hard links but not ownership.
fn extract_dir(
    data: &[u8],
    dir: &str,
    path: &str,
    out: &mut tar::Builder<Vec<u8>>,
) -> Result<(), PlaceError> {
    let moved = |name: &str| -> Option<String> {
        if name == dir {
            return Some(path.to_string());
        }
        let rest = name.strip_prefix(dir)?.strip_prefix('/')?;
        Some(format!("{path}/{rest}"))
    };

    let mut found = false;
    let mut archive = tar::Archive::new(decompress(data)?);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = archive_path(&entry)?;
        let Some(target) = moved(&name) else {
            continue;
        };
        found = true;
        let mode = entry.header().mode()? & 0o7777;
        let kind = entry.header().entry_type();
        if name == dir && !kind.is_dir() {
            return Err(PlaceError::NotADirectory(name));
        }
        match kind {
            tar::EntryType::Directory => append(out, kind, &target, mode, &[])?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mut contents = Vec::with_capacity(entry.size().min(PREALLOCATE) as usize);
                entry.read_to_end(&mut contents)?;
                append(out, tar::EntryType::Regular, &target, mode, &contents)?;
            }
            tar::EntryType::Symlink => {
                let link = link_name(&entry)?;
                append_link(out, kind, &target, mode, &link)?;
            }
            tar::EntryType::Link => {
                let link = link_name(&entry)?;
                let linked = normalize(&link);
                let Some(moved) = moved(&linked) else {
                    return Err(PlaceError::LinkOutside {
                        link: name,
                        target: link,
                        dir: dir.to_string(),
                    });
                };
                append_link(out, kind, &target, mode, &moved)?;
            }
            // Devices and FIFOs have no place in a release tarball.
            _ => {}
        }
    }
    if !found {
        return Err(PlaceError::Missing(format!("{dir}/")));
    }
    Ok(())
}

/// An entry's path as `extract` spells it: no leading `./` or `/`, and no
/// trailing `/`.
fn archive_path<R: Read>(entry: &tar::Entry<'_, R>) -> io::Result<String> {
    Ok(normalize(&entry.path()?.to_string_lossy()))
}

fn normalize(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn link_name<R: Read>(entry: &tar::Entry<'_, R>) -> io::Result<String> {
    let link = entry
        .link_name()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "link without a target"))?;
    Ok(link.to_string_lossy().into_owned())
}

/// A header owned by root, with the times every image gets.
fn header(kind: tar::EntryType, mode: u32, size: u64) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(size);
    header
}

fn append(
    out: &mut tar::Builder<Vec<u8>>,
    kind: tar::EntryType,
    path: &str,
    mode: u32,
    data: &[u8],
) -> io::Result<()> {
    let mut header = header(kind, mode, data.len() as u64);
    out.append_data(&mut header, path, data)
}

fn append_link(
    out: &mut tar::Builder<Vec<u8>>,
    kind: tar::EntryType,
    path: &str,
    mode: u32,
    target: &str,
) -> io::Result<()> {
    let mut header = header(kind, mode, 0);
    out.append_link(&mut header, path, target)
}

/// A tar archive, decompressed by what its first bytes say it is.
fn decompress(data: &[u8]) -> io::Result<Box<dyn Read + '_>> {
    Ok(match data {
        [0x1f, 0x8b, ..] => Box::new(GzDecoder::new(data)),
        [0xfd, b'7', b'z', b'X', b'Z', 0x00, ..] => Box::new(XzDecoder::new(data)),
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Box::new(zstd::stream::Decoder::new(data)?),
        _ => Box::new(data),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::hash::Sha256;
    use crate::rootfs::Node;

    /// Entries of a test archive: (path, kind, mode, contents or link).
    type E<'a> = (&'a str, tar::EntryType, u32, &'a str);

    fn tarball(entries: &[E<'_>]) -> Vec<u8> {
        let mut out = tar::Builder::new(Vec::new());
        for &(path, kind, mode, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(mode);
            header.set_uid(1000);
            header.set_gid(1000);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_size(0);
                out.append_link(&mut header, path, contents).unwrap();
            } else {
                header.set_size(contents.len() as u64);
                out.append_data(&mut header, path, contents.as_bytes())
                    .unwrap();
            }
        }
        let tar = out.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn spec(path: &str, mode: Option<&str>, extract: Option<&str>) -> FileSpec {
        FileSpec {
            url: "https://example.com/x".parse().unwrap(),
            sha256: Sha256::of(b""),
            path: path.parse().unwrap(),
            mode: mode.map(|m| m.parse().unwrap()),
            extract: extract.map(|e| e.parse().unwrap()),
        }
    }

    fn file(rootfs: &Rootfs, path: &str) -> (u32, u64, String) {
        match rootfs.get(path) {
            Some(Node::File(meta, data)) => (
                meta.mode,
                meta.uid,
                String::from_utf8(data.to_vec()).unwrap(),
            ),
            other => panic!("{path}: {other:?}"),
        }
    }

    const DIR: tar::EntryType = tar::EntryType::Directory;
    const FILE: tar::EntryType = tar::EntryType::Regular;
    const SYMLINK: tar::EntryType = tar::EntryType::Symlink;
    const LINK: tar::EntryType = tar::EntryType::Link;

    #[test]
    fn places_a_download_as_it_is() {
        let mut rootfs = Rootfs::new();
        place(
            &mut rootfs,
            &spec("/usr/bin/tini", Some("0755"), None),
            b"tini",
        )
        .unwrap();
        assert_eq!(file(&rootfs, "usr/bin/tini"), (0o755, 0, "tini".into()));

        place(&mut rootfs, &spec("/etc/motd", None, None), b"hi").unwrap();
        assert_eq!(file(&rootfs, "etc/motd").0, 0o644);
    }

    #[test]
    fn extracts_one_member() {
        let archive = tarball(&[
            ("./etcd-v3/", DIR, 0o755, ""),
            ("./etcd-v3/etcd", FILE, 0o755, "etcd"),
            ("./etcd-v3/etcdctl", FILE, 0o750, "etcdctl"),
        ]);
        let mut rootfs = Rootfs::new();
        let etcdctl = spec("/usr/local/bin/etcdctl", None, Some("etcd-v3/etcdctl"));
        place(&mut rootfs, &etcdctl, &archive).unwrap();
        assert_eq!(
            file(&rootfs, "usr/local/bin/etcdctl"),
            (0o750, 0, "etcdctl".into())
        );
        assert!(rootfs.get("usr/local/bin/etcd").is_none());

        let mut rootfs = Rootfs::new();
        let moded = spec("/etcdctl", Some("0700"), Some("etcd-v3/etcdctl"));
        place(&mut rootfs, &moded, &archive).unwrap();
        assert_eq!(file(&rootfs, "etcdctl").0, 0o700);

        let missing = spec("/x", None, Some("etcd-v3/etcdutl"));
        let err = place(&mut Rootfs::new(), &missing, &archive).unwrap_err();
        assert_eq!(err.to_string(), "the archive has no 'etcd-v3/etcdutl'");

        let dir = spec("/x", None, Some("etcd-v3"));
        let err = place(&mut Rootfs::new(), &dir, &archive).unwrap_err();
        assert!(matches!(err, PlaceError::NotAFile(_)), "{err}");
    }

    #[test]
    fn extracts_a_directory_with_its_links() {
        let archive = tarball(&[
            ("go/", DIR, 0o755, ""),
            ("go/bin/", DIR, 0o755, ""),
            ("go/bin/go", FILE, 0o755, "go"),
            ("go/bin/gofmt", LINK, 0o755, "go/bin/go"),
            ("go/VERSION", FILE, 0o644, "go1.23"),
            ("go/current", SYMLINK, 0o777, "bin"),
            ("other/file", FILE, 0o644, "not this"),
        ]);
        let mut rootfs = Rootfs::new();
        place(
            &mut rootfs,
            &spec("/usr/local/go", None, Some("go/")),
            &archive,
        )
        .unwrap();

        assert_eq!(
            file(&rootfs, "usr/local/go/bin/go"),
            (0o755, 0, "go".into())
        );
        assert_eq!(file(&rootfs, "usr/local/go/VERSION").1, 0);
        assert!(matches!(
            rootfs.get("usr/local/go/bin/gofmt"),
            Some(Node::Hardlink { target, .. }) if target == "usr/local/go/bin/go"
        ));
        assert!(matches!(
            rootfs.get("usr/local/go/current"),
            Some(Node::Symlink(_, target)) if target == "bin"
        ));
        assert!(rootfs.iter().all(|(path, _)| !path.contains("other")));
    }

    #[test]
    fn a_hard_link_out_of_the_directory_is_refused() {
        let archive = tarball(&[
            ("etc/passwd", FILE, 0o644, "root"),
            ("go/passwd", LINK, 0o644, "etc/passwd"),
        ]);
        let err = place(
            &mut Rootfs::new(),
            &spec("/go", None, Some("go/")),
            &archive,
        )
        .unwrap_err();
        assert!(matches!(err, PlaceError::LinkOutside { .. }), "{err}");
    }

    #[test]
    fn never_replaces_what_is_there() {
        let mut rootfs = Rootfs::new();
        place(&mut rootfs, &spec("/usr/bin/tool", None, None), b"first").unwrap();
        let err = place(&mut rootfs, &spec("/usr/bin/tool", None, None), b"second").unwrap_err();
        assert_eq!(err.to_string(), "'/usr/bin/tool' is already in the image");
        assert_eq!(file(&rootfs, "usr/bin/tool").2, "first");

        // Directories are shared.
        place(&mut rootfs, &spec("/usr/bin/other", None, None), b"other").unwrap();
    }

    #[test]
    fn reads_plain_and_compressed_archives() {
        let tar = {
            let mut out = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(4);
            out.append_data(&mut header, "dir/tool", &b"tool"[..])
                .unwrap();
            out.into_inner().unwrap()
        };
        let xz = {
            let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
            enc.write_all(&tar).unwrap();
            enc.finish().unwrap()
        };
        let zst = zstd::encode_all(tar.as_slice(), 1).unwrap();
        for archive in [&tar, &xz, &zst] {
            let mut rootfs = Rootfs::new();
            place(&mut rootfs, &spec("/tool", None, Some("dir/tool")), archive).unwrap();
            assert_eq!(file(&rootfs, "tool").2, "tool");
        }
    }

    /// A rootfs with what packages would have put there: a directory with
    /// its own mode and group, a merged `/bin`, and a secret.
    fn packaged() -> Rootfs {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, kind, mode, gid, target) in [
            ("usr/", DIR, 0o755, 0, ""),
            ("usr/bin/", DIR, 0o755, 0, ""),
            ("usr/local/", DIR, 0o755, 0, ""),
            ("usr/local/bin/", DIR, 0o2775, 50, ""),
            ("bin", SYMLINK, 0o777, 0, "usr/bin"),
            ("etc/", DIR, 0o755, 0, ""),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(mode);
            header.set_uid(0);
            header.set_gid(gid);
            header.set_size(0);
            if kind.is_symlink() {
                tar.append_link(&mut header, path, target).unwrap();
            } else {
                tar.append_data(&mut header, path, &[][..]).unwrap();
            }
        }
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o600);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(6);
        tar.append_data(&mut header, "etc/shadow", &b"secret"[..])
            .unwrap();
        let mut rootfs = Rootfs::new();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        rootfs
    }

    #[test]
    fn leaves_a_directory_that_is_there_as_it_is() {
        let archive = tarball(&[
            ("etcd/", DIR, 0o700, ""),
            ("etcd/etcd", FILE, 0o755, "etcd"),
        ]);
        let mut rootfs = packaged();
        place(
            &mut rootfs,
            &spec("/usr/local/bin", None, Some("etcd/")),
            &archive,
        )
        .unwrap();
        assert!(matches!(
            rootfs.get("usr/local/bin"),
            Some(Node::Dir(meta)) if meta.mode == 0o2775 && meta.gid == 50
        ));
        assert_eq!(file(&rootfs, "usr/local/bin/etcd").2, "etcd");
    }

    #[test]
    fn follows_the_images_own_links_to_the_path() {
        let mut rootfs = packaged();
        place(&mut rootfs, &spec("/bin/tini", Some("0755"), None), b"tini").unwrap();
        assert_eq!(file(&rootfs, "usr/bin/tini").2, "tini");
    }

    #[test]
    fn a_symlink_in_the_archive_cannot_lead_out_of_the_path() {
        let archive = tarball(&[
            ("go/", DIR, 0o755, ""),
            ("go/s", SYMLINK, 0o777, "/"),
            ("go/s/usr/bin/planted", FILE, 0o4755, "planted"),
        ]);
        let mut rootfs = packaged();
        let err = place(&mut rootfs, &spec("/opt/go", None, Some("go/")), &archive).unwrap_err();
        assert!(err.to_string().contains("outside '/opt/go'"), "{err}");

        let archive = tarball(&[
            ("go/s", SYMLINK, 0o777, "../.."),
            ("go/shadow", LINK, 0o644, "go/s/etc/shadow"),
        ]);
        let err = place(
            &mut packaged(),
            &spec("/opt/go", None, Some("go/")),
            &archive,
        )
        .unwrap_err();
        assert!(err.to_string().contains("outside '/opt/go'"), "{err}");
    }

    #[test]
    fn extracting_a_directory_needs_one() {
        let archive = tarball(&[("go", FILE, 0o755, "not a dir")]);
        let err = place(
            &mut Rootfs::new(),
            &spec("/go", None, Some("go/")),
            &archive,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "'go' in the archive is not a directory");
    }
}
