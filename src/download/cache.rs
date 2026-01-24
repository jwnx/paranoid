//! Content-addressable package cache.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::lock::LockedPackage;

use super::DownloadError;

/// Content-addressable cache for downloaded packages.
///
/// Packages are stored by their SHA256 hash:
/// `{base_dir}/sha256/{first-2-chars}/{hash}.deb`
pub struct PackageCache {
    base_dir: PathBuf,
}

impl PackageCache {
    /// Create a new package cache using the default cache directory.
    ///
    /// The default location is `~/.cache/paranoid`.
    pub fn new() -> Result<Self, DownloadError> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| DownloadError::CacheDir("could not determine cache directory".into()))?;

        let base_dir = cache_dir.join("paranoid");
        Self::with_base_dir(base_dir)
    }

    /// Create a new package cache with a custom base directory.
    pub fn with_base_dir(base_dir: PathBuf) -> Result<Self, DownloadError> {
        // Ensure the sha256 directory exists
        let sha256_dir = base_dir.join("sha256");
        fs::create_dir_all(&sha256_dir).map_err(|e| {
            DownloadError::CacheDir(format!("failed to create {}: {}", sha256_dir.display(), e))
        })?;

        Ok(Self { base_dir })
    }

    /// Get the cache path for a package.
    ///
    /// Returns the path where the package would be stored based on its SHA256 hash.
    pub fn path_for(&self, package: &LockedPackage) -> PathBuf {
        let hash = package.sha256.as_str();
        let prefix = &hash[..2]; // SHA256Hash guarantees 64 chars
        self.base_dir
            .join("sha256")
            .join(prefix)
            .join(format!("{}.deb", hash))
    }

    /// Check if a package is already cached.
    ///
    /// Returns the cache path if the package exists, None otherwise.
    pub fn get(&self, package: &LockedPackage) -> Option<PathBuf> {
        let path = self.path_for(package);
        path.exists().then_some(path)
    }

    /// Store downloaded data in the cache after verifying its checksum.
    ///
    /// Returns the path to the cached file on success.
    pub fn store(&self, package: &LockedPackage, data: &[u8]) -> Result<PathBuf, DownloadError> {
        // Verify checksum
        let actual_hash = Self::compute_sha256(data);
        if actual_hash != package.sha256.as_str() {
            return Err(DownloadError::ChecksumMismatch {
                package: package.name.clone(),
                expected: package.sha256.to_string(),
                actual: actual_hash,
            });
        }

        // Determine target path and ensure parent directory exists
        let target_path = self.path_for(package);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                DownloadError::CacheDir(format!(
                    "failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Write atomically using a temp file
        let temp_path = target_path.with_extension("deb.tmp");
        fs::write(&temp_path, data)?;
        fs::rename(&temp_path, &target_path)?;

        Ok(target_path)
    }

    /// Compute the SHA256 hash of data.
    fn compute_sha256(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        hex::encode(result)
    }

    /// Get the base directory of the cache.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::test_utils::make_test_package;
    use tempfile::TempDir;

    #[test]
    fn test_path_for_package() {
        let dir = TempDir::new().unwrap();
        let cache = PackageCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        let package = make_test_package(
            "test-pkg",
            "abc123def456abc123def456abc123def456abc123def456abc123def456abc1",
        );
        let path = cache.path_for(&package);

        assert!(path.to_string_lossy().contains("sha256"));
        assert!(path.to_string_lossy().contains("ab")); // first 2 chars of hash
        assert!(path
            .to_string_lossy()
            .ends_with("abc123def456abc123def456abc123def456abc123def456abc123def456abc1.deb"));
    }

    // Note: test_path_for_short_hash was removed because Sha256Hash now
    // guarantees all hashes are exactly 64 hex characters. Short hashes
    // are no longer representable.

    #[test]
    fn test_get_nonexistent() {
        let dir = TempDir::new().unwrap();
        let cache = PackageCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        let package = make_test_package("test-pkg", "abc123");
        assert!(cache.get(&package).is_none());
    }

    #[test]
    fn test_store_and_get() {
        let dir = TempDir::new().unwrap();
        let cache = PackageCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        // Create test data
        let data = b"test package content";
        let hash = PackageCache::compute_sha256(data);

        let package = make_test_package("test-pkg", &hash);

        // Store the package
        let path = cache.store(&package, data).unwrap();
        assert!(path.exists());

        // Verify we can get it back
        let cached_path = cache.get(&package);
        assert!(cached_path.is_some());
        assert_eq!(cached_path.unwrap(), path);

        // Verify content
        let read_data = std::fs::read(&path).unwrap();
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_store_checksum_mismatch() {
        let dir = TempDir::new().unwrap();
        let cache = PackageCache::with_base_dir(dir.path().to_path_buf()).unwrap();

        let data = b"test package content";
        let package = make_test_package("test-pkg", "wrong_hash");

        let result = cache.store(&package, data);
        assert!(result.is_err());

        match result.unwrap_err() {
            DownloadError::ChecksumMismatch {
                package: name,
                expected,
                actual: _,
            } => {
                assert_eq!(name, "test-pkg");
                assert_eq!(expected, "wrong_hash");
            }
            other => panic!("Expected ChecksumMismatch, got {:?}", other),
        }
    }

    #[test]
    fn test_compute_sha256() {
        let data = b"hello world";
        let hash = PackageCache::compute_sha256(data);
        // Known SHA256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
