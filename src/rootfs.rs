//! An in-memory root filesystem assembled from package archives.
//!
//! Building an image used to mean extracting every package onto the host and
//! then walking the result back into a tar. That leaked the host into the
//! image (file ownership came from whoever ran the build), made the outcome
//! depend on extraction timing, and let a crafted package write outside the
//! extraction directory through symlinks.
//!
//! Instead we treat the rootfs as data. Each package's `data.tar` is applied
//! to a [`Rootfs`], in lock order, and the image layer is serialized straight
//! from it:
//!
//! - Ownership and modes come from the package's tar headers.
//! - Paths are resolved component by component inside the rootfs. Symlinks
//!   are followed the way dpkg follows them on a real system, but `..` and
//!   absolute targets can never climb above the root, because there is
//!   nothing above it.
//! - When two packages ship the same path, the later one in lock order wins.

use std::collections::{BTreeMap, VecDeque};
use std::io::Read;
use std::sync::Arc;

use thiserror::Error;

use crate::deb::{DebArchive, DebError};

/// Permissions for directories a package implies but doesn't ship.
const IMPLICIT_DIR_MODE: u32 = 0o755;

/// How many symlinks we follow while resolving one path, matching Linux's
/// `MAXSYMLINKS`.
const MAX_SYMLINK_HOPS: usize = 40;

/// Errors applying a package to a [`Rootfs`].
#[derive(Debug, Error)]
pub enum RootfsError {
    #[error(transparent)]
    Deb(#[from] DebError),

    #[error("failed to read package data")]
    Io(#[from] std::io::Error),

    #[error("path '{0}' climbs out of the package root")]
    EscapingPath(String),

    #[error("'{0}' is not a directory")]
    NotADirectory(String),

    #[error("cannot replace non-empty directory '{0}' with a non-directory")]
    ReplacesDirectory(String),

    #[error("cannot replace '{0}' with a directory")]
    ReplacedByDirectory(String),

    #[error("too many levels of symbolic links resolving '{0}'")]
    SymlinkLoop(String),

    #[error("hard link '{link}' points to '{target}', which is not a regular file")]
    BadHardlink { link: String, target: String },
}

/// Ownership and permission bits of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// Permission bits, including setuid/setgid/sticky (`0o7777`).
    pub mode: u32,
    pub uid: u64,
    pub gid: u64,
}

impl Meta {
    fn implicit_dir() -> Self {
        Self {
            mode: IMPLICIT_DIR_MODE,
            uid: 0,
            gid: 0,
        }
    }

    fn from_header(header: &tar::Header) -> Result<Self, std::io::Error> {
        Ok(Self {
            mode: header.mode()? & 0o7777,
            uid: header.uid()?,
            gid: header.gid()?,
        })
    }
}

/// One entry in the rootfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Dir(Meta),
    File(Meta, Arc<[u8]>),
    /// A symbolic link; the target is kept verbatim.
    Symlink(Meta, String),
    /// A hard link to `target`. We keep the contents as well, so the link
    /// stays valid even if a later package replaces the target.
    Hardlink {
        target: String,
        meta: Meta,
        data: Arc<[u8]>,
    },
}

impl Node {
    pub fn meta(&self) -> &Meta {
        match self {
            Node::Dir(meta) | Node::File(meta, _) | Node::Symlink(meta, _) => meta,
            Node::Hardlink { meta, .. } => meta,
        }
    }
}

/// What applying one package did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyStats {
    /// Entries read from the package.
    pub entries: usize,
    /// Entries skipped because we don't model them (device nodes, FIFOs).
    pub skipped: usize,
    /// Every path in the package, in archive order: what dpkg lists in
    /// `/var/lib/dpkg/info/<pkg>.list`.
    pub paths: Vec<InstalledPath>,
}

/// A path a package installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPath {
    /// As dpkg lists it: `/.`, `/usr`, `/usr/bin/bash`.
    pub listed: String,
    /// Where it ended up in the rootfs, after following symlinked parents
    /// (see [`Rootfs::get`]). `None` for the root itself.
    pub key: Option<String>,
}

/// An in-memory root filesystem.
///
/// Paths are stored relative to the root, without a leading `/` or `./`,
/// e.g. `usr/bin/bash`. Iteration is in byte order, so parents always come
/// before their children.
#[derive(Debug, Default, Clone)]
pub struct Rootfs {
    nodes: BTreeMap<String, Node>,
}

impl Rootfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries, not counting the root itself.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Look up a node by its path relative to the root.
    pub fn get(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }

    /// All entries in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(path, node)| (path.as_str(), node))
    }

    /// Move a file to a new path, as dpkg does with a `.dpkg-new` conffile.
    /// Returns whether there was a file to move.
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<bool, RootfsError> {
        match self.nodes.get(from) {
            Some(Node::File(..) | Node::Hardlink { .. }) => {
                let node = self.nodes.remove(from).expect("just looked it up");
                self.replace(to.to_string(), node)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Apply a `.deb` package's files.
    pub fn apply_deb(&mut self, deb: &[u8]) -> Result<ApplyStats, RootfsError> {
        let deb = DebArchive::new(deb)?;
        self.apply_tar(deb.data_tar()?)
    }

    /// Apply the entries of an uncompressed tar stream.
    pub fn apply_tar(&mut self, tar: impl Read) -> Result<ApplyStats, RootfsError> {
        let mut stats = ApplyStats::default();
        let mut archive = tar::Archive::new(tar);

        for entry in archive.entries()? {
            let mut entry = entry?;
            stats.entries += 1;

            let raw_path = entry.path()?.to_string_lossy().into_owned();
            let components = components(&raw_path)?;
            let Some((name, parents)) = components.split_last() else {
                stats.paths.push(InstalledPath {
                    listed: "/.".to_string(),
                    key: None,
                });
                continue; // the root directory itself
            };

            let meta = Meta::from_header(entry.header())?;
            let parent = self.resolve_dir(parents.iter().copied(), &raw_path)?;
            let path = join(&parent, name);
            stats.paths.push(InstalledPath {
                listed: format!("/{}", components.join("/")),
                key: Some(path.clone()),
            });

            match entry.header().entry_type() {
                tar::EntryType::Directory => self.add_dir(path, meta)?,
                tar::EntryType::Regular | tar::EntryType::Continuous => {
                    let mut data = Vec::with_capacity(entry.size() as usize);
                    entry.read_to_end(&mut data)?;
                    self.replace(path, Node::File(meta, data.into()))?;
                }
                tar::EntryType::Symlink => {
                    let target = link_name(&entry, &raw_path)?;
                    self.replace(path, Node::Symlink(meta, target))?;
                }
                tar::EntryType::Link => {
                    let target = link_name(&entry, &raw_path)?;
                    let node = self.hardlink(&path, &target)?;
                    self.replace(path, node)?;
                }
                _ => stats.skipped += 1,
            }
        }

        Ok(stats)
    }

    /// Add a directory entry. An existing directory takes the new
    /// permissions; an existing symlink to a directory is kept, as dpkg does
    /// for merged-`/usr` style links.
    fn add_dir(&mut self, path: String, meta: Meta) -> Result<(), RootfsError> {
        match self.nodes.get_mut(&path) {
            None => {
                self.nodes.insert(path, Node::Dir(meta));
            }
            Some(Node::Dir(existing)) => *existing = meta,
            Some(Node::Symlink(..)) => {
                self.resolve_dir(path.split('/'), &path)?;
            }
            Some(Node::File(..) | Node::Hardlink { .. }) => {
                return Err(RootfsError::ReplacedByDirectory(path));
            }
        }
        Ok(())
    }

    /// Insert a non-directory node, replacing whatever was there unless it's
    /// a directory with contents.
    fn replace(&mut self, path: String, node: Node) -> Result<(), RootfsError> {
        if let Some(Node::Dir(_)) = self.nodes.get(&path)
            && self.has_children(&path)
        {
            return Err(RootfsError::ReplacesDirectory(path));
        }
        self.nodes.insert(path, node);
        Ok(())
    }

    /// Build a hard link node to `target`, an archive path.
    fn hardlink(&mut self, link: &str, target: &str) -> Result<Node, RootfsError> {
        let target_components = components(target)?;
        let bad = || RootfsError::BadHardlink {
            link: link.to_string(),
            target: target.to_string(),
        };
        let (name, parents) = target_components.split_last().ok_or_else(bad)?;
        let parent = self.resolve_dir(parents.iter().copied(), target)?;
        let target = join(&parent, name);

        match self.nodes.get(&target) {
            Some(Node::File(meta, data)) => Ok(Node::Hardlink {
                meta: *meta,
                data: data.clone(),
                target,
            }),
            Some(Node::Hardlink { meta, data, .. }) => Ok(Node::Hardlink {
                meta: *meta,
                data: data.clone(),
                target,
            }),
            _ => Err(bad()),
        }
    }

    fn has_children(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        self.nodes
            .range(prefix.clone()..)
            .next()
            .is_some_and(|(path, _)| path.starts_with(&prefix))
    }

    /// Resolve a directory path inside the rootfs, following symlinks and
    /// creating missing directories, and return its canonical path.
    ///
    /// Resolution keeps a stack of canonical components: `..` pops it and an
    /// absolute symlink clears it, so the result is always inside the root.
    fn resolve_dir<'a>(
        &mut self,
        path: impl IntoIterator<Item = &'a str>,
        display: &str,
    ) -> Result<String, RootfsError> {
        let mut pending: VecDeque<String> = path.into_iter().map(str::to_string).collect();
        let mut resolved: Vec<String> = Vec::new();
        let mut hops = 0;

        while let Some(component) = pending.pop_front() {
            match component.as_str() {
                "" | "." => continue,
                ".." => {
                    resolved.pop();
                    continue;
                }
                _ => resolved.push(component),
            }

            let key = resolved.join("/");
            match self.nodes.get(&key) {
                None => {
                    self.nodes.insert(key, Node::Dir(Meta::implicit_dir()));
                }
                Some(Node::Dir(_)) => {}
                Some(Node::Symlink(_, target)) => {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err(RootfsError::SymlinkLoop(display.to_string()));
                    }
                    resolved.pop();
                    if target.starts_with('/') {
                        resolved.clear();
                    }
                    for part in target.split('/').rev() {
                        pending.push_front(part.to_string());
                    }
                }
                Some(Node::File(..) | Node::Hardlink { .. }) => {
                    return Err(RootfsError::NotADirectory(key));
                }
            }
        }

        Ok(resolved.join("/"))
    }
}

/// Split an archive path into normal components, dropping `.` and a leading
/// `/`. Archive paths with `..` are rejected outright: no package has a
/// legitimate reason to ship one.
fn components(path: &str) -> Result<Vec<&str>, RootfsError> {
    let mut out = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(RootfsError::EscapingPath(path.to_string())),
            name => out.push(name),
        }
    }
    Ok(out)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn link_name<R: Read>(entry: &tar::Entry<'_, R>, path: &str) -> Result<String, RootfsError> {
    entry
        .link_name()?
        .map(|target| target.to_string_lossy().into_owned())
        .ok_or_else(|| RootfsError::BadHardlink {
            link: path.to_string(),
            target: String::new(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an uncompressed tar from a compact description.
    enum E<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8], u32),
        Symlink(&'a str, &'a str),
        Hardlink(&'a str, &'a str),
    }

    fn tar(entries: &[E<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            header.set_uid(0);
            header.set_gid(0);
            match *entry {
                E::Dir(path) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                    builder.append_data(&mut header, path, std::io::empty())
                }
                E::File(path, data, mode) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(mode);
                    header.set_size(data.len() as u64);
                    builder.append_data(&mut header, path, data)
                }
                E::Symlink(path, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    header.set_size(0);
                    builder.append_link(&mut header, path, target)
                }
                E::Hardlink(path, target) => {
                    header.set_entry_type(tar::EntryType::Link);
                    header.set_mode(0o644);
                    header.set_size(0);
                    builder.append_link(&mut header, path, target)
                }
            }
            .unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn apply(fs: &mut Rootfs, entries: &[E<'_>]) -> Result<ApplyStats, RootfsError> {
        fs.apply_tar(tar(entries).as_slice())
    }

    fn file_data<'a>(fs: &'a Rootfs, path: &str) -> &'a [u8] {
        match fs.get(path) {
            Some(Node::File(_, data) | Node::Hardlink { data, .. }) => data,
            other => panic!("{path}: expected a file, got {other:?}"),
        }
    }

    #[test]
    fn applies_entries_with_header_metadata() {
        let mut fs = Rootfs::new();
        apply(
            &mut fs,
            &[
                E::Dir("./"),
                E::Dir("./usr/"),
                E::File("./usr/su", b"x", 0o4755),
            ],
        )
        .unwrap();

        assert_eq!(fs.len(), 2);
        let meta = fs.get("usr/su").unwrap().meta();
        assert_eq!(meta.mode, 0o4755, "setuid bit must survive");
        assert_eq!((meta.uid, meta.gid), (0, 0));
    }

    #[test]
    fn creates_implicit_parent_directories() {
        let mut fs = Rootfs::new();
        apply(&mut fs, &[E::File("./a/b/c", b"x", 0o644)]).unwrap();
        assert_eq!(fs.get("a"), Some(&Node::Dir(Meta::implicit_dir())));
        assert_eq!(fs.get("a/b"), Some(&Node::Dir(Meta::implicit_dir())));
    }

    #[test]
    fn later_package_wins() {
        let mut fs = Rootfs::new();
        apply(&mut fs, &[E::File("./etc/motd", b"first", 0o644)]).unwrap();
        apply(&mut fs, &[E::File("./etc/motd", b"second", 0o644)]).unwrap();
        assert_eq!(file_data(&fs, "etc/motd"), b"second");
    }

    #[test]
    fn rejects_dotdot_in_archive_paths() {
        let mut fs = Rootfs::new();
        // The tar crate refuses to *write* `..` paths, so craft the header.
        let mut header = tar::Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..9].copy_from_slice(b"../escape");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&[0; 1024]);

        assert!(matches!(
            fs.apply_tar(bytes.as_slice()),
            Err(RootfsError::EscapingPath(_))
        ));
    }

    #[test]
    fn absolute_symlink_cannot_escape_the_root() {
        // A package ships `evil -> /etc` and then `evil/passwd`. On the host
        // this would write /etc/passwd; here it lands in the rootfs's etc.
        let mut fs = Rootfs::new();
        apply(
            &mut fs,
            &[
                E::Symlink("./evil", "/etc"),
                E::File("./evil/passwd", b"pwned", 0o644),
            ],
        )
        .unwrap();

        assert_eq!(file_data(&fs, "etc/passwd"), b"pwned");
        assert!(matches!(fs.get("evil"), Some(Node::Symlink(_, t)) if t == "/etc"));
    }

    #[test]
    fn relative_symlink_chain_cannot_escape_the_root() {
        // `s -> .` then `esc -> s/../host` escaped the old lexical checks.
        let mut fs = Rootfs::new();
        apply(
            &mut fs,
            &[
                E::Symlink("./s", "."),
                E::Symlink("./esc", "s/../../../host"),
                E::File("./esc/pwned", b"x", 0o644),
            ],
        )
        .unwrap();

        assert_eq!(file_data(&fs, "host/pwned"), b"x");
        assert!(fs.iter().all(|(path, _)| !path.starts_with("..")));
    }

    #[test]
    fn follows_directory_symlinks_like_dpkg() {
        // Merged /usr: `bin -> usr/bin`, then a package ships ./bin/sh.
        let mut fs = Rootfs::new();
        apply(
            &mut fs,
            &[E::Dir("./usr/bin/"), E::Symlink("./bin", "usr/bin")],
        )
        .unwrap();
        apply(
            &mut fs,
            &[E::Dir("./bin/"), E::File("./bin/sh", b"sh", 0o755)],
        )
        .unwrap();

        assert!(matches!(fs.get("bin"), Some(Node::Symlink(..))));
        assert_eq!(file_data(&fs, "usr/bin/sh"), b"sh");
    }

    #[test]
    fn detects_symlink_loops() {
        let mut fs = Rootfs::new();
        let result = apply(
            &mut fs,
            &[
                E::Symlink("./a", "b"),
                E::Symlink("./b", "a"),
                E::File("./a/x", b"x", 0o644),
            ],
        );
        assert!(matches!(result, Err(RootfsError::SymlinkLoop(_))));
    }

    #[test]
    fn hardlink_keeps_contents_when_target_is_replaced() {
        let mut fs = Rootfs::new();
        apply(
            &mut fs,
            &[
                E::File("./usr/bin/perl5.36", b"perl", 0o755),
                E::Hardlink("./usr/bin/perl", "./usr/bin/perl5.36"),
            ],
        )
        .unwrap();
        apply(&mut fs, &[E::File("./usr/bin/perl5.36", b"other", 0o755)]).unwrap();

        assert_eq!(file_data(&fs, "usr/bin/perl"), b"perl");
        assert_eq!(fs.get("usr/bin/perl").unwrap().meta().mode, 0o755);
    }

    #[test]
    fn hardlink_to_missing_target_is_an_error() {
        let mut fs = Rootfs::new();
        let result = apply(&mut fs, &[E::Hardlink("./a", "./missing")]);
        assert!(matches!(result, Err(RootfsError::BadHardlink { .. })));
    }

    #[test]
    fn file_cannot_replace_non_empty_directory() {
        let mut fs = Rootfs::new();
        apply(&mut fs, &[E::File("./d/x", b"x", 0o644)]).unwrap();
        let result = apply(&mut fs, &[E::File("./d", b"y", 0o644)]);
        assert!(matches!(result, Err(RootfsError::ReplacesDirectory(_))));
    }

    #[test]
    fn file_parent_must_be_a_directory() {
        let mut fs = Rootfs::new();
        apply(&mut fs, &[E::File("./f", b"x", 0o644)]).unwrap();
        let result = apply(&mut fs, &[E::File("./f/x", b"y", 0o644)]);
        assert!(matches!(result, Err(RootfsError::NotADirectory(_))));
    }

    #[test]
    fn applies_a_deb() {
        let data_tar = tar(&[E::File("./usr/share/doc/x", b"doc", 0o644)]);
        let mut fs = Rootfs::new();
        fs.apply_deb(&crate::deb::test_utils::deb(&data_tar))
            .unwrap();
        assert_eq!(file_data(&fs, "usr/share/doc/x"), b"doc");
    }
}
