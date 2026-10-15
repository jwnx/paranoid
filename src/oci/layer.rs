//! Deterministic tar layer creation for OCI images.
//!
//! A layer is serialized from a [`Rootfs`], never from the host filesystem:
//! - entries in path order, parents before children
//! - ownership and permissions exactly as the packages declared them
//! - fixed timestamps (Unix epoch)
//! - GNU tar format

use std::io::Write;
use std::sync::Arc;

use flate2::Compression as GzipLevel;
use flate2::write::GzEncoder;

use super::digest::Digest;
use super::error::OciError;
use super::spec::MediaType;
use crate::rootfs::{Meta, Node, Rootfs};

/// Fixed mtime for reproducible builds (Unix epoch).
const REPRODUCIBLE_MTIME: u64 = 0;

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

    /// Compress data using this format.
    fn compress(&self, data: Vec<u8>) -> Result<Vec<u8>, OciError> {
        match self {
            Compression::Uncompressed => Ok(data),
            Compression::Gzip => {
                let mut encoder = GzEncoder::new(Vec::new(), GzipLevel::default());
                encoder.write_all(&data).map_err(|e| {
                    OciError::Compression(format!("gzip compression failed: {}", e))
                })?;
                encoder
                    .finish()
                    .map_err(|e| OciError::Compression(format!("gzip finish failed: {}", e)))
            }
            Compression::Zstd => zstd::encode_all(data.as_slice(), 3)
                .map_err(|e| OciError::Compression(format!("zstd compression failed: {}", e))),
        }
    }
}

/// An OCI image layer.
///
/// Contains the (possibly compressed) tar data along with its digests
/// and metadata needed for OCI manifests and configs.
#[derive(Debug, Clone)]
pub struct Layer {
    /// The layer data (compressed or uncompressed).
    data: Vec<u8>,
    /// SHA256 digest of the compressed/final data (for manifest).
    digest: Digest,
    /// SHA256 digest of the uncompressed tar (for config diff_id).
    diff_id: Digest,
    /// Compression format used.
    compression: Compression,
}

impl Layer {
    /// Serialize a rootfs into a compressed layer.
    pub fn from_rootfs(rootfs: &Rootfs, compression: Compression) -> Result<Self, OciError> {
        let uncompressed = build_tar(rootfs)
            .map_err(|e| OciError::LayerCreation(format!("failed to build tar: {e}")))?;
        let diff_id = Digest::from_bytes(&uncompressed);
        let data = compression.compress(uncompressed)?;
        let digest = Digest::from_bytes(&data);

        Ok(Self {
            data,
            digest,
            diff_id,
            compression,
        })
    }

    /// Returns the layer data (compressed or uncompressed).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Consumes the layer and returns the underlying data.
    pub fn into_data(self) -> Vec<u8> {
        self.data
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
        self.data.len() as u64
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

/// Write every rootfs node as a tar entry, in path order.
fn build_tar(rootfs: &Rootfs) -> std::io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());

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

fn append_file(
    builder: &mut tar::Builder<Vec<u8>>,
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
        let mut archive = tar::Archive::new(layer.data());
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
        let layer = Layer::from_rootfs(&sample_rootfs(), Compression::Uncompressed).unwrap();
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

        let layer = Layer::from_rootfs(&fs, Compression::Uncompressed).unwrap();
        let mut archive = tar::Archive::new(layer.data());
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
            let layer = Layer::from_rootfs(&sample_rootfs(), compression).unwrap();
            assert_eq!(layer.digest(), &Digest::from_bytes(layer.data()));
            assert_eq!(layer.size(), layer.data().len() as u64);
            assert_eq!(layer.compression(), compression);
        }
        let raw = Layer::from_rootfs(&sample_rootfs(), Compression::Uncompressed).unwrap();
        assert_eq!(raw.digest(), raw.diff_id());
    }

    #[test]
    fn layer_is_reproducible() {
        let a = Layer::from_rootfs(&sample_rootfs(), Compression::Gzip).unwrap();
        let b = Layer::from_rootfs(&sample_rootfs(), Compression::Gzip).unwrap();
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.diff_id(), b.diff_id());
    }

    #[test]
    fn empty_rootfs_is_a_valid_layer() {
        let layer = Layer::from_rootfs(&Rootfs::new(), Compression::Gzip).unwrap();
        assert!(layer.size() > 0);
    }
}
