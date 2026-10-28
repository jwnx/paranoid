//! Files fetched from a URL rather than a repository: `[files.<id>]`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::hash::Sha256;

/// A file pinned by its URL and SHA-256, and where it goes in the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSpec {
    #[serde(deserialize_with = "deserialize_file_url")]
    pub url: Url,
    pub sha256: Sha256,
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
}

impl FileSpec {
    /// Whether the spec makes sense as a whole.
    pub(super) fn check(&self) -> Result<(), String> {
        if self.mode.is_some() && matches!(self.extract, Some(Extract::Dir(_))) {
            return Err(
                "`mode` applies to one file, not to a directory extracted from an archive"
                    .to_string(),
            );
        }
        Ok(())
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
}
