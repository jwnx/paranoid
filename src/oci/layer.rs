//! Deterministic tar layer creation for OCI images.
//!
//! Creates reproducible tar archives with:
//! - Sorted entries (lexicographic by path)
//! - Fixed timestamps (Unix epoch)
//! - Filesystem-based ownership and permissions
//! - GNU tar format

use std::fs::{self, Metadata};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression as GzipLevel;
use flate2::write::GzEncoder;

use super::digest::Digest;
use super::error::OciError;
use super::spec::MediaType;

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
    /// Create a layer from a source directory.
    ///
    /// Recursively collects all files and directories from `source`,
    /// creates a deterministic tar archive, and compresses it using the
    /// specified format.
    ///
    /// The layer is fully reproducible: timestamps are fixed to Unix epoch,
    /// entries are sorted lexicographically, and ownership/permissions come
    /// from the filesystem.
    ///
    /// # Errors
    ///
    /// Returns `OciError::SourceNotFound` if the source directory doesn't exist.
    /// Returns `OciError::NotADirectory` if the source path is not a directory.
    /// Returns `OciError::ReadSourceDir` if entries cannot be read.
    /// Returns `OciError::LayerCreation` if tar archive creation fails.
    /// Returns `OciError::Compression` if compression fails.
    pub fn from_directory(source: &Path, compression: Compression) -> Result<Self, OciError> {
        if !source.exists() {
            return Err(OciError::SourceNotFound {
                path: source.to_path_buf(),
            });
        }

        if !source.is_dir() {
            return Err(OciError::NotADirectory {
                path: source.to_path_buf(),
            });
        }

        // Collect and sort entries for reproducibility
        let entries = collect_entries(source)?;

        // Build the tar archive
        let uncompressed = TarBuilder::new().build(&entries)?;

        // Calculate diff_id from uncompressed data
        let diff_id = Digest::from_bytes(&uncompressed);

        // Compress the data
        let data = compression.compress(uncompressed)?;

        // Calculate digest from final (possibly compressed) data
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

#[derive(Debug)]
struct CollectedEntry {
    /// Path relative to source root (with leading ./).
    path: String,
    /// Absolute path on disk.
    disk_path: PathBuf,
    /// Cached metadata.
    metadata: Metadata,
}

impl CollectedEntry {
    fn is_dir(&self) -> bool {
        self.metadata.is_dir()
    }

    fn is_symlink(&self) -> bool {
        self.metadata.is_symlink()
    }
}

struct TarBuilder {
    inner: tar::Builder<Vec<u8>>,
}

impl TarBuilder {
    fn new() -> Self {
        Self {
            inner: tar::Builder::new(Vec::new()),
        }
    }

    fn build(mut self, entries: &[CollectedEntry]) -> Result<Vec<u8>, OciError> {
        for entry in entries {
            self.append(entry)?;
        }

        self.inner
            .into_inner()
            .map_err(|e| OciError::LayerCreation(format!("failed to finish tar archive: {}", e)))
    }

    fn append(&mut self, entry: &CollectedEntry) -> Result<(), OciError> {
        let mut header = tar::Header::new_gnu();
        header.set_mtime(REPRODUCIBLE_MTIME);
        header.set_uid(get_uid(&entry.metadata));
        header.set_gid(get_gid(&entry.metadata));

        if entry.is_symlink() {
            self.append_symlink(&mut header, entry)
        } else if entry.is_dir() {
            self.append_directory(&mut header, entry)
        } else {
            self.append_file(&mut header, entry)
        }
    }

    fn append_symlink(
        &mut self,
        header: &mut tar::Header,
        entry: &CollectedEntry,
    ) -> Result<(), OciError> {
        let target = fs::read_link(&entry.disk_path).map_err(|e| OciError::ReadSourceDir {
            path: entry.disk_path.clone(),
            source: e,
        })?;

        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(get_mode(&entry.metadata, 0o777));
        header.set_cksum();

        self.inner
            .append_link(header, &entry.path, &target)
            .map_err(|e| OciError::LayerCreation(format!("failed to append symlink: {}", e)))
    }

    fn append_directory(
        &mut self,
        header: &mut tar::Header,
        entry: &CollectedEntry,
    ) -> Result<(), OciError> {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(get_mode(&entry.metadata, 0o755));
        header.set_cksum();

        self.inner
            .append_data(header, &entry.path, std::io::empty())
            .map_err(|e| OciError::LayerCreation(format!("failed to append directory: {}", e)))
    }

    fn append_file(
        &mut self,
        header: &mut tar::Header,
        entry: &CollectedEntry,
    ) -> Result<(), OciError> {
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(entry.metadata.len());
        header.set_mode(get_mode(&entry.metadata, 0o644));
        header.set_cksum();

        let mut file = fs::File::open(&entry.disk_path).map_err(|e| OciError::ReadSourceDir {
            path: entry.disk_path.clone(),
            source: e,
        })?;

        let mut contents = Vec::new();

        file.read_to_end(&mut contents)
            .map_err(|e| OciError::ReadSourceDir {
                path: entry.disk_path.clone(),
                source: e,
            })?;

        self.inner
            .append_data(header, &entry.path, contents.as_slice())
            .map_err(|e| OciError::LayerCreation(format!("failed to append file: {}", e)))
    }
}

fn get_mode(metadata: &Metadata, #[allow(unused)] default: u32) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        default
    }
}

fn get_uid(#[allow(unused)] metadata: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        u64::from(metadata.uid())
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn get_gid(#[allow(unused)] metadata: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        u64::from(metadata.gid())
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Collect all entries from a directory recursively.
/// Entries are sorted lexicographically by path for reproducibility.
fn collect_entries(source_dir: &Path) -> Result<Vec<CollectedEntry>, OciError> {
    let mut entries = Vec::new();
    collect_entries_recursive(source_dir, source_dir, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(entries)
}

fn collect_entries_recursive(
    base: &Path,
    current: &Path,
    entries: &mut Vec<CollectedEntry>,
) -> Result<(), OciError> {
    let read_dir = fs::read_dir(current).map_err(|e| OciError::ReadSourceDir {
        path: current.to_path_buf(),
        source: e,
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|e| OciError::ReadSourceDir {
            path: current.to_path_buf(),
            source: e,
        })?;

        let disk_path = entry.path();
        let metadata = fs::symlink_metadata(&disk_path).map_err(|e| OciError::ReadSourceDir {
            path: disk_path.clone(),
            source: e,
        })?;

        let rel_path = disk_path
            .strip_prefix(base)
            .map_err(|_| OciError::RelativePath {
                path: disk_path.clone(),
            })?;

        let tar_path = format!("./{}", rel_path.display());
        let is_dir = metadata.is_dir();

        entries.push(CollectedEntry {
            path: tar_path,
            disk_path: disk_path.clone(),
            metadata,
        });

        if is_dir {
            collect_entries_recursive(base, &disk_path, entries)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();

        // Create some files
        File::create(dir.path().join("file1.txt"))
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        File::create(dir.path().join("file2.txt"))
            .unwrap()
            .write_all(b"world")
            .unwrap();

        // Create a subdirectory with a file
        fs::create_dir(dir.path().join("subdir")).unwrap();
        File::create(dir.path().join("subdir/nested.txt"))
            .unwrap()
            .write_all(b"nested content")
            .unwrap();

        dir
    }

    #[test]
    fn test_layer_gzip() {
        let dir = create_test_dir();

        let layer = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();

        assert!(!layer.data().is_empty());
        assert!(layer.digest().as_str().starts_with("sha256:"));
        assert!(layer.diff_id().as_str().starts_with("sha256:"));
        assert!(layer.size() > 0);
        assert_ne!(layer.digest(), layer.diff_id()); // Compressed != uncompressed
        assert_eq!(layer.compression(), Compression::Gzip);
        assert_eq!(layer.media_type(), MediaType::LayerTarGzip);
    }

    #[test]
    fn test_layer_uncompressed() {
        let dir = create_test_dir();

        let layer = Layer::from_directory(dir.path(), Compression::Uncompressed).unwrap();

        assert!(!layer.data().is_empty());
        // For uncompressed, digest and diff_id should be the same
        assert_eq!(layer.digest(), layer.diff_id());
        assert_eq!(layer.compression(), Compression::Uncompressed);
        assert_eq!(layer.media_type(), MediaType::LayerTar);
    }

    #[test]
    fn test_layer_zstd() {
        let dir = create_test_dir();

        let layer = Layer::from_directory(dir.path(), Compression::Zstd).unwrap();

        assert!(!layer.data().is_empty());
        assert!(layer.digest().as_str().starts_with("sha256:"));
        assert_ne!(layer.digest(), layer.diff_id());
        assert_eq!(layer.compression(), Compression::Zstd);
        assert_eq!(layer.media_type(), MediaType::LayerTarZstd);
    }

    #[test]
    fn test_layer_reproducibility() {
        let dir = create_test_dir();

        let layer1 = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();
        let layer2 = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();

        assert_eq!(layer1.data(), layer2.data());
        assert_eq!(layer1.digest(), layer2.digest());
        assert_eq!(layer1.diff_id(), layer2.diff_id());
    }

    #[test]
    fn test_source_not_found() {
        let result = Layer::from_directory(Path::new("/nonexistent/path"), Compression::Gzip);

        assert!(matches!(result, Err(OciError::SourceNotFound { .. })));
    }

    #[test]
    fn test_not_a_directory() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("file.txt");
        File::create(&file_path).unwrap();

        let result = Layer::from_directory(&file_path, Compression::Gzip);

        assert!(matches!(result, Err(OciError::NotADirectory { .. })));
    }

    #[test]
    fn test_empty_directory() {
        let dir = TempDir::new().unwrap();

        let layer = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();

        // Empty directory should still produce a valid (small) tar
        assert!(!layer.data().is_empty());
        assert!(layer.size() > 0);
    }

    #[test]
    fn test_deep_nesting() {
        let dir = TempDir::new().unwrap();

        // Create deeply nested structure
        let deep_path = dir.path().join("a/b/c/d/e/f");
        fs::create_dir_all(&deep_path).unwrap();
        File::create(deep_path.join("deep.txt"))
            .unwrap()
            .write_all(b"deep content")
            .unwrap();

        let layer = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();

        assert!(!layer.data().is_empty());
        assert!(layer.digest().as_str().starts_with("sha256:"));
    }

    #[cfg(unix)]
    #[test]
    fn test_layer_with_symlink() {
        let dir = TempDir::new().unwrap();

        // Create a file and a symlink
        let file_path = dir.path().join("target.txt");
        File::create(&file_path)
            .unwrap()
            .write_all(b"target content")
            .unwrap();

        std::os::unix::fs::symlink("target.txt", dir.path().join("link.txt")).unwrap();

        let layer = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();

        assert!(!layer.data().is_empty());
        assert!(layer.digest().as_str().starts_with("sha256:"));
    }

    #[test]
    fn test_into_data() {
        let dir = create_test_dir();

        let layer = Layer::from_directory(dir.path(), Compression::Gzip).unwrap();
        let expected_len = layer.size() as usize;
        let data = layer.into_data();

        assert_eq!(data.len(), expected_len);
    }

    #[test]
    fn test_get_mode() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        File::create(&file_path).unwrap();

        let metadata = fs::metadata(&file_path).unwrap();
        let mode = get_mode(&metadata, 0o644);

        // On Unix, should return actual mode; on other platforms, default
        #[cfg(unix)]
        assert!(mode > 0);
        #[cfg(not(unix))]
        assert_eq!(mode, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn test_get_uid_gid() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        File::create(&file_path).unwrap();

        let metadata = fs::metadata(&file_path).unwrap();

        // Should return current user's uid/gid
        let uid = get_uid(&metadata);
        let gid = get_gid(&metadata);

        // Verify they match what std::os::unix reports
        use std::os::unix::fs::MetadataExt;
        assert_eq!(uid, u64::from(metadata.uid()));
        assert_eq!(gid, u64::from(metadata.gid()));
    }
}
