//! SHA-256 hashes.
//!
//! One type for every hash we handle: package checksums from Debian
//! indexes, the lock file's fingerprints, and OCI content digests (which add
//! a `sha256:` prefix on top, see [`crate::oci::Digest`]).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::Digest as _;
use thiserror::Error;

/// A SHA-256 hash. Displays and parses as 64 hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256([u8; 32]);

/// Error parsing a [`Sha256`].
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid SHA-256 '{0}': expected 64 hex characters")]
pub struct ParseSha256Error(String);

impl Sha256 {
    /// Hash `data`.
    pub fn of(data: &[u8]) -> Self {
        Self(sha2::Sha256::digest(data).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for Sha256 {
    type Err = ParseSha256Error;

    /// Parse 64 hex characters. Upper case is accepted and normalized.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        hex::decode_to_slice(s, &mut bytes).map_err(|_| ParseSha256Error(s.to_string()))?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({self})")
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO_WORLD: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    #[test]
    fn hashes_and_displays_lowercase_hex() {
        assert_eq!(Sha256::of(b"hello world").to_string(), HELLO_WORLD);
    }

    #[test]
    fn parses_and_normalizes_case() {
        let upper: Sha256 = HELLO_WORLD.to_uppercase().parse().unwrap();
        assert_eq!(upper, Sha256::of(b"hello world"));
        assert_eq!(upper.to_string(), HELLO_WORLD);
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            "abc123",
            &HELLO_WORLD[1..],
            &format!("{HELLO_WORLD}00"),
            &HELLO_WORLD.replace('b', "z"),
        ] {
            assert!(bad.parse::<Sha256>().is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn serde_roundtrip() {
        let hash = Sha256::of(b"x");
        let json = serde_json::to_string(&hash).unwrap();
        assert_eq!(json, format!("\"{hash}\""));
        assert_eq!(serde_json::from_str::<Sha256>(&json).unwrap(), hash);
        assert!(serde_json::from_str::<Sha256>("\"nope\"").is_err());
    }
}
