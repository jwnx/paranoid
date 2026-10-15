//! Domain types for OCI image configuration.
//!
//! These types enforce OCI spec invariants at construction time rather than at
//! serialization time. Every type in this module upholds its invariants as long
//! as it is constructed through the public API.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Implement the boilerplate shared by our validated string newtypes,
/// given a `fn validate(&str) -> Result<(), $err>` on the type: `as_str`,
/// `TryFrom<&str>`, `TryFrom<String>`, `FromStr`, `Display`, `AsRef<str>`,
/// `From<$ty> for String` and serde via strings.
macro_rules! string_newtype {
    ($ty:ident, $err:ty) => {
        impl $ty {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $ty {
            type Error = $err;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::validate(&s)?;
                Ok(Self(s))
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = $err;

            fn try_from(s: &str) -> Result<Self, Self::Error> {
                Self::validate(s)?;
                Ok(Self(s.to_string()))
            }
        }

        impl FromStr for $ty {
            type Err = $err;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.try_into()
            }
        }

        impl From<$ty> for String {
            fn from(value: $ty) -> String {
                value.0
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .try_into()
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

// ── EnvVar ────────────────────────────────────────────────────────────────────

/// An environment variable in `KEY=VALUE` format.
///
/// # Invariants
///
/// - Contains exactly one or more `=` characters (key ends at the first `=`).
/// - The key part (before the first `=`) is non-empty.
/// - Neither the key nor the value contains a NUL byte (`\0`).
/// - The value may be empty (`"KEY="` is valid).
///
/// # Example
///
/// ```
/// use paranoid::oci::EnvVar;
/// use std::convert::TryFrom;
///
/// let var = EnvVar::try_from("PATH=/usr/bin:/bin").unwrap();
/// assert_eq!(var.key(), "PATH");
/// assert_eq!(var.value(), "/usr/bin:/bin");
///
/// let var = EnvVar::new("HOME", "/root").unwrap();
/// assert_eq!(var.to_string(), "HOME=/root");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvVar(String);

/// Error constructing an [`EnvVar`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvVarError {
    #[error("environment variable key is empty")]
    EmptyKey,
    #[error("environment variable has no '=' separator: {0:?}")]
    MissingSeparator(String),
    #[error("environment variable contains a NUL byte")]
    NulByte,
}

impl EnvVar {
    /// Create an env var from separate key and value strings.
    ///
    /// # Errors
    ///
    /// Returns [`EnvVarError::EmptyKey`] if `key` is empty.
    /// Returns [`EnvVarError::NulByte`] if `key` or `value` contains `\0`.
    pub fn new(key: &str, value: &str) -> Result<Self, EnvVarError> {
        if key.is_empty() {
            return Err(EnvVarError::EmptyKey);
        }
        if key.contains('\0') || value.contains('\0') {
            return Err(EnvVarError::NulByte);
        }
        Ok(Self(format!("{}={}", key, value)))
    }

    /// The key part (everything before the first `=`).
    pub fn key(&self) -> &str {
        self.0.split_once('=').map(|(k, _)| k).unwrap_or(&self.0)
    }

    /// The value part (everything after the first `=`).
    pub fn value(&self) -> &str {
        self.0.split_once('=').map(|(_, v)| v).unwrap_or("")
    }

    fn validate(s: &str) -> Result<(), EnvVarError> {
        if s.contains('\0') {
            return Err(EnvVarError::NulByte);
        }
        match s.find('=') {
            None => Err(EnvVarError::MissingSeparator(s.to_string())),
            Some(0) => Err(EnvVarError::EmptyKey),
            Some(_) => Ok(()),
        }
    }
}

string_newtype!(EnvVar, EnvVarError);

// ── AbsolutePath ──────────────────────────────────────────────────────────────

/// An absolute filesystem path for use in OCI container configuration.
///
/// # Invariants
///
/// The path is absolute (i.e. [`Path::is_absolute`] returns `true`).
/// On all Unix-like systems and in OCI images this means the path starts with `/`.
///
/// # Example
///
/// ```
/// use paranoid::oci::AbsolutePath;
/// use std::convert::TryFrom;
///
/// let p = AbsolutePath::try_from("/home/alice").unwrap();
/// assert_eq!(p.as_str(), "/home/alice");
///
/// assert!(AbsolutePath::try_from("relative/path").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsolutePath(String);

/// Error constructing an [`AbsolutePath`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AbsolutePathError {
    #[error("path is not absolute: {0:?}")]
    NotAbsolute(String),
}

impl AbsolutePath {
    fn validate(s: &str) -> Result<(), AbsolutePathError> {
        if Path::new(s).is_absolute() {
            Ok(())
        } else {
            Err(AbsolutePathError::NotAbsolute(s.to_string()))
        }
    }
}

string_newtype!(AbsolutePath, AbsolutePathError);

// ── Signal ────────────────────────────────────────────────────────────────────

/// A POSIX signal that can be sent to a container to request shutdown.
///
/// Covers all signals defined by POSIX and commonly used in Linux containers.
/// The OCI spec allows signal names in the format `SIGNAME` (e.g. `SIGTERM`)
/// or numeric strings (e.g. `"15"`).
///
/// # Example
///
/// ```
/// use paranoid::oci::Signal;
/// use std::str::FromStr;
///
/// let s: Signal = "SIGTERM".parse().unwrap();
/// assert_eq!(s.as_str(), "SIGTERM");
///
/// let s: Signal = "15".parse().unwrap();
/// assert_eq!(s, Signal::Sigterm);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Sighup,
    Sigint,
    Sigquit,
    Sigill,
    Sigtrap,
    Sigabrt,
    Sigbus,
    Sigfpe,
    Sigkill,
    Sigusr1,
    Sigsegv,
    Sigusr2,
    Sigpipe,
    Sigalrm,
    Sigterm,
    Sigchld,
    Sigcont,
    Sigstop,
    Sigtstp,
    Sigttin,
    Sigttou,
    Sigurg,
    Sigxcpu,
    Sigxfsz,
    Sigvtalrm,
    Sigprof,
    Sigwinch,
    Sigio,
    Sigpwr,
    Sigsys,
}

/// Error parsing a [`Signal`].
#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown signal: {0:?}")]
pub struct ParseSignalError(String);

/// Every signal with its name and Linux number, in one place.
const SIGNALS: &[(Signal, &str, u8)] = &[
    (Signal::Sighup, "SIGHUP", 1),
    (Signal::Sigint, "SIGINT", 2),
    (Signal::Sigquit, "SIGQUIT", 3),
    (Signal::Sigill, "SIGILL", 4),
    (Signal::Sigtrap, "SIGTRAP", 5),
    (Signal::Sigabrt, "SIGABRT", 6),
    (Signal::Sigbus, "SIGBUS", 7),
    (Signal::Sigfpe, "SIGFPE", 8),
    (Signal::Sigkill, "SIGKILL", 9),
    (Signal::Sigusr1, "SIGUSR1", 10),
    (Signal::Sigsegv, "SIGSEGV", 11),
    (Signal::Sigusr2, "SIGUSR2", 12),
    (Signal::Sigpipe, "SIGPIPE", 13),
    (Signal::Sigalrm, "SIGALRM", 14),
    (Signal::Sigterm, "SIGTERM", 15),
    (Signal::Sigchld, "SIGCHLD", 17),
    (Signal::Sigcont, "SIGCONT", 18),
    (Signal::Sigstop, "SIGSTOP", 19),
    (Signal::Sigtstp, "SIGTSTP", 20),
    (Signal::Sigttin, "SIGTTIN", 21),
    (Signal::Sigttou, "SIGTTOU", 22),
    (Signal::Sigurg, "SIGURG", 23),
    (Signal::Sigxcpu, "SIGXCPU", 24),
    (Signal::Sigxfsz, "SIGXFSZ", 25),
    (Signal::Sigvtalrm, "SIGVTALRM", 26),
    (Signal::Sigprof, "SIGPROF", 27),
    (Signal::Sigwinch, "SIGWINCH", 28),
    (Signal::Sigio, "SIGIO", 29),
    (Signal::Sigpwr, "SIGPWR", 30),
    (Signal::Sigsys, "SIGSYS", 31),
];

impl Signal {
    fn entry(self) -> &'static (Signal, &'static str, u8) {
        SIGNALS
            .iter()
            .find(|(signal, ..)| *signal == self)
            .expect("every Signal has a row in SIGNALS")
    }

    /// The canonical signal name (e.g. `"SIGTERM"`).
    pub fn as_str(&self) -> &'static str {
        self.entry().1
    }

    /// The signal number on Linux.
    pub fn number(&self) -> u8 {
        self.entry().2
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Signal {
    type Err = ParseSignalError;

    /// Accepts a number (`"15"`), or a name with or without the `SIG`
    /// prefix, in any case (`"SIGTERM"`, `"term"`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let upper = s.to_ascii_uppercase();
        let name = upper.strip_prefix("SIG").unwrap_or(&upper);
        let number = s.parse::<u8>().ok();
        SIGNALS
            .iter()
            .find(|(_, n, num)| Some(*num) == number || n[3..] == *name)
            .map(|(signal, ..)| *signal)
            .ok_or_else(|| ParseSignalError(s.to_string()))
    }
}

impl Serialize for Signal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Signal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ── Timestamp ─────────────────────────────────────────────────────────────────

/// An RFC 3339 timestamp for use in OCI image metadata.
///
/// Wraps [`chrono::DateTime<Utc>`] and serializes/deserializes as an RFC 3339
/// string (e.g. `"2015-10-31T22:22:56.015925234Z"`).
///
/// # Example
///
/// ```
/// use paranoid::oci::Timestamp;
/// use std::str::FromStr;
///
/// let ts: Timestamp = "2015-10-31T22:22:56Z".parse().unwrap();
/// assert!(ts.to_string().starts_with("2015-10-31"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timestamp(DateTime<Utc>);

/// Error parsing a [`Timestamp`].
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid RFC 3339 timestamp: {0:?}")]
pub struct ParseTimestampError(String);

impl Timestamp {
    /// The current UTC time.
    pub fn now() -> Self {
        Self(Utc::now())
    }

    /// The Unix epoch (`1970-01-01T00:00:00Z`).
    pub fn epoch() -> Self {
        Self(DateTime::UNIX_EPOCH)
    }

    /// A timestamp from seconds since the Unix epoch, as used by
    /// `SOURCE_DATE_EPOCH`. Returns `None` if out of range.
    pub fn from_unix(secs: i64) -> Option<Self> {
        DateTime::from_timestamp(secs, 0).map(Self)
    }

    /// The inner [`DateTime<Utc>`].
    pub fn as_datetime(&self) -> &DateTime<Utc> {
        &self.0
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(ts: Timestamp) -> DateTime<Utc> {
        ts.0
    }
}

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| Self(dt.with_timezone(&Utc)))
            .map_err(|_| ParseTimestampError(s.to_string()))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

// ── ImageTag ──────────────────────────────────────────────────────────────────

/// A valid OCI image tag.
///
/// # Invariants
///
/// - Matches `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}`: at most 128 characters,
///   not starting with `.` or `-`.
///
/// Ref.: <https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests>
///
/// # Example
///
/// ```
/// use paranoid::oci::ImageTag;
/// use std::convert::TryFrom;
///
/// let tag = ImageTag::try_from("v1.2.3").unwrap();
/// assert_eq!(tag.as_str(), "v1.2.3");
///
/// // "latest" is always a valid tag and is the default.
/// let tag = ImageTag::default();
/// assert_eq!(tag.as_str(), "latest");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageTag(String);

/// Error constructing an [`ImageTag`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImageTagError {
    #[error("image tag is empty")]
    Empty,
    #[error("image tag exceeds 128 characters: {0:?}")]
    TooLong(String),
    #[error("image tag contains invalid character {ch:?} in {tag:?}")]
    InvalidChar { ch: char, tag: String },
}

impl ImageTag {
    fn validate(s: &str) -> Result<(), ImageTagError> {
        let first = s.chars().next().ok_or(ImageTagError::Empty)?;
        if s.len() > 128 {
            return Err(ImageTagError::TooLong(s.to_string()));
        }
        let invalid = |ch| ImageTagError::InvalidChar {
            ch,
            tag: s.to_string(),
        };
        if !(first.is_ascii_alphanumeric() || first == '_') {
            return Err(invalid(first));
        }
        match s
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
        {
            Some(ch) => Err(invalid(ch)),
            None => Ok(()),
        }
    }
}

string_newtype!(ImageTag, ImageTagError);

impl Default for ImageTag {
    fn default() -> Self {
        // "latest" always satisfies the invariants.
        Self("latest".to_string())
    }
}

// ── ImageName ─────────────────────────────────────────────────────────────────

/// A valid OCI image name (the repository portion of an image reference).
///
/// # Invariants
///
/// - Non-empty.
/// - No `:` (reserved for separating name from tag), except for a registry
///   port in the first component, as in `localhost:5000/app`.
/// - No `@` (reserved for digest references).
///
/// # Example
///
/// ```
/// use paranoid::oci::ImageName;
/// use std::convert::TryFrom;
///
/// let name = ImageName::try_from("myregistry.io/myapp").unwrap();
/// assert_eq!(name.as_str(), "myregistry.io/myapp");
///
/// assert!(ImageName::try_from("name:tag").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageName(String);

/// Error constructing an [`ImageName`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImageNameError {
    #[error("image name is empty")]
    Empty,
    #[error("image name must not contain ':' (use image_tag for the tag): {0:?}")]
    ContainsColon(String),
    #[error("image name must not contain '@' (digest references are not supported here): {0:?}")]
    ContainsAt(String),
}

impl ImageName {
    fn validate(s: &str) -> Result<(), ImageNameError> {
        if s.is_empty() {
            return Err(ImageNameError::Empty);
        }
        if s.contains('@') {
            return Err(ImageNameError::ContainsAt(s.to_string()));
        }
        // A colon may only appear as a registry port: `host:port/path`.
        let path = match s.split_once('/') {
            Some((host, path)) if host.contains(':') => path,
            _ => s,
        };
        if path.contains(':') {
            return Err(ImageNameError::ContainsColon(s.to_string()));
        }
        Ok(())
    }
}

string_newtype!(ImageName, ImageNameError);

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // EnvVar
    #[test]
    fn env_var_valid() {
        let v = EnvVar::try_from("PATH=/usr/bin:/bin").unwrap();
        assert_eq!(v.key(), "PATH");
        assert_eq!(v.value(), "/usr/bin:/bin");
        assert_eq!(v.to_string(), "PATH=/usr/bin:/bin");
    }

    #[test]
    fn env_var_empty_value_valid() {
        let v = EnvVar::try_from("EMPTY=").unwrap();
        assert_eq!(v.key(), "EMPTY");
        assert_eq!(v.value(), "");
    }

    #[test]
    fn env_var_value_with_equals() {
        // value itself may contain '='
        let v = EnvVar::try_from("A=b=c").unwrap();
        assert_eq!(v.key(), "A");
        assert_eq!(v.value(), "b=c");
    }

    #[test]
    fn env_var_missing_separator() {
        assert!(matches!(
            EnvVar::try_from("NOEQUALS"),
            Err(EnvVarError::MissingSeparator(_))
        ));
    }

    #[test]
    fn env_var_empty_key() {
        assert!(matches!(
            EnvVar::try_from("=VALUE"),
            Err(EnvVarError::EmptyKey)
        ));
    }

    #[test]
    fn env_var_nul_byte() {
        assert!(matches!(
            EnvVar::try_from("KEY=val\0ue"),
            Err(EnvVarError::NulByte)
        ));
    }

    #[test]
    fn env_var_new_constructor() {
        let v = EnvVar::new("HOME", "/root").unwrap();
        assert_eq!(v.to_string(), "HOME=/root");
    }

    #[test]
    fn env_var_from_string_conversion() {
        let s: String = EnvVar::try_from("K=V").unwrap().into();
        assert_eq!(s, "K=V");
    }

    // AbsolutePath
    #[test]
    fn absolute_path_valid() {
        let p = AbsolutePath::try_from("/home/alice").unwrap();
        assert_eq!(p.as_str(), "/home/alice");
    }

    #[test]
    fn absolute_path_root_valid() {
        assert!(AbsolutePath::try_from("/").is_ok());
    }

    #[test]
    fn absolute_path_relative_rejected() {
        assert!(matches!(
            AbsolutePath::try_from("relative/path"),
            Err(AbsolutePathError::NotAbsolute(_))
        ));
    }

    #[test]
    fn absolute_path_empty_rejected() {
        // An empty string is not absolute on any platform.
        assert!(AbsolutePath::try_from("").is_err());
    }

    #[test]
    fn absolute_path_string_conversion() {
        let s: String = AbsolutePath::try_from("/usr/bin").unwrap().into();
        assert_eq!(s, "/usr/bin");
    }

    // Signal
    #[test]
    fn signal_parse_name() {
        assert_eq!("SIGTERM".parse::<Signal>().unwrap(), Signal::Sigterm);
        assert_eq!("SIGKILL".parse::<Signal>().unwrap(), Signal::Sigkill);
    }

    #[test]
    fn signal_parse_without_prefix() {
        assert_eq!("TERM".parse::<Signal>().unwrap(), Signal::Sigterm);
        assert_eq!("term".parse::<Signal>().unwrap(), Signal::Sigterm);
    }

    #[test]
    fn signal_parse_number() {
        assert_eq!("15".parse::<Signal>().unwrap(), Signal::Sigterm);
        assert_eq!("9".parse::<Signal>().unwrap(), Signal::Sigkill);
    }

    #[test]
    fn signal_parse_unknown() {
        assert!("SIGNOTREAL".parse::<Signal>().is_err());
        assert!("99".parse::<Signal>().is_err());
    }

    #[test]
    fn signal_number_16_is_undefined() {
        // Signal 16 has no standardised POSIX meaning on Linux; there is a gap
        // between SIGTERM (15) and SIGCHLD (17). Our closed enum must reject it.
        assert!("16".parse::<Signal>().is_err());
    }

    #[test]
    fn signal_display() {
        assert_eq!(Signal::Sigterm.to_string(), "SIGTERM");
    }

    #[test]
    fn signal_number() {
        assert_eq!(Signal::Sigterm.number(), 15);
        assert_eq!(Signal::Sigkill.number(), 9);
    }

    // Timestamp
    #[test]
    fn timestamp_parse_valid() {
        let ts: Timestamp = "2015-10-31T22:22:56Z".parse().unwrap();
        assert_eq!(ts.to_string(), "2015-10-31T22:22:56+00:00");
    }

    #[test]
    fn timestamp_parse_invalid() {
        assert!("not-a-date".parse::<Timestamp>().is_err());
        assert!("2015-13-01T00:00:00Z".parse::<Timestamp>().is_err());
    }

    #[test]
    fn timestamp_epoch() {
        let ts = Timestamp::epoch();
        assert_eq!(ts.to_string(), "1970-01-01T00:00:00+00:00");
    }

    #[test]
    fn timestamp_serde_roundtrip() {
        let ts: Timestamp = "2024-01-15T10:30:00Z".parse().unwrap();
        let json = serde_json::to_string(&ts).unwrap();
        let ts2: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, ts2);
    }

    // ImageTag
    #[test]
    fn image_tag_valid() {
        assert!(ImageTag::try_from("latest").is_ok());
        assert!(ImageTag::try_from("v1.2.3").is_ok());
        assert!(ImageTag::try_from("my-tag_2").is_ok());
    }

    #[test]
    fn image_tag_empty_rejected() {
        assert!(matches!(ImageTag::try_from(""), Err(ImageTagError::Empty)));
    }

    #[test]
    fn image_tag_too_long_rejected() {
        let long = "a".repeat(129);
        assert!(matches!(
            ImageTag::try_from(long.as_str()),
            Err(ImageTagError::TooLong(_))
        ));
    }

    #[test]
    fn image_tag_invalid_char_rejected() {
        assert!(matches!(
            ImageTag::try_from("tag:invalid"),
            Err(ImageTagError::InvalidChar { .. })
        ));
        assert!(matches!(
            ImageTag::try_from("tag@sha"),
            Err(ImageTagError::InvalidChar { .. })
        ));
    }

    #[test]
    fn image_tag_default_is_latest() {
        assert_eq!(ImageTag::default().as_str(), "latest");
    }

    // ImageName
    #[test]
    fn image_name_valid() {
        assert!(ImageName::try_from("myapp").is_ok());
        assert!(ImageName::try_from("registry.io/myapp").is_ok());
    }

    #[test]
    fn image_name_empty_rejected() {
        assert!(matches!(
            ImageName::try_from(""),
            Err(ImageNameError::Empty)
        ));
    }

    #[test]
    fn image_name_colon_rejected() {
        assert!(matches!(
            ImageName::try_from("name:tag"),
            Err(ImageNameError::ContainsColon(_))
        ));
    }

    #[test]
    fn image_name_at_rejected() {
        // '@' alone (no ':') should trigger ContainsAt
        assert!(matches!(
            ImageName::try_from("name@digest"),
            Err(ImageNameError::ContainsAt(_))
        ));
        // '@' with ':' triggers ContainsColon first — either error is correct
        assert!(ImageName::try_from("name@sha256:abc").is_err());
    }

    #[test]
    fn image_name_allows_registry_port() {
        assert!(ImageName::try_from("localhost:5000/app").is_ok());
        assert!(ImageName::try_from("registry.io:443/org/app").is_ok());
        assert!(ImageName::try_from("localhost:5000/app:tag").is_err());
        assert!(ImageName::try_from("app:tag").is_err());
    }

    #[test]
    fn image_tag_rejects_leading_dot_or_dash() {
        assert!(ImageTag::try_from(".hidden").is_err());
        assert!(ImageTag::try_from("-dash").is_err());
        assert!(ImageTag::try_from("_ok").is_ok());
    }

    #[test]
    fn string_newtypes_parse() {
        let tag: ImageTag = "v1".parse().unwrap();
        assert_eq!(String::from(tag), "v1");
        assert!("relative".parse::<AbsolutePath>().is_err());
    }

    #[test]
    fn signal_table_roundtrips() {
        for (signal, name, number) in SIGNALS {
            assert_eq!(signal.as_str(), *name);
            assert_eq!(signal.number(), *number);
            assert_eq!(name.parse::<Signal>().unwrap(), *signal);
            assert_eq!(number.to_string().parse::<Signal>().unwrap(), *signal);
            assert_eq!(name[3..].to_lowercase().parse::<Signal>().unwrap(), *signal);
        }
        assert!("16".parse::<Signal>().is_err());
        assert!("SIGNOPE".parse::<Signal>().is_err());
    }
}
