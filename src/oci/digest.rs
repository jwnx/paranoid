//! Content-addressable digest types and utilities.

use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use thiserror::Error;

/// Error constructing a [`Digest`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DigestError {
    #[error("digest must have 'sha256:' prefix, got: {0:?}")]
    MissingPrefix(String),
    #[error("digest hash must be exactly 64 lowercase hex characters, got {len} in: {value:?}")]
    InvalidHash { value: String, len: usize },
}

/// A content-addressable digest in the format `"sha256:<64-hex>"`.
///
/// # Invariants
///
/// - Always starts with `"sha256:"`.
/// - The hash portion is exactly 64 lowercase hexadecimal characters.
///
/// # Construction
///
/// - [`Digest::from_bytes`] — compute a digest by hashing data (always valid).
/// - [`Digest::try_from`] / [`str::parse`] — parse and validate a full `"sha256:..."` string.
///
/// # Example
///
/// ```
/// use paranoid::oci::Digest;
/// use std::convert::TryFrom;
///
/// // Compute from data — always valid.
/// let d = Digest::from_bytes(b"hello world");
/// assert!(d.as_str().starts_with("sha256:"));
///
/// // Parse from a full digest string.
/// let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
/// let d = Digest::try_from(format!("sha256:{}", hash)).unwrap();
/// assert_eq!(d.hash(), hash);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest(String);

impl Digest {
    /// Create a digest by hashing the given data.
    ///
    /// This is the primary way to create a `Digest` from raw bytes.
    /// The resulting digest is always valid.
    pub fn from_bytes(data: &[u8]) -> Self {
        // Safety: hex::encode(Sha256::digest(...)) always produces 64 lowercase hex chars.
        Self(format!("sha256:{}", hex::encode(Sha256::digest(data))))
    }

    /// Create a digest from a pre-computed, trusted hex hash (without prefix).
    ///
    /// # Safety / Correctness
    ///
    /// This is `pub(crate)` and intended only for code that has already
    /// verified the hash is correct (e.g. values read from our own cache
    /// or produced by `sha256_hex`). External callers must use `TryFrom`.
    pub(crate) fn from_trusted_hash(hash: &str) -> Self {
        Self(format!("sha256:{}", hash))
    }

    /// Get the full digest string including the `"sha256:"` prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Get the raw 64-character hex hash without the `"sha256:"` prefix.
    pub fn hash(&self) -> &str {
        // SAFETY: the invariant guarantees "sha256:" (7 bytes) is always present.
        &self.0["sha256:".len()..]
    }
}

/// Validate a full `"sha256:<64-hex>"` string.
fn validate_digest(s: &str) -> Result<(), DigestError> {
    let hash = s
        .strip_prefix("sha256:")
        .ok_or_else(|| DigestError::MissingPrefix(s.to_string()))?;

    let len = hash.len();
    if len != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(DigestError::InvalidHash {
            value: s.to_string(),
            len,
        });
    }
    Ok(())
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        validate_digest(&s)?;
        Ok(Self(s))
    }
}

impl TryFrom<&str> for Digest {
    type Error = DigestError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        validate_digest(s)?;
        Ok(Self(s.to_string()))
    }
}

impl std::str::FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

impl From<Digest> for String {
    fn from(d: Digest) -> String {
        d.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for Digest {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

/// Compute the SHA256 hash of data and return it as a 64-char hex string (without prefix).
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_from_bytes() {
        let digest = Digest::from_bytes(b"hello world");
        // Known SHA-256 of "hello world"
        assert_eq!(
            digest.as_str(),
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_digest_from_bytes_hash_accessor() {
        let digest = Digest::from_bytes(b"hello world");
        assert_eq!(digest.hash().len(), 64);
        assert!(digest
            .hash()
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn test_digest_try_from_valid() {
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let digest = Digest::try_from(format!("sha256:{}", hash)).unwrap();
        assert_eq!(digest.hash(), hash);
    }

    #[test]
    fn test_digest_try_from_missing_prefix() {
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let result = Digest::try_from(hash);
        assert!(matches!(result, Err(DigestError::MissingPrefix(_))));
    }

    #[test]
    fn test_digest_try_from_empty() {
        assert!(matches!(
            Digest::try_from(""),
            Err(DigestError::MissingPrefix(_))
        ));
    }

    #[test]
    fn test_digest_try_from_wrong_length() {
        // 63 chars instead of 64
        let result = Digest::try_from("sha256:abc");
        assert!(matches!(result, Err(DigestError::InvalidHash { .. })));
    }

    #[test]
    fn test_digest_try_from_uppercase_rejected() {
        // uppercase hex is not allowed
        let hash = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        let result = Digest::try_from(format!("sha256:{}", hash));
        assert!(matches!(result, Err(DigestError::InvalidHash { .. })));
    }

    #[test]
    fn test_digest_display() {
        let digest = Digest::from_bytes(b"test");
        assert_eq!(format!("{}", digest), digest.as_str());
    }

    #[test]
    fn test_digest_serde_roundtrip() {
        let digest = Digest::from_bytes(b"roundtrip");
        let json = serde_json::to_string(&digest).unwrap();
        let digest2: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(digest, digest2);
    }

    #[test]
    fn test_digest_serde_rejects_invalid() {
        let result: Result<Digest, _> = serde_json::from_str(r#""not-a-digest""#);
        assert!(result.is_err());
    }

    #[test]
    fn test_digest_into_string() {
        let d = Digest::from_bytes(b"x");
        let s: String = d.clone().into();
        assert_eq!(s, d.as_str());
    }
}
