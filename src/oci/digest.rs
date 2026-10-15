//! OCI content digests.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hash::Sha256;

/// Error constructing a [`Digest`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DigestError {
    #[error("digest must have 'sha256:' prefix, got: {0:?}")]
    MissingPrefix(String),
    #[error("digest hash must be exactly 64 lowercase hex characters, got {len} in: {value:?}")]
    InvalidHash { value: String, len: usize },
}

/// A content-addressable digest, written `sha256:<64 lowercase hex>`.
///
/// # Example
///
/// ```
/// use paranoid::oci::Digest;
///
/// let d = Digest::from_bytes(b"hello world");
/// assert!(d.to_string().starts_with("sha256:"));
///
/// let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
/// let d: Digest = format!("sha256:{hash}").parse().unwrap();
/// assert_eq!(d.hash(), hash);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(Sha256);

impl Digest {
    /// The digest of `data`.
    pub fn from_bytes(data: &[u8]) -> Self {
        Self(Sha256::of(data))
    }

    /// The underlying hash.
    pub fn sha256(&self) -> &Sha256 {
        &self.0
    }

    /// The 64-character hex hash, without the `sha256:` prefix.
    pub fn hash(&self) -> String {
        self.0.to_string()
    }
}

impl From<Sha256> for Digest {
    fn from(hash: Sha256) -> Self {
        Self(hash)
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix("sha256:")
            .ok_or_else(|| DigestError::MissingPrefix(s.to_string()))?;
        // The OCI spec allows only lowercase hex in digests.
        let invalid = || DigestError::InvalidHash {
            value: s.to_string(),
            len: hex.len(),
        };
        if hex.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(invalid());
        }
        hex.parse().map(Self).map_err(|_| invalid())
    }
}

impl TryFrom<&str> for Digest {
    type Error = DigestError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_from_bytes() {
        let digest = Digest::from_bytes(b"hello world");
        // Known SHA-256 of "hello world"
        assert_eq!(
            digest.to_string(),
            "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_digest_from_bytes_hash_accessor() {
        let digest = Digest::from_bytes(b"hello world");
        assert_eq!(digest.hash().len(), 64);
        assert!(
            digest
                .hash()
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
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
        assert_eq!(format!("{}", digest), digest.to_string());
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
    fn test_digest_display_roundtrip() {
        let d = Digest::from_bytes(b"x");
        assert_eq!(d.to_string().parse::<Digest>().unwrap(), d);
    }
}
