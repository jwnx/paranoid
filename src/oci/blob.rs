//! Content-addressable blob storage for OCI images.
//!
//! Handles writing blobs to the blobs/sha256/ directory structure.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::digest::{Digest, sha256_hex};
use super::error::OciError;

/// Content-addressable blob storage.
///
/// Manages blobs in the OCI `blobs/sha256/` directory structure.
/// Each blob is stored with its SHA256 hash as the filename.
pub struct BlobStore {
    blob_dir: PathBuf,
}

impl BlobStore {
    pub fn new(output_dir: &Path) -> Result<Self, OciError> {
        let blob_dir = output_dir.join("blobs").join("sha256");
        fs::create_dir_all(&blob_dir).map_err(|e| OciError::CreateOutputDir {
            path: blob_dir.clone(),
            source: e,
        })?;

        Ok(Self { blob_dir })
    }

    pub fn write(&self, data: &[u8]) -> Result<(Digest, u64), OciError> {
        let hash = sha256_hex(data);
        let path = self.blob_dir.join(&hash);

        self.write_file(&path, data)?;

        Ok((Digest::from_trusted_hash(&hash), data.len() as u64))
    }

    pub fn write_with_digest(&self, data: &[u8], digest: &Digest) -> Result<u64, OciError> {
        let path = self.blob_dir.join(digest.hash());
        self.write_file(&path, data)?;
        Ok(data.len() as u64)
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), OciError> {
        let mut file = fs::File::create(path).map_err(|e| OciError::WriteFile {
            path: path.to_path_buf(),
            source: e,
        })?;

        file.write_all(data).map_err(|e| OciError::WriteFile {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Get the path to the blobs directory.
    #[cfg(test)]
    fn blobs_dir(&self) -> &Path {
        &self.blob_dir
    }

    /// Get the path where a blob with the given digest would be stored.
    #[cfg(test)]
    fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.blob_dir.join(digest.hash())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_blob_store_creation() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        assert!(store.blobs_dir().exists());
        assert!(dir.path().join("blobs/sha256").exists());
    }

    #[test]
    fn test_write_blob() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let data = b"hello world";
        let (digest, size) = store.write(data).unwrap();

        assert_eq!(size, 11);
        // SHA256 of "hello world"
        assert_eq!(
            digest.hash(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        // Verify file exists
        let blob_path = store.blob_path(&digest);
        assert!(blob_path.exists());
        assert_eq!(fs::read(&blob_path).unwrap(), data);
    }

    #[test]
    fn test_write_with_digest() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let data = b"test data";
        let digest = Digest::from_bytes(data);

        let size = store.write_with_digest(data, &digest).unwrap();
        assert_eq!(size, 9);

        let blob_path = store.blob_path(&digest);
        assert!(blob_path.exists());
        assert_eq!(fs::read(&blob_path).unwrap(), data);
    }

    #[test]
    fn test_blob_path() {
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let digest = Digest::from_bytes(b"abc123");
        let path = store.blob_path(&digest);
        assert!(path.ends_with(digest.hash()));
    }

    #[test]
    fn test_write_blob_idempotent() {
        // Writing the same data twice must succeed and produce the same digest both times.
        let dir = TempDir::new().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();

        let data = b"idempotent content";
        let (digest1, size1) = store.write(data).unwrap();
        let (digest2, size2) = store.write(data).unwrap();

        assert_eq!(digest1, digest2);
        assert_eq!(size1, size2);
        assert_eq!(fs::read(store.blob_path(&digest1)).unwrap(), data);
    }
}
