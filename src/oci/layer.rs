//! Deterministic tar layer creation for OCI images.
//!
//! A layer is serialized from a [`Rootfs`], never from the host filesystem:
//! - entries in path order, parents before children
//! - ownership and permissions exactly as the packages declared them
//! - fixed timestamps (Unix epoch)
//! - GNU tar format

use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::path::Path;
use std::sync::Arc;

use tempfile::NamedTempFile;

use super::digest::Digest;
use super::error::OciError;
use super::gzip::GzipWriter;
use super::spec::MediaType;
use crate::hash::HashingWriter;
use crate::rootfs::{Meta, Node, Rootfs};

/// Fixed mtime for reproducible builds (Unix epoch).
const REPRODUCIBLE_MTIME: u64 = 0;

/// zstd level for layers.
const ZSTD_LEVEL: i32 = 3;

/// Compression format for OCI layers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    /// No compression (raw tar).
    Uncompressed,
    /// Gzip compression (most compatible).
    #[default]
    Gzip,
    /// Zstandard compression (better ratio, faster).
    Zstd,
}

impl Compression {
    /// Returns the OCI media type for layers with this compression.
    fn media_type(&self) -> MediaType {
        match self {
            Compression::Uncompressed => MediaType::LayerTar,
            Compression::Gzip => MediaType::LayerTarGzip,
            Compression::Zstd => MediaType::LayerTarZstd,
        }
    }

    /// A writer that compresses into `inner`.
    fn encoder<W: Write>(&self, inner: W) -> io::Result<Encoder<W>> {
        Ok(match self {
            Compression::Uncompressed => Encoder::Raw(inner),
            Compression::Gzip => Encoder::Gzip(GzipWriter::new(inner)?),
            Compression::Zstd => Encoder::Zstd(zstd::Encoder::new(inner, ZSTD_LEVEL)?),
        })
    }
}

/// A streaming compressor for one of the [`Compression`] formats.
enum Encoder<W: Write> {
    Raw(W),
    Gzip(GzipWriter<W>),
    Zstd(zstd::Encoder<'static, W>),
}

impl<W: Write> Encoder<W> {
    fn finish(self) -> io::Result<W> {
        match self {
            Encoder::Raw(w) => Ok(w),
            Encoder::Gzip(e) => e.finish(),
            Encoder::Zstd(e) => e.finish(),
        }
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Encoder::Raw(w) => w.write(buf),
            Encoder::Gzip(e) => e.write(buf),
            Encoder::Zstd(e) => e.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Encoder::Raw(w) => w.flush(),
            Encoder::Gzip(e) => e.flush(),
            Encoder::Zstd(e) => e.flush(),
        }
    }
}

/// An OCI image layer, written to a temporary file.
///
/// The tar is streamed from the rootfs through the compressor into the file,
/// hashing both sides on the way, so neither the tar nor the compressed
/// layer is ever held in memory.
#[derive(Debug)]
pub struct Layer {
    /// The layer data (compressed or uncompressed).
    file: NamedTempFile,
    /// SHA256 digest of the compressed/final data (for manifest).
    digest: Digest,
    /// SHA256 digest of the uncompressed tar (for config diff_id).
    diff_id: Digest,
    /// Size of the layer data in bytes.
    size: u64,
    /// Compression format used.
    compression: Compression,
}

impl Layer {
    /// Serialize a rootfs into a compressed layer, in a temporary file in
    /// `dir`. Put it on the filesystem the image is written to, so the
    /// layer can be moved into place rather than copied.
    pub fn from_rootfs(
        rootfs: &Rootfs,
        compression: Compression,
        dir: &Path,
    ) -> Result<Self, OciError> {
        let err = |e: io::Error| OciError::LayerCreation(format!("failed to build tar: {e}"));
        let file = NamedTempFile::new_in(dir).map_err(err)?;

        let compressed = HashingWriter::new(BufWriter::new(file));
        let tar = HashingWriter::new(compression.encoder(compressed).map_err(err)?);
        let tar = write_tar(rootfs, tar).map_err(err)?;
        let (encoder, diff_id, _) = tar.finish();
        let compressed = encoder
            .finish()
            .map_err(|e| OciError::Compression(e.to_string()))?;
        let (file, digest, size) = compressed.finish();
        let file = file.into_inner().map_err(|e| err(e.into_error()))?;

        Ok(Self {
            file,
            digest: digest.into(),
            diff_id: diff_id.into(),
            size,
            compression,
        })
    }

    /// The layer data, read from the start.
    pub fn reader(&self) -> io::Result<File> {
        let mut file = self.file.reopen()?;
        file.rewind()?;
        Ok(file)
    }

    /// Move the layer data to `path`.
    pub fn persist(self, path: &Path) -> io::Result<()> {
        self.file.persist(path).map(drop).map_err(|e| e.error)
    }

    /// Returns the SHA256 digest of the (possibly compressed) layer data.
    ///
    /// This is used in the OCI manifest to reference the layer blob.
    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Returns the SHA256 digest of the uncompressed tar data.
    ///
    /// This is used as the `diff_id` in the OCI image config.
    pub fn diff_id(&self) -> &Digest {
        &self.diff_id
    }

    /// Returns the size of the layer data in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the compression format used for this layer.
    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// Returns the OCI media type for this layer.
    pub fn media_type(&self) -> MediaType {
        self.compression.media_type()
    }
}

/// Write every rootfs node as a tar entry, in path order, to `out`.
fn write_tar<W: Write>(rootfs: &Rootfs, out: W) -> io::Result<W> {
    let mut builder = tar::Builder::new(out);

    for (path, node) in rootfs.iter() {
        let mut header = header_for(node.meta());
        match node {
            Node::Dir(_) => {
                header.set_entry_type(tar::EntryType::Directory);
                builder.append_data(&mut header, format!("{path}/"), std::io::empty())?;
            }
            Node::File(_, data) => append_file(&mut builder, &mut header, path, data)?,
            Node::Symlink(_, target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                builder.append_link(&mut header, path, target)?;
            }
            Node::Hardlink { target, data, .. } => {
                // A tar link must point at an entry that came before it, with
                // the same contents. Otherwise we write a full copy.
                let target_written = target.as_str() < path
                    && matches!(rootfs.get(target), Some(Node::File(_, d)) if Arc::ptr_eq(d, data));
                if target_written {
                    header.set_entry_type(tar::EntryType::Link);
                    builder.append_link(&mut header, path, target)?;
                } else {
                    append_file(&mut builder, &mut header, path, data)?;
                }
            }
        }
    }

    builder.into_inner()
}

fn header_for(meta: &Meta) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_mtime(REPRODUCIBLE_MTIME);
    header.set_mode(meta.mode);
    header.set_uid(meta.uid);
    header.set_gid(meta.gid);
    header.set_size(0);
    header
}

fn append_file<W: Write>(
    builder: &mut tar::Builder<W>,
    header: &mut tar::Header,
    name: &str,
    data: &[u8],
) -> std::io::Result<()> {
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(data.len() as u64);
    builder.append_data(header, name, data)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn layer(rootfs: &Rootfs, compression: Compression) -> Result<Layer, OciError> {
        Layer::from_rootfs(rootfs, compression, &std::env::temp_dir())
    }

    fn data(layer: &Layer) -> Vec<u8> {
        let mut data = Vec::new();
        layer.reader().unwrap().read_to_end(&mut data).unwrap();
        data
    }

    /// A small rootfs: a directory, a setuid file, a symlink and a hardlink.
    fn sample_rootfs() -> Rootfs {
        let mut builder = tar::Builder::new(Vec::new());
        let mut add =
            |path: &str, kind: tar::EntryType, mode: u32, data: &[u8], link: Option<&str>| {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(kind);
                h.set_mode(mode);
                h.set_uid(0);
                h.set_gid(0);
                h.set_size(data.len() as u64);
                match link {
                    Some(target) => builder.append_link(&mut h, path, target).unwrap(),
                    None => builder.append_data(&mut h, path, data).unwrap(),
                }
            };
        add("./usr/", tar::EntryType::Directory, 0o755, b"", None);
        add("./usr/bin/", tar::EntryType::Directory, 0o755, b"", None);
        add("./usr/bin/su", tar::EntryType::Regular, 0o4755, b"su", None);
        add(
            "./usr/bin/sh",
            tar::EntryType::Symlink,
            0o777,
            b"",
            Some("dash"),
        );
        add(
            "./usr/bin/zz",
            tar::EntryType::Link,
            0o4755,
            b"",
            Some("./usr/bin/su"),
        );
        let tar = builder.into_inner().unwrap();

        let mut fs = Rootfs::new();
        fs.apply_tar(tar.as_slice()).unwrap();
        fs
    }

    /// (path, type, mode, uid, mtime) for each entry of an uncompressed layer.
    fn entries(layer: &Layer) -> Vec<(String, tar::EntryType, u32, u64, u64)> {
        let mut archive = tar::Archive::new(layer.reader().unwrap());
        archive
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let h = e.header();
                (
                    e.path().unwrap().to_string_lossy().into_owned(),
                    h.entry_type(),
                    h.mode().unwrap(),
                    h.uid().unwrap(),
                    h.mtime().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn layer_preserves_package_metadata() {
        let layer = layer(&sample_rootfs(), Compression::Uncompressed).unwrap();
        let entries = entries(&layer);

        let paths: Vec<_> = entries.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(
            paths,
            ["usr/", "usr/bin/", "usr/bin/sh", "usr/bin/su", "usr/bin/zz"]
        );
        let su = &entries[3];
        assert_eq!((su.2, su.3, su.4), (0o4755, 0, REPRODUCIBLE_MTIME));
        assert_eq!(entries[2].1, tar::EntryType::Symlink);
        assert_eq!(entries[4].1, tar::EntryType::Link);
    }

    #[test]
    fn hardlink_before_its_target_is_written_as_a_file() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(1);
        builder.append_data(&mut h, "./b", &b"x"[..]).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(0);
        builder.append_link(&mut h, "./a", "./b").unwrap();
        let mut fs = Rootfs::new();
        fs.apply_tar(builder.into_inner().unwrap().as_slice())
            .unwrap();

        let layer = layer(&fs, Compression::Uncompressed).unwrap();
        let mut archive = tar::Archive::new(layer.reader().unwrap());
        let mut first = archive.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(first.path().unwrap().to_str(), Some("a"));
        assert_eq!(first.header().entry_type(), tar::EntryType::Regular);
        let mut contents = String::new();
        first.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "x");
    }

    #[test]
    fn layer_digests_match_contents() {
        for compression in [
            Compression::Uncompressed,
            Compression::Gzip,
            Compression::Zstd,
        ] {
            let layer = layer(&sample_rootfs(), compression).unwrap();
            let data = data(&layer);
            assert_eq!(layer.digest(), &Digest::from_bytes(&data));
            assert_eq!(layer.size(), data.len() as u64);
            assert_eq!(layer.compression(), compression);
        }
        let raw = layer(&sample_rootfs(), Compression::Uncompressed).unwrap();
        assert_eq!(raw.digest(), raw.diff_id());
    }

    #[test]
    fn layer_is_reproducible() {
        let a = layer(&sample_rootfs(), Compression::Gzip).unwrap();
        let b = layer(&sample_rootfs(), Compression::Gzip).unwrap();
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.diff_id(), b.diff_id());
    }

    #[test]
    fn empty_rootfs_is_a_valid_layer() {
        let layer = layer(&Rootfs::new(), Compression::Gzip).unwrap();
        assert!(layer.size() > 0);
    }
}
