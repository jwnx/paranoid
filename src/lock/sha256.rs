//! SHA256 hash type with validation.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// A validated SHA256 hash (64 lowercase hex characters).
///
/// This type ensures that the hash is correctly formatted:
/// - Exactly 64 characters
/// - All characters are valid hexadecimal digits
/// - Stored in lowercase form
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Hash(String);

/// Error when parsing an invalid SHA256 hash.
#[derive(Debug, Error)]
#[error("invalid SHA256 hash: {reason}")]
pub struct InvalidSha256Error {
    reason: String,
}

impl Sha256Hash {
    /// Create a new SHA256 hash from a string, validating the format.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not exactly 64 hex characters.
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidSha256Error> {
        let s = s.into();

        if s.len() != 64 {
            return Err(InvalidSha256Error {
                reason: format!("expected 64 characters, got {}", s.len()),
            });
        }

        if !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidSha256Error {
                reason: "contains non-hexadecimal characters".to_string(),
            });
        }

        Ok(Self(s.to_ascii_lowercase()))
    }

    /// Create a SHA256 hash from a string without validation.
    ///
    /// This is intended for use when the hash comes from a trusted source
    /// (e.g., computing from data). Prefer [`Sha256Hash::new`] for untrusted input.
    ///
    /// # Safety
    ///
    /// The caller must ensure the string is exactly 64 lowercase hex characters.
    #[must_use]
    pub fn from_trusted(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the hash as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Sha256Hash {
    type Err = InvalidSha256Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl fmt::Display for Sha256Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for Sha256Hash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for Sha256Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Sha256Hash::new(s).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_HASH: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    #[test]
    fn test_valid_hash() {
        let hash = Sha256Hash::new(VALID_HASH).unwrap();
        assert_eq!(hash.as_str(), VALID_HASH);
    }

    #[test]
    fn test_uppercase_normalized() {
        let hash = Sha256Hash::new(VALID_HASH.to_uppercase()).unwrap();
        assert_eq!(hash.as_str(), VALID_HASH);
    }

    #[test]
    fn test_too_short() {
        let result = Sha256Hash::new("abc123");
        assert!(result.is_err());
    }

    #[test]
    fn test_too_long() {
        let result = Sha256Hash::new(format!("{}extra", VALID_HASH));
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_chars() {
        let invalid = "z94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let result = Sha256Hash::new(invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_str() {
        let hash: Sha256Hash = VALID_HASH.parse().unwrap();
        assert_eq!(hash.as_str(), VALID_HASH);
    }

    #[test]
    fn test_display() {
        let hash = Sha256Hash::new(VALID_HASH).unwrap();
        assert_eq!(format!("{}", hash), VALID_HASH);
    }

    #[test]
    fn test_serde_roundtrip() {
        let hash = Sha256Hash::new(VALID_HASH).unwrap();
        let json = serde_json::to_string(&hash).unwrap();
        let parsed: Sha256Hash = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, parsed);
    }

    #[test]
    fn test_serde_invalid_rejects() {
        let json = "\"invalid\"";
        let result: Result<Sha256Hash, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
