//! Files fetched from a URL rather than a repository: `[files.<id>]`.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::hash::Sha256;

/// A file fetched from a URL rather than a repository, and where it goes
/// in the image. It comes from a `url`, pinned by its SHA-256 or signed by
/// keys the manifest names, or both; or it's a release that `lock`
/// resolves from an index: Go's (`go`), or a Google Cloud Storage bucket's
/// (`gcs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_file_url"
    )]
    pub url: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256>,
    /// Where the file goes in the image, or where an extracted directory's
    /// contents go.
    pub path: ImagePath,
    /// Permissions for the file. Defaults to `0644` for a download placed
    /// as it is, and to the archive's for an extracted member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FileMode>,
    /// What to take out of the download, a tar archive: one member, or
    /// everything under a directory when it ends in `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<Extract>,
    /// OpenPGP keys allowed to sign the file, relative to the manifest, as
    /// a repository's `signed_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<PathBuf>,
    /// A detached signature: over the file, or over `checksums` if there
    /// is one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_file_url"
    )]
    pub signature: Option<Url>,
    /// A checksums file listing the file's SHA-256 by the last part of its
    /// URL, as `sha256sum` writes it. Signed by `signature`, or clearsigned
    /// itself.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_file_url"
    )]
    pub checksums: Option<Url>,
    /// A Go release, resolved from go.dev's list of them: the Linux
    /// tarball for the image's architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<VersionPrefix>,
    /// A public Google Cloud Storage bucket with a directory per release,
    /// named for its version (`v3.6.16/`), each with a `SHA256SUMS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs: Option<String>,
    /// The releases of `gcs` that will do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionPrefix>,
    /// The file in a `gcs` release's directory, with `{version}` and
    /// `{arch}` filled in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
    /// Where the release directories are in a `gcs` bucket, if not at the
    /// top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Where a file comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    /// A URL, as it is.
    Url(&'a Url),
    /// The newest Go release that `version` allows.
    Go { version: &'a VersionPrefix },
    /// The newest release in a GCS bucket that `version` allows.
    Gcs {
        bucket: &'a str,
        /// Empty, or ending in `/`.
        prefix: &'a str,
        version: &'a VersionPrefix,
        asset: &'a str,
    },
}

impl FileSpec {
    /// Where the file comes from. Only valid for a spec that passed
    /// [`Self::check`], which makes sure there's exactly one source.
    pub fn source(&self) -> Source<'_> {
        match (&self.url, &self.go, &self.gcs) {
            (Some(url), _, _) => Source::Url(url),
            (_, Some(version), _) => Source::Go { version },
            (_, _, Some(bucket)) => Source::Gcs {
                bucket,
                prefix: self.prefix.as_deref().unwrap_or(""),
                version: self.version.as_ref().expect("checked"),
                asset: self.asset.as_deref().expect("checked"),
            },
            _ => unreachable!("a checked spec has a source"),
        }
    }

    /// Whether the spec makes sense as a whole.
    pub(super) fn check(&self) -> Result<(), String> {
        let sources = [self.url.is_some(), self.go.is_some(), self.gcs.is_some()];
        if sources.iter().filter(|&&s| s).count() != 1 {
            return Err("give it exactly one of `url`, `go` and `gcs`".to_string());
        }
        let only = |source: &str, allowed: &[&str]| -> Result<(), String> {
            let given = [
                ("sha256", self.sha256.is_some()),
                ("signed_by", self.signed_by.is_some()),
                ("signature", self.signature.is_some()),
                ("checksums", self.checksums.is_some()),
                ("version", self.version.is_some()),
                ("asset", self.asset.is_some()),
                ("prefix", self.prefix.is_some()),
            ];
            match given.iter().find(|(k, set)| *set && !allowed.contains(k)) {
                Some((key, _)) => Err(format!("`{key}` doesn't apply to a file from `{source}`")),
                None => Ok(()),
            }
        };
        if let Some(bucket) = &self.gcs {
            only("gcs", &["version", "asset", "prefix"])?;
            // GCS's rules: 3 to 63 of these, starting and ending with a
            // letter or digit.
            let edge =
                |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            if !(3..=63).contains(&bucket.len())
                || !edge(bucket.chars().next())
                || !edge(bucket.chars().last())
                || !bucket
                    .chars()
                    .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '.' | '-' | '_'))
            {
                return Err(format!("'{bucket}' is not a bucket name"));
            }
            if self.version.is_none() || self.asset.is_none() {
                return Err("a file from `gcs` needs a `version` and an `asset`".to_string());
            }
            // Both end up in URLs, so nothing a URL would read as more
            // than a path.
            let urlish = |s: &str| s.contains([':', '\\', '%', '?', '#']);
            if self
                .asset
                .as_ref()
                .is_some_and(|a| a.is_empty() || a.contains('/') || urlish(a))
            {
                return Err(
                    "`asset` is a file name, without '/', ':', '\\', '%', '?' or '#'".to_string(),
                );
            }
            if let Some(prefix) = &self.prefix
                && (prefix.starts_with('/')
                    || !prefix.ends_with('/')
                    || urlish(prefix)
                    || prefix.split('/').any(|c| c == ".." || c == "."))
            {
                return Err(format!(
                    "`prefix` '{prefix}' must be a relative directory ending in '/'"
                ));
            }
            return self.check_placement();
        }
        if self.go.is_some() {
            only("go", &["signed_by"])?;
            return self.check_placement();
        }
        only("url", &["sha256", "signed_by", "signature", "checksums"])?;
        let signs = self.signature.is_some() || self.checksums.is_some();
        match (self.sha256.is_some(), &self.signed_by, signs) {
            (_, None, true) => {
                return Err("a `signature` or `checksums` needs `signed_by`".to_string());
            }
            (false, None, false) => {
                return Err(
                    "pin it with `sha256`, or name the keys that sign it with `signed_by`"
                        .to_string(),
                );
            }
            (_, Some(_), false) => {
                return Err("`signed_by` needs a `signature` or `checksums` to check".to_string());
            }
            _ => {}
        }
        if self
            .signed_by
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("`signed_by` is empty".to_string());
        }
        if self.checksums.is_some() && self.name().is_none() {
            return Err(format!(
                "a checksums file lists files by name, and '{}' doesn't end in one",
                self.url.as_ref().expect("checked")
            ));
        }
        self.check_placement()
    }

    /// Whether `mode` and `extract` go together.
    fn check_placement(&self) -> Result<(), String> {
        if self
            .signed_by
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("`signed_by` is empty".to_string());
        }
        if self.mode.is_some() && matches!(self.extract, Some(Extract::Dir(_))) {
            return Err(
                "`mode` applies to one file, not to a directory extracted from an archive"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl FileSpec {
    /// The last part of the URL's path: the name a checksums file lists
    /// the file by.
    pub fn name(&self) -> Option<&str> {
        self.url
            .as_ref()?
            .path_segments()?
            .next_back()
            .filter(|s| !s.is_empty())
    }
}

/// An absolute path in the image, other than the root, without `.` or
/// `..`: `/usr/local/bin/etcdctl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePath(String);

impl ImagePath {
    /// The path relative to the root, as an archive spells it:
    /// `usr/local/bin/etcdctl`.
    pub fn relative(&self) -> &str {
        &self.0[1..]
    }
}

impl FromStr for ImagePath {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some(rest) = s.strip_prefix('/') else {
            return Err(format!("path '{s}' must be absolute"));
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            return Err("path can't be the root".to_string());
        }
        if rest.split('/').any(|c| matches!(c, "" | "." | "..")) {
            return Err(format!(
                "path '{s}' must not have empty, '.' or '..' components"
            ));
        }
        Ok(Self(format!("/{rest}")))
    }
}

impl fmt::Display for ImagePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to take out of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extract {
    /// One member, by its path in the archive: `etcd-v3.6.16/etcdctl`.
    Member(String),
    /// Everything under a directory, written with a trailing `/`: `go/`.
    Dir(String),
}

impl Extract {
    /// The archive path as written in the manifest.
    pub fn as_str(&self) -> String {
        match self {
            Self::Member(path) => path.clone(),
            Self::Dir(path) => format!("{path}/"),
        }
    }
}

impl FromStr for Extract {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = s.trim_start_matches("./");
        let (path, dir) = match path.strip_suffix('/') {
            Some(dir) => (dir, true),
            None => (path, false),
        };
        if path.is_empty()
            || path.starts_with('/')
            || path.split('/').any(|c| matches!(c, "" | "." | ".."))
        {
            return Err(format!(
                "extract '{s}' must be a relative path in the archive, without '.' or '..'"
            ));
        }
        Ok(if dir {
            Self::Dir(path.to_string())
        } else {
            Self::Member(path.to_string())
        })
    }
}

/// Which releases will do: a version, or a prefix of one. `"1.27"` takes
/// the newest 1.27.x, `"1.27.3"` takes just that one. Prereleases never
/// match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPrefix(Vec<u64>);

impl VersionPrefix {
    /// Whether a release, by its publisher's name for it (`go1.27.3`,
    /// `v3.6.16`, `3.6.16`), is one that will do.
    pub fn matches(&self, release: &str) -> bool {
        release_version(release).is_some_and(|v| v.starts_with(&self.0))
    }
}

/// A release's version as numbers, padded to three: `go1.20` is
/// `[1, 20, 0]`. `None` for a prerelease like `go1.27rc1` or `v3.6.0-rc.1`,
/// or anything else that isn't a plain version.
pub fn release_version(release: &str) -> Option<Vec<u64>> {
    // Release names end up in URLs, so nothing but a known prefix and the
    // version itself.
    let digits = ["go", "v", ""]
        .iter()
        .find_map(|prefix| release.strip_prefix(prefix))
        .filter(|d| d.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts: Vec<u64> = digits
        .split('.')
        .map(|p| {
            p.parse()
                .ok()
                .filter(|_| p.chars().all(|c| c.is_ascii_digit()))
        })
        .collect::<Option<_>>()?;
    if parts.len() > 3 {
        return None;
    }
    parts.resize(3, 0);
    Some(parts)
}

impl FromStr for VersionPrefix {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Option<Vec<u64>> = s
            .split('.')
            .map(|p| {
                p.parse()
                    .ok()
                    .filter(|_| p.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        match parts {
            Some(parts) if (1..=3).contains(&parts.len()) => Ok(Self(parts)),
            _ => Err(format!("version '{s}' must be like \"1.27\" or \"1.27.3\"")),
        }
    }
}

impl fmt::Display for VersionPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u64::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

/// Permission bits, written in octal: `"0755"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(pub u32);

impl FromStr for FileMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match u32::from_str_radix(s, 8) {
            Ok(mode) if mode <= 0o7777 && !s.is_empty() => Ok(Self(mode)),
            _ => Err(format!(
                "mode '{s}' must be octal permission bits, like \"0755\""
            )),
        }
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04o}", self.0)
    }
}

/// Serialize and deserialize through `Display` and `FromStr`.
macro_rules! string_serde {
    ($($ty:ty: $display:expr),* $(,)?) => {$(
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let display: fn(&$ty) -> String = $display;
                serializer.serialize_str(&display(self))
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(serde::de::Error::custom)
            }
        }
    )*};
}

string_serde! {
    VersionPrefix: |v| v.to_string(),
    ImagePath: |p| p.to_string(),
    Extract: |e| e.as_str(),
    FileMode: |m| m.to_string(),
}

/// A file's URL: http or https, without credentials. Unlike a repository's,
/// it may have a query, since nothing is appended to it.
fn deserialize_file_url<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Url, D::Error> {
    let s = String::deserialize(deserializer)?;
    let url = Url::parse(&s).map_err(serde::de::Error::custom)?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(serde::de::Error::custom(format!(
            "URL must use http or https scheme, got '{}'",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(serde::de::Error::custom(format!(
            "'{}' has credentials in it, which would show up in logs",
            url.host_str().unwrap_or_default()
        )));
    }
    Ok(url)
}

fn deserialize_optional_file_url<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Url>, D::Error> {
    deserialize_file_url(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(toml: &str) -> Result<FileSpec, String> {
        let spec: FileSpec = toml::from_str(&format!(
            "url = \"https://example.com/tool.tar.gz\"\n\
             sha256 = \"{}\"\n{toml}",
            Sha256::of(b"tool")
        ))
        .map_err(|e| e.to_string())?;
        spec.check()?;
        Ok(spec)
    }

    #[test]
    fn parses_a_file_with_everything() {
        let spec =
            spec("path = \"/usr/local/bin/tool\"\nmode = \"0755\"\nextract = \"tool-1.0/tool\"")
                .unwrap();
        assert_eq!(spec.path.relative(), "usr/local/bin/tool");
        assert_eq!(spec.mode, Some(FileMode(0o755)));
        assert_eq!(spec.extract, Some(Extract::Member("tool-1.0/tool".into())));
    }

    #[test]
    fn a_trailing_slash_extracts_a_directory() {
        let spec = spec("path = \"/usr/local/go/\"\nextract = \"./go/\"").unwrap();
        assert_eq!(spec.path.to_string(), "/usr/local/go");
        assert_eq!(spec.extract, Some(Extract::Dir("go".into())));
        assert_eq!(spec.extract.unwrap().as_str(), "go/");
    }

    #[test]
    fn rejects_bad_paths() {
        for path in [
            "usr/bin/tool",
            "/",
            "/usr/../etc/passwd",
            "/usr//bin",
            "/./x",
        ] {
            let err = spec(&format!("path = \"{path}\"")).unwrap_err();
            assert!(err.contains("path"), "{path}: {err}");
        }
        for extract in ["/go/", "../x", "a/../../b", "", "/"] {
            let err = spec(&format!("path = \"/x\"\nextract = \"{extract}\"")).unwrap_err();
            assert!(err.contains("extract"), "{extract}: {err}");
        }
    }

    #[test]
    fn rejects_bad_modes() {
        for mode in ["755x", "17777", "", "0o755"] {
            let err = spec(&format!("path = \"/x\"\nmode = \"{mode}\"")).unwrap_err();
            assert!(err.contains("mode"), "{mode}: {err}");
        }
        let err = spec("path = \"/x\"\nmode = \"0755\"\nextract = \"go/\"").unwrap_err();
        assert!(err.contains("not to a directory"), "{err}");
    }

    #[test]
    fn a_url_may_have_a_query_but_not_credentials() {
        let ok: Result<FileSpec, _> = toml::from_str(&format!(
            "url = \"https://example.com/dl?file=tool\"\nsha256 = \"{}\"\npath = \"/x\"",
            Sha256::of(b"")
        ));
        assert!(ok.is_ok());
        let err = toml::from_str::<FileSpec>(&format!(
            "url = \"https://me:secret@example.com/tool\"\nsha256 = \"{}\"\npath = \"/x\"",
            Sha256::of(b"")
        ))
        .unwrap_err();
        assert!(err.to_string().contains("has credentials"), "{err}");
    }

    #[test]
    fn a_file_is_pinned_or_signed() {
        let parse = |toml: &str| -> Result<FileSpec, String> {
            let spec: FileSpec = toml::from_str(&format!(
                "url = \"https://example.com/tool.tar.gz\"\npath = \"/x\"\n{toml}"
            ))
            .map_err(|e| e.to_string())?;
            spec.check().map(|()| spec)
        };
        let err = parse("").unwrap_err();
        assert!(err.contains("pin it with `sha256`"), "{err}");
        let err = parse("signed_by = \"keys.asc\"").unwrap_err();
        assert!(err.contains("needs a `signature` or `checksums`"), "{err}");
        let err = parse("signature = \"https://example.com/tool.tar.gz.asc\"").unwrap_err();
        assert!(err.contains("needs `signed_by`"), "{err}");

        let signed =
            parse("signed_by = \"keys.asc\"\nchecksums = \"https://example.com/SHA256SUMS.asc\"")
                .unwrap();
        assert_eq!(signed.name(), Some("tool.tar.gz"));
        assert!(signed.sha256.is_none());
    }

    #[test]
    fn a_file_has_one_source_and_only_its_keys() {
        let parse = |toml: &str| -> Result<FileSpec, String> {
            let spec: FileSpec =
                toml::from_str(&format!("path = \"/x\"\n{toml}")).map_err(|e| e.to_string())?;
            spec.check().map(|()| spec)
        };
        let etcd = "gcs = \"etcd\"\nversion = \"3.6\"\nasset = \"etcd-v{version}.tar.gz\"";
        assert!(parse(etcd).is_ok());
        assert!(parse("go = \"1.27\"\nsigned_by = \"google.asc\"").is_ok());

        for (toml, wanted) in [
            (
                "go = \"1.27\"\nurl = \"https://example.com/x\"",
                "exactly one of",
            ),
            ("", "exactly one of"),
            (
                "go = \"1.27\"\nsha256 = \"{0}\"",
                "`sha256` doesn't apply to a file from `go`",
            ),
            (
                "go = \"1.27\"\nversion = \"1.27\"",
                "`version` doesn't apply",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3.6\"",
                "needs a `version` and an `asset`",
            ),
            (
                "gcs = \"Etcd!\"\nversion = \"3\"\nasset = \"x\"",
                "not a bucket name",
            ),
            (
                "gcs = \"..\"\nversion = \"3\"\nasset = \"x\"",
                "not a bucket name",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3\"\nasset = \"http:evil.example\"",
                "without '/', ':'",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3\"\nasset = \"x\"\nprefix = \"%2e%2e/b/\"",
                "ending in '/'",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3\"\nasset = \"a/b\"",
                "without '/'",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3\"\nasset = \"x\"\nprefix = \"rel\"",
                "ending in '/'",
            ),
            (
                "gcs = \"etcd\"\nversion = \"3\"\nasset = \"x\"\nsigned_by = \"k.asc\"",
                "`signed_by` doesn't apply",
            ),
            (
                "url = \"https://example.com/x\"\nsha256 = \"{0}\"\nversion = \"1\"",
                "`version` doesn't apply",
            ),
        ] {
            let toml = toml.replace("{0}", &Sha256::of(b"").to_string());
            let err = parse(&toml).unwrap_err();
            assert!(err.contains(wanted), "{toml}: {err}");
        }
    }

    #[test]
    fn matches_releases_by_their_version() {
        let v = |s: &str| s.parse::<VersionPrefix>().unwrap();
        assert!(v("1.27").matches("go1.27.3"));
        assert!(v("1.27").matches("go1.27"));
        assert!(v("1.27.0").matches("go1.27"));
        assert!(v("1.27.3").matches("go1.27.3"));
        assert!(!v("1.27.3").matches("go1.27.30"));
        assert!(!v("1.2").matches("go1.27.3"));
        assert!(!v("1.27").matches("go1.27rc1"));
        assert!(v("3.6").matches("v3.6.16"));
        assert!(!v("3.6").matches("v3.6.0-rc.1"));
        assert!(!v("3").matches("release-3.6.1"));
        for bad in ["", "1.x", "v1.2", "1.2.3.4", "1..2"] {
            assert!(bad.parse::<VersionPrefix>().is_err(), "{bad}");
        }
        assert_eq!(release_version("go1.20"), Some(vec![1, 20, 0]));
        assert!(release_version("go1.27.3") > release_version("go1.27.2"));
        assert!(release_version("v3.6.10") > release_version("v3.6.9"));
    }
}
