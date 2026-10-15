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

    /// The full `KEY=VALUE` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for EnvVar {
    type Error = EnvVarError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s.contains('\0') {
            return Err(EnvVarError::NulByte);
        }
        let eq = s
            .find('=')
            .ok_or_else(|| EnvVarError::MissingSeparator(s.to_string()))?;
        if eq == 0 {
            return Err(EnvVarError::EmptyKey);
        }
        Ok(Self(s.to_string()))
    }
}

impl TryFrom<String> for EnvVar {
    type Error = EnvVarError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.contains('\0') {
            return Err(EnvVarError::NulByte);
        }
        let eq = s
            .find('=')
            .ok_or_else(|| EnvVarError::MissingSeparator(s.clone()))?;
        if eq == 0 {
            return Err(EnvVarError::EmptyKey);
        }
        Ok(Self(s))
    }
}

impl From<EnvVar> for String {
    fn from(v: EnvVar) -> String {
        v.0
    }
}

impl AsRef<str> for EnvVar {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for EnvVar {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EnvVar {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

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
    /// The path as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AbsolutePath {
    type Error = AbsolutePathError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if Path::new(s).is_absolute() {
            Ok(Self(s.to_string()))
        } else {
            Err(AbsolutePathError::NotAbsolute(s.to_string()))
        }
    }
}

impl TryFrom<String> for AbsolutePath {
    type Error = AbsolutePathError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Path::new(&s).is_absolute() {
            Ok(Self(s))
        } else {
            Err(AbsolutePathError::NotAbsolute(s))
        }
    }
}

impl From<AbsolutePath> for String {
    fn from(p: AbsolutePath) -> String {
        p.0
    }
}

impl AsRef<str> for AbsolutePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AbsolutePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AbsolutePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AbsolutePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

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

impl Signal {
    /// The canonical signal name string (e.g. `"SIGTERM"`).
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Sighup => "SIGHUP",
            Self::Sigint => "SIGINT",
            Self::Sigquit => "SIGQUIT",
            Self::Sigill => "SIGILL",
            Self::Sigtrap => "SIGTRAP",
            Self::Sigabrt => "SIGABRT",
            Self::Sigbus => "SIGBUS",
            Self::Sigfpe => "SIGFPE",
            Self::Sigkill => "SIGKILL",
            Self::Sigusr1 => "SIGUSR1",
            Self::Sigsegv => "SIGSEGV",
            Self::Sigusr2 => "SIGUSR2",
            Self::Sigpipe => "SIGPIPE",
            Self::Sigalrm => "SIGALRM",
            Self::Sigterm => "SIGTERM",
            Self::Sigchld => "SIGCHLD",
            Self::Sigcont => "SIGCONT",
            Self::Sigstop => "SIGSTOP",
            Self::Sigtstp => "SIGTSTP",
            Self::Sigttin => "SIGTTIN",
            Self::Sigttou => "SIGTTOU",
            Self::Sigurg => "SIGURG",
            Self::Sigxcpu => "SIGXCPU",
            Self::Sigxfsz => "SIGXFSZ",
            Self::Sigvtalrm => "SIGVTALRM",
            Self::Sigprof => "SIGPROF",
            Self::Sigwinch => "SIGWINCH",
            Self::Sigio => "SIGIO",
            Self::Sigpwr => "SIGPWR",
            Self::Sigsys => "SIGSYS",
        }
    }

    /// The standard signal number on Linux.
    pub const fn number(&self) -> u8 {
        match self {
            Self::Sighup => 1,
            Self::Sigint => 2,
            Self::Sigquit => 3,
            Self::Sigill => 4,
            Self::Sigtrap => 5,
            Self::Sigabrt => 6,
            Self::Sigbus => 7,
            Self::Sigfpe => 8,
            Self::Sigkill => 9,
            Self::Sigusr1 => 10,
            Self::Sigsegv => 11,
            Self::Sigusr2 => 12,
            Self::Sigpipe => 13,
            Self::Sigalrm => 14,
            Self::Sigterm => 15,
            Self::Sigchld => 17,
            Self::Sigcont => 18,
            Self::Sigstop => 19,
            Self::Sigtstp => 20,
            Self::Sigttin => 21,
            Self::Sigttou => 22,
            Self::Sigurg => 23,
            Self::Sigxcpu => 24,
            Self::Sigxfsz => 25,
            Self::Sigvtalrm => 26,
            Self::Sigprof => 27,
            Self::Sigwinch => 28,
            Self::Sigio => 29,
            Self::Sigpwr => 30,
            Self::Sigsys => 31,
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Signal {
    /// Look up a signal by its canonical `SIGFOO` name (must be uppercase, with prefix).
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "SIGHUP" => Some(Self::Sighup),
            "SIGINT" => Some(Self::Sigint),
            "SIGQUIT" => Some(Self::Sigquit),
            "SIGILL" => Some(Self::Sigill),
            "SIGTRAP" => Some(Self::Sigtrap),
            "SIGABRT" => Some(Self::Sigabrt),
            "SIGBUS" => Some(Self::Sigbus),
            "SIGFPE" => Some(Self::Sigfpe),
            "SIGKILL" => Some(Self::Sigkill),
            "SIGUSR1" => Some(Self::Sigusr1),
            "SIGSEGV" => Some(Self::Sigsegv),
            "SIGUSR2" => Some(Self::Sigusr2),
            "SIGPIPE" => Some(Self::Sigpipe),
            "SIGALRM" => Some(Self::Sigalrm),
            "SIGTERM" => Some(Self::Sigterm),
            "SIGCHLD" => Some(Self::Sigchld),
            "SIGCONT" => Some(Self::Sigcont),
            "SIGSTOP" => Some(Self::Sigstop),
            "SIGTSTP" => Some(Self::Sigtstp),
            "SIGTTIN" => Some(Self::Sigttin),
            "SIGTTOU" => Some(Self::Sigttou),
            "SIGURG" => Some(Self::Sigurg),
            "SIGXCPU" => Some(Self::Sigxcpu),
            "SIGXFSZ" => Some(Self::Sigxfsz),
            "SIGVTALRM" => Some(Self::Sigvtalrm),
            "SIGPROF" => Some(Self::Sigprof),
            "SIGWINCH" => Some(Self::Sigwinch),
            "SIGIO" => Some(Self::Sigio),
            "SIGPWR" => Some(Self::Sigpwr),
            "SIGSYS" => Some(Self::Sigsys),
            _ => None,
        }
    }
}

impl FromStr for Signal {
    type Err = ParseSignalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Try interpreting as a number first.
        if let Ok(n) = s.parse::<u8>() {
            return Self::from_number(n).ok_or_else(|| ParseSignalError(s.to_string()));
        }

        // Accept with or without the "SIG" prefix, case-insensitive.
        let upper = s.to_ascii_uppercase();
        let name = if upper.starts_with("SIG") {
            upper
        } else {
            format!("SIG{}", upper)
        };

        Self::from_name(&name).ok_or_else(|| ParseSignalError(s.to_string()))
    }
}

impl Signal {
    fn from_number(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Sighup),
            2 => Some(Self::Sigint),
            3 => Some(Self::Sigquit),
            4 => Some(Self::Sigill),
            5 => Some(Self::Sigtrap),
            6 => Some(Self::Sigabrt),
            7 => Some(Self::Sigbus),
            8 => Some(Self::Sigfpe),
            9 => Some(Self::Sigkill),
            10 => Some(Self::Sigusr1),
            11 => Some(Self::Sigsegv),
            12 => Some(Self::Sigusr2),
            13 => Some(Self::Sigpipe),
            14 => Some(Self::Sigalrm),
            15 => Some(Self::Sigterm),
            17 => Some(Self::Sigchld),
            18 => Some(Self::Sigcont),
            19 => Some(Self::Sigstop),
            20 => Some(Self::Sigtstp),
            21 => Some(Self::Sigttin),
            22 => Some(Self::Sigttou),
            23 => Some(Self::Sigurg),
            24 => Some(Self::Sigxcpu),
            25 => Some(Self::Sigxfsz),
            26 => Some(Self::Sigvtalrm),
            27 => Some(Self::Sigprof),
            28 => Some(Self::Sigwinch),
            29 => Some(Self::Sigio),
            30 => Some(Self::Sigpwr),
            31 => Some(Self::Sigsys),
            _ => None,
        }
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
/// - Non-empty.
/// - At most 128 characters.
/// - Contains only ASCII alphanumeric characters, `.`, `_`, or `-`.
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
    /// The tag string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ImageTag {
    fn default() -> Self {
        // "latest" always satisfies the invariants.
        Self("latest".to_string())
    }
}

impl TryFrom<&str> for ImageTag {
    type Error = ImageTagError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s.is_empty() {
            return Err(ImageTagError::Empty);
        }
        if s.len() > 128 {
            return Err(ImageTagError::TooLong(s.to_string()));
        }
        if let Some(ch) = s
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
        {
            return Err(ImageTagError::InvalidChar {
                ch,
                tag: s.to_string(),
            });
        }
        Ok(Self(s.to_string()))
    }
}

impl TryFrom<String> for ImageTag {
    type Error = ImageTagError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.is_empty() {
            return Err(ImageTagError::Empty);
        }
        if s.len() > 128 {
            return Err(ImageTagError::TooLong(s));
        }
        if let Some(ch) = s
            .chars()
            .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '_' | '-'))
        {
            return Err(ImageTagError::InvalidChar { ch, tag: s });
        }
        Ok(Self(s))
    }
}

impl From<ImageTag> for String {
    fn from(t: ImageTag) -> String {
        t.0
    }
}

impl AsRef<str> for ImageTag {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ImageTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ImageTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

// ── ImageName ─────────────────────────────────────────────────────────────────

/// A valid OCI image name (the repository portion of an image reference).
///
/// # Invariants
///
/// - Non-empty.
/// - Does not contain `:` (that is reserved for separating name from tag).
/// - Does not contain `@` (that is reserved for digest references).
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
    /// The name string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ImageName {
    type Error = ImageNameError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s.is_empty() {
            return Err(ImageNameError::Empty);
        }
        if s.contains(':') {
            return Err(ImageNameError::ContainsColon(s.to_string()));
        }
        if s.contains('@') {
            return Err(ImageNameError::ContainsAt(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
}

impl TryFrom<String> for ImageName {
    type Error = ImageNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if s.is_empty() {
            return Err(ImageNameError::Empty);
        }
        if s.contains(':') {
            return Err(ImageNameError::ContainsColon(s));
        }
        if s.contains('@') {
            return Err(ImageNameError::ContainsAt(s));
        }
        Ok(Self(s))
    }
}

impl From<ImageName> for String {
    fn from(n: ImageName) -> String {
        n.0
    }
}

impl AsRef<str> for ImageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ImageName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ImageName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

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
}
