//! How two lock files differ.

use std::collections::BTreeMap;

use super::{LockFile, LockedImage};

/// How two lock files differ, image by image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockDiff<'a> {
    /// Images only the newer lock has.
    pub added_images: Vec<&'a str>,
    /// Images only the older lock has.
    pub removed_images: Vec<&'a str>,
    /// Package changes in the images both locks have, sorted by name.
    pub changes: Vec<Change<'a>>,
}

impl LockDiff<'_> {
    pub fn is_empty(&self) -> bool {
        self.added_images.is_empty() && self.removed_images.is_empty() && self.changes.is_empty()
    }
}

/// How one package changed in some images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<'a> {
    pub name: &'a str,
    pub kind: ChangeKind<'a>,
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

impl LockFile {
    /// Changes going from `self` to `newer`, by image. A package that only
    /// moves into or out of an image, at the same version, is a change too.
    pub fn diff<'a>(&'a self, newer: &'a LockFile) -> LockDiff<'a> {
        let names = |lock: &'a LockFile| lock.images.iter().map(|i| i.name.as_str());
        let mut diff = LockDiff {
            added_images: names(newer).filter(|&i| self.image(i).is_none()).collect(),
            removed_images: names(self).filter(|&i| newer.image(i).is_none()).collect(),
            changes: Vec::new(),
        };

        // Each package's changes, merged across images.
        let mut changes: BTreeMap<(&str, ChangeKind), Vec<&str>> = BTreeMap::new();
        for image in &newer.images {
            let Some(old_image) = self.image(&image.name) else {
                continue;
            };
            let versions =
                |lock: &'a LockFile, image: &'a LockedImage| -> BTreeMap<&'a str, &'a str> {
                    lock.image_packages(image)
                        .into_iter()
                        .map(|p| (p.name.as_str(), p.version.as_str()))
                        .collect()
                };
            let old = versions(self, old_image);
            let new = versions(newer, image);
            let added_or_changed = new.iter().filter_map(|(&name, &version)| {
                let kind = match old.get(name) {
                    None => ChangeKind::Added { version },
                    Some(&from) if from != version => ChangeKind::Changed { from, to: version },
                    Some(_) => return None,
                };
                Some((name, kind))
            });
            let removed = old
                .iter()
                .filter(|(name, _)| !new.contains_key(*name))
                .map(|(&name, &version)| (name, ChangeKind::Removed { version }));
            for key in added_or_changed.chain(removed) {
                changes.entry(key).or_default().push(&image.name);
            }
        }
        diff.changes = changes
            .into_iter()
            .map(|((name, kind), images)| Change { name, kind, images })
            .collect();
        diff
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::Architecture;
    use crate::hash::Sha256;
    use crate::lock::LockedPackage;

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
            images: images
                .iter()
                .map(|(name, pkgs)| LockedImage {
                    name: name.to_string(),
                    packages: pkgs.iter().map(|(n, _)| n.to_string()).collect(),
                    direct: Vec::new(),
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
                    images: vec!["builder"],
                },
                Change {
                    name: "libpq5",
                    kind: ChangeKind::Changed {
                        from: "15",
                        to: "16"
                    },
                    images: vec!["server"],
                },
                Change {
                    name: "libpq5",
                    kind: ChangeKind::Removed { version: "15" },
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
}
