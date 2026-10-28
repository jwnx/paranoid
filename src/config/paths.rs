//! Files, directories and symlinks written straight from the manifest:
//! `[paths."<path>"]`.

use std::fmt;

use serde::Deserialize;

use super::FileMode;

/// One path in the image: a file with `content`, a symlink to `target`, or
/// a directory, with its owner and mode.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathSpec {
    /// A file's contents.
    pub content: Option<String>,
    /// Where a symlink points, verbatim.
    pub target: Option<String>,
    /// Its owner, by name or UID. Defaults to root.
    pub owner: Option<Owner>,
    /// Its group, by name or GID. Defaults to the owner's primary group.
    pub group: Option<Owner>,
    /// Defaults to `0644` for a file and `0755` for a directory. A symlink
    /// has none.
    pub mode: Option<FileMode>,
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind<'a> {
    File(&'a str),
    Symlink(&'a str),
    Directory,
}

/// A user or group, by name or by ID.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Owner {
    Id(u32),
    Name(String),
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

impl PathSpec {
    /// What the path is. Only valid for a spec that passed
    /// [`Self::check`].
    pub fn kind(&self) -> PathKind<'_> {
        match (&self.content, &self.target) {
            (Some(content), _) => PathKind::File(content),
            (_, Some(target)) => PathKind::Symlink(target),
            _ => PathKind::Directory,
        }
    }

    /// The permission bits it gets.
    pub fn mode(&self) -> u32 {
        match (self.mode, self.kind()) {
            (Some(mode), _) => mode.0,
            (None, PathKind::File(_)) => 0o644,
            (None, PathKind::Directory) => 0o755,
            (None, PathKind::Symlink(_)) => 0o777,
        }
    }

    pub(super) fn check(&self) -> Result<(), String> {
        match (&self.content, &self.target, self.mode) {
            (Some(_), Some(_), _) => {
                Err("a path has `content` (a file) or `target` (a symlink), not both".to_string())
            }
            (_, Some(target), _) if target.is_empty() || target.contains(['\0', '\n']) => {
                Err("a symlink's `target` can't be empty or have a NUL or a newline".to_string())
            }
            (_, Some(_), Some(_)) => Err("a symlink has no `mode`".to_string()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(toml: &str) -> Result<PathSpec, String> {
        let spec: PathSpec = toml::from_str(toml).map_err(|e| e.to_string())?;
        spec.check().map(|()| spec)
    }

    #[test]
    fn a_path_is_a_file_a_symlink_or_a_directory() {
        let file = spec("content = \"#!/bin/sh\\nexit 101\\n\"\nmode = \"0755\"").unwrap();
        assert_eq!(file.kind(), PathKind::File("#!/bin/sh\nexit 101\n"));
        assert_eq!(file.mode(), 0o755);

        let link = spec("target = \"../lib/x\"").unwrap();
        assert_eq!(link.kind(), PathKind::Symlink("../lib/x"));
        assert_eq!(link.mode(), 0o777);

        let dir = spec("owner = \"postgres\"\ngroup = 102\nmode = \"2775\"").unwrap();
        assert_eq!(dir.kind(), PathKind::Directory);
        assert_eq!(dir.owner, Some(Owner::Name("postgres".to_string())));
        assert_eq!(dir.group, Some(Owner::Id(102)));
        assert_eq!(dir.mode(), 0o2775);
        assert_eq!(spec("").unwrap().mode(), 0o755);

        for (toml, wanted) in [
            ("content = \"a\"\ntarget = \"b\"", "not both"),
            ("target = \"\"", "can't be empty"),
            ("target = \"b\"\nmode = \"0644\"", "a symlink has no `mode`"),
        ] {
            let err = spec(toml).unwrap_err();
            assert!(err.contains(wanted), "{toml}: {err}");
        }
    }
}
