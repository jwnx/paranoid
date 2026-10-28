//! How two lock files differ.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::hash::Sha256;

use super::{LockFile, LockedFile, LockedImage, LockedPackage};

/// How two lock files differ, image by image.
///
/// It serializes to the JSON that `paranoid diff --json` and `paranoid lock
/// --json` print, which paranoid keeps stable whatever happens to the lock's
/// own layout: fields may be added, but none removed or changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockDiff<'a> {
    /// Images only the newer lock has.
    pub added_images: Vec<&'a str>,
    /// Images only the older lock has.
    pub removed_images: Vec<&'a str>,
    /// Package changes in the images both locks have, sorted by name.
    pub changes: Vec<Change<'a>>,
    /// File changes in the images both locks have, sorted by name.
    pub files: Vec<FileChange<'a>>,
}

impl<'a> LockDiff<'a> {
    /// The diff from no lock at all to `lock`: every image is new.
    pub fn from_nothing(lock: &'a LockFile) -> Self {
        Self {
            added_images: lock.images.iter().map(|i| i.name.as_str()).collect(),
            ..Self::default()
        }
    }

    /// Whether nothing moved: no image, package or file changed. Two locks
    /// can still differ in the snapshots they record.
    pub fn is_empty(&self) -> bool {
        self.added_images.is_empty()
            && self.removed_images.is_empty()
            && self.changes.is_empty()
            && self.files.is_empty()
    }
}

/// How one package changed in some images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<'a> {
    pub name: &'a str,
    pub kind: ChangeKind<'a>,
    /// The repository the package comes from: the newer lock's, or the
    /// older one's for a removal.
    pub repository: &'a str,
    /// The images it changed in, in lock order.
    pub images: Vec<&'a str>,
}

/// How one file changed in some images: it's new, gone, or now comes
/// from another URL or has another hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange<'a> {
    pub name: &'a str,
    /// The file before; `None` if it's new.
    pub from: Option<&'a LockedFile>,
    /// The file after; `None` if it's gone.
    pub to: Option<&'a LockedFile>,
    /// The images it changed in, in lock order.
    pub images: Vec<&'a str>,
}

/// What happened to a package. Versions are set-wide, so a package changes
/// the same way in every image that keeps it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind<'a> {
    Changed { from: &'a str, to: &'a str },
    Added { version: &'a str },
    Removed { version: &'a str },
}

impl<'a> ChangeKind<'a> {
    /// The version before and after; `None` where there was none.
    pub fn versions(&self) -> (Option<&'a str>, Option<&'a str>) {
        match *self {
            Self::Changed { from, to } => (Some(from), Some(to)),
            Self::Added { version } => (None, Some(version)),
            Self::Removed { version } => (Some(version), None),
        }
    }
}

impl Serialize for LockDiff<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Json<'a> {
            changed: bool,
            images: Images<'a>,
            packages: Vec<Package<'a>>,
            files: Vec<File<'a>>,
        }
        #[derive(Serialize)]
        struct Images<'a> {
            added: &'a [&'a str],
            removed: &'a [&'a str],
        }
        #[derive(Serialize)]
        struct Package<'a> {
            name: &'a str,
            from: Option<&'a str>,
            to: Option<&'a str>,
            repository: &'a str,
            images: &'a [&'a str],
        }
        #[derive(Serialize)]
        struct File<'a> {
            name: &'a str,
            from: Option<Pin<'a>>,
            to: Option<Pin<'a>>,
            images: &'a [&'a str],
        }
        #[derive(Serialize)]
        struct Pin<'a> {
            url: &'a str,
            sha256: &'a Sha256,
        }
        fn pin(file: &LockedFile) -> Pin<'_> {
            Pin {
                url: file.url.as_str(),
                sha256: &file.sha256,
            }
        }

        Json {
            changed: !self.is_empty(),
            images: Images {
                added: &self.added_images,
                removed: &self.removed_images,
            },
            packages: self
                .changes
                .iter()
                .map(|c| {
                    let (from, to) = c.kind.versions();
                    Package {
                        name: c.name,
                        from,
                        to,
                        repository: c.repository,
                        images: &c.images,
                    }
                })
                .collect(),
            files: self
                .files
                .iter()
                .map(|f| File {
                    name: f.name,
                    from: f.from.map(pin),
                    to: f.to.map(pin),
                    images: &f.images,
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

impl LockFile {
    /// Changes going from `self` to `newer`, by image. A package that only
    /// moves into or out of an image, at the same version, is a change too.
    pub fn diff<'a>(&'a self, newer: &'a LockFile) -> LockDiff<'a> {
        let names = |lock: &'a LockFile| lock.images.iter().map(|i| i.name.as_str());
        let mut diff = LockDiff {
            added_images: names(newer).filter(|&i| self.image(i).is_none()).collect(),
            removed_images: names(self).filter(|&i| newer.image(i).is_none()).collect(),
            changes: Vec::new(),
            files: self.diff_files(newer),
        };

        // Each package's changes, merged across images, with the package
        // that has the repository.
        type Changes<'a> = BTreeMap<(&'a str, ChangeKind<'a>), (&'a LockedPackage, Vec<&'a str>)>;
        let mut changes: Changes = BTreeMap::new();
        for image in &newer.images {
            let Some(old_image) = self.image(&image.name) else {
                continue;
            };
            let packages = |lock: &'a LockFile,
                            image: &'a LockedImage|
             -> BTreeMap<&'a str, &'a LockedPackage> {
                lock.image_packages(image)
                    .into_iter()
                    .map(|p| (p.name.as_str(), p))
                    .collect()
            };
            let old = packages(self, old_image);
            let new = packages(newer, image);
            let added_or_changed = new.iter().filter_map(|(&name, &package)| {
                let version = package.version.as_str();
                let kind = match old.get(name) {
                    None => ChangeKind::Added { version },
                    Some(from) if from.version != version => ChangeKind::Changed {
                        from: &from.version,
                        to: version,
                    },
                    Some(_) => return None,
                };
                Some((name, kind, package))
            });
            let removed =
                old.iter()
                    .filter(|(name, _)| !new.contains_key(*name))
                    .map(|(&name, &package)| {
                        let version = &package.version;
                        (name, ChangeKind::Removed { version }, package)
                    });
            for (name, kind, package) in added_or_changed.chain(removed) {
                changes
                    .entry((name, kind))
                    .or_insert_with(|| (package, Vec::new()))
                    .1
                    .push(&image.name);
            }
        }
        diff.changes = changes
            .into_iter()
            .map(|((name, kind), (package, images))| Change {
                name,
                kind,
                repository: &package.repository,
                images,
            })
            .collect();
        diff
    }

    /// File changes going from `self` to `newer`, in the images both have.
    fn diff_files<'a>(&'a self, newer: &'a LockFile) -> Vec<FileChange<'a>> {
        // A file is the same if it has the same URL and hash.
        type Pin<'a> = Option<(&'a str, &'a Sha256)>;
        let pin =
            |file: Option<&'a LockedFile>| -> Pin<'a> { file.map(|f| (f.url.as_str(), &f.sha256)) };
        type Key<'a> = (&'a str, Pin<'a>, Pin<'a>);
        type Value<'a> = (Option<&'a LockedFile>, Option<&'a LockedFile>, Vec<&'a str>);
        let mut changes: BTreeMap<Key<'a>, Value<'a>> = BTreeMap::new();
        for image in &newer.images {
            let Some(old_image) = self.image(&image.name) else {
                continue;
            };
            let files = |lock: &'a LockFile, image: &'a LockedImage| {
                lock.image_files(image)
                    .into_iter()
                    .map(|f| (f.name.as_str(), f))
                    .collect::<BTreeMap<_, _>>()
            };
            let (old, new) = (files(self, old_image), files(newer, image));
            let names: std::collections::BTreeSet<&str> =
                old.keys().chain(new.keys()).copied().collect();
            for name in names {
                let (from, to) = (old.get(name).copied(), new.get(name).copied());
                if pin(from) == pin(to) {
                    continue;
                }
                changes
                    .entry((name, pin(from), pin(to)))
                    .or_insert_with(|| (from, to, Vec::new()))
                    .2
                    .push(&image.name);
            }
        }
        changes
            .into_iter()
            .map(|((name, ..), (from, to, images))| FileChange {
                name,
                from,
                to,
                images,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::Architecture;
    use crate::hash::Sha256;
    use crate::lock::{LockedFile, LockedPackage};

    /// A lock with these images, each a list of (name, version).
    fn set_lock(images: &[(&str, &[(&str, &str)])]) -> LockFile {
        let mut packages: Vec<LockedPackage> = images
            .iter()
            .flat_map(|(_, pkgs)| pkgs.iter())
            .map(|(name, version)| LockedPackage {
                name: name.to_string(),
                version: version.to_string(),
                architecture: Architecture::Amd64,
                sha256: Sha256::of(name.as_bytes()),
                filename: String::new(),
                size: 0,
                repository: "debian".to_string(),
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        packages.dedup_by(|a, b| a.name == b.name);
        LockFile {
            config_hash: String::new(),
            repositories: vec![],
            packages,
            files: Vec::new(),
            images: images
                .iter()
                .map(|(name, pkgs)| LockedImage {
                    name: name.to_string(),
                    packages: pkgs.iter().map(|(n, _)| n.to_string()).collect(),
                    direct: Vec::new(),
                    files: Vec::new(),
                    unmet: Vec::new(),
                    deps: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn diff_reports_added_changed_and_removed() {
        let old = set_lock(&[("default", &[("bash", "5.1"), ("curl", "7"), ("zlib", "1")])]);
        let new = set_lock(&[(
            "default",
            &[("bash", "5.2"), ("curl", "7"), ("libc6", "2.36")],
        )]);

        let change = |name, kind| Change {
            name,
            kind,
            repository: "debian",
            images: vec!["default"],
        };
        assert_eq!(
            old.diff(&new).changes,
            [
                change(
                    "bash",
                    ChangeKind::Changed {
                        from: "5.1",
                        to: "5.2"
                    }
                ),
                change("libc6", ChangeKind::Added { version: "2.36" }),
                change("zlib", ChangeKind::Removed { version: "1" }),
            ]
        );
        assert!(new.diff(&new).is_empty());
    }

    #[test]
    fn diff_names_the_images_a_change_is_in() {
        let old = set_lock(&[
            ("server", &[("libc6", "2.36"), ("libpq5", "15")]),
            ("builder", &[("libc6", "2.36"), ("libpq5", "15")]),
            ("old", &[("libc6", "2.36")]),
        ]);
        let new = set_lock(&[
            ("server", &[("libc6", "2.36"), ("libpq5", "16")]),
            ("builder", &[("gcc", "12"), ("libc6", "2.36")]),
            ("tools", &[("libc6", "2.36")]),
        ]);

        let diff = old.diff(&new);
        assert_eq!(diff.added_images, ["tools"]);
        assert_eq!(diff.removed_images, ["old"]);
        assert_eq!(
            diff.changes,
            [
                Change {
                    name: "gcc",
                    kind: ChangeKind::Added { version: "12" },
                    repository: "debian",
                    images: vec!["builder"],
                },
                Change {
                    name: "libpq5",
                    kind: ChangeKind::Changed {
                        from: "15",
                        to: "16"
                    },
                    repository: "debian",
                    images: vec!["server"],
                },
                Change {
                    name: "libpq5",
                    kind: ChangeKind::Removed { version: "15" },
                    repository: "debian",
                    images: vec!["builder"],
                },
            ]
        );
    }

    #[test]
    fn diff_shows_a_package_moving_between_images() {
        let old = set_lock(&[("a", &[("curl", "7")]), ("b", &[])]);
        let new = set_lock(&[("a", &[]), ("b", &[("curl", "7")])]);
        let diff = old.diff(&new);
        let kinds: Vec<_> = diff.changes.iter().map(|c| (&c.kind, &c.images)).collect();
        assert_eq!(
            kinds,
            [
                (&ChangeKind::Added { version: "7" }, &vec!["b"]),
                (&ChangeKind::Removed { version: "7" }, &vec!["a"]),
            ]
        );
    }

    #[test]
    fn diff_names_the_repository_of_each_change() {
        let old = set_lock(&[("default", &[("libssl3", "3.0.15"), ("zlib", "1")])]);
        let mut new = set_lock(&[("default", &[("libssl3", "3.0.16")])]);
        new.packages[0].repository = "debian-security".to_string();

        let repositories: Vec<_> = old
            .diff(&new)
            .changes
            .iter()
            .map(|c| (c.name, c.repository))
            .collect();
        assert_eq!(
            repositories,
            [("libssl3", "debian-security"), ("zlib", "debian")]
        );
    }

    #[test]
    fn diff_serializes_to_the_documented_json() {
        let old = set_lock(&[("server", &[("bash", "5.1"), ("zlib", "1")]), ("old", &[])]);
        let new = set_lock(&[
            ("server", &[("bash", "5.2"), ("curl", "7")]),
            ("tools", &[]),
        ]);

        assert_eq!(
            serde_json::to_value(old.diff(&new)).unwrap(),
            serde_json::json!({
                "changed": true,
                "images": {"added": ["tools"], "removed": ["old"]},
                "packages": [
                    {"name": "bash", "from": "5.1", "to": "5.2",
                     "repository": "debian", "images": ["server"]},
                    {"name": "curl", "from": null, "to": "7",
                     "repository": "debian", "images": ["server"]},
                    {"name": "zlib", "from": "1", "to": null,
                     "repository": "debian", "images": ["server"]},
                ],
                "files": [],
            })
        );
        assert_eq!(
            serde_json::to_value(new.diff(&new)).unwrap(),
            serde_json::json!({
                "changed": false,
                "images": {"added": [], "removed": []},
                "packages": [],
                "files": [],
            })
        );
    }

    #[test]
    fn a_first_lock_adds_every_image() {
        let lock = set_lock(&[("a", &[("curl", "7")]), ("b", &[])]);
        let diff = LockDiff::from_nothing(&lock);
        assert_eq!(diff.added_images, ["a", "b"]);
        assert!(diff.changes.is_empty());
        assert!(!diff.is_empty());
    }

    fn with_files(mut lock: LockFile, files: &[(&str, &str, &[u8])]) -> LockFile {
        lock.files = files
            .iter()
            .map(|&(name, url, data)| LockedFile {
                name: name.to_string(),
                url: url.parse().unwrap(),
                sha256: Sha256::of(data),
                size: data.len() as u64,
                trust: crate::lock::Trust::Pinned,
            })
            .collect();
        for image in &mut lock.images {
            image.files = files.iter().map(|(name, ..)| name.to_string()).collect();
        }
        lock
    }

    #[test]
    fn diff_reports_files_by_url_and_hash() {
        let base = || set_lock(&[("default", &[("bash", "5.2")])]);
        let old = with_files(
            base(),
            &[
                ("etcd", "https://example.com/etcd-3.6.15.tar.gz", b"3.6.15"),
                ("tini", "https://example.com/tini", b"tini"),
                ("gone", "https://example.com/gone", b"gone"),
            ],
        );
        let new = with_files(
            base(),
            &[
                ("etcd", "https://example.com/etcd-3.6.16.tar.gz", b"3.6.16"),
                ("tini", "https://example.com/tini", b"tini"),
                ("new", "https://example.com/new", b"new"),
            ],
        );

        let diff = old.diff(&new);
        let summary: Vec<_> = diff
            .files
            .iter()
            .map(|f| (f.name, f.from.is_some(), f.to.is_some()))
            .collect();
        assert_eq!(
            summary,
            [
                ("etcd", true, true),
                ("gone", true, false),
                ("new", false, true)
            ]
        );
        assert!(!diff.is_empty());
        assert!(diff.changes.is_empty());

        let json = serde_json::to_value(&diff).unwrap();
        assert_eq!(
            json["files"][0],
            serde_json::json!({
                "name": "etcd",
                "from": {
                    "url": "https://example.com/etcd-3.6.15.tar.gz",
                    "sha256": Sha256::of(b"3.6.15").to_string(),
                },
                "to": {
                    "url": "https://example.com/etcd-3.6.16.tar.gz",
                    "sha256": Sha256::of(b"3.6.16").to_string(),
                },
                "images": ["default"],
            })
        );
        assert_eq!(json["files"][1]["to"], serde_json::Value::Null);
        assert_eq!(json["changed"], true);

        let repinned = with_files(base(), &[("tini", "https://example.com/tini", b"other")]);
        let only_tini = with_files(base(), &[("tini", "https://example.com/tini", b"tini")]);
        assert_eq!(only_tini.diff(&repinned).files.len(), 1);
        assert!(new.diff(&new).is_empty());
    }
}
