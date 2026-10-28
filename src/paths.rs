//! Writing the manifest's `[paths]`: files, directories and symlinks with
//! an owner and a mode.
//!
//! They go in last, after the packages, the accounts and the files, so a
//! path can name a user the accounts declare and a directory a package or
//! a file made. A directory gets the owner and mode the manifest says,
//! whether it was there or not; a file or a symlink can't replace
//! anything.

use std::collections::HashMap;

use indexmap::IndexMap;
use thiserror::Error;

use crate::config::{ImagePath, Owner, PathKind, PathSpec};
use crate::rootfs::{Meta, Node, Rootfs, RootfsError};

/// Errors writing a path.
#[derive(Debug, Error)]
pub enum PathsError {
    #[error("{path}: there's no {kind} '{name}' in the image")]
    Unknown {
        path: ImagePath,
        kind: &'static str,
        name: String,
    },

    #[error("{path}: owner {owner} isn't a user in the image, so name its `group`")]
    NoGroup { path: ImagePath, owner: u32 },

    #[error("{path}: {source}")]
    Rootfs {
        path: ImagePath,
        #[source]
        source: RootfsError,
    },
}

/// Users and groups by name, as the image has them.
#[derive(Debug, Default)]
struct Ids {
    /// Name → (UID, primary GID).
    users: HashMap<String, (u32, u32)>,
    groups: HashMap<String, u32>,
}

impl Ids {
    /// The image's `/etc/passwd` and `/etc/group`, or base-passwd's master
    /// files if the image doesn't have those (yet).
    fn of(rootfs: &Rootfs) -> Self {
        let text = |paths: [&str; 2]| {
            paths.iter().find_map(|path| match rootfs.get(path) {
                Some(Node::File(_, data) | Node::Hardlink { data, .. }) => {
                    Some(String::from_utf8_lossy(data).into_owned())
                }
                _ => None,
            })
        };
        let passwd = text(["etc/passwd", "usr/share/base-passwd/passwd.master"]);
        let group = text(["etc/group", "usr/share/base-passwd/group.master"]);

        let mut ids = Self::default();
        for line in passwd.as_deref().unwrap_or("").lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if let [name, _, uid, gid, ..] = fields[..]
                && let (Ok(uid), Ok(gid)) = (uid.parse(), gid.parse())
            {
                ids.users.insert(name.to_string(), (uid, gid));
            }
        }
        for line in group.as_deref().unwrap_or("").lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if let [name, _, gid, ..] = fields[..]
                && let Ok(gid) = gid.parse()
            {
                ids.groups.insert(name.to_string(), gid);
            }
        }
        ids
    }

    /// The UID and GID `spec` asks for.
    fn owner(&self, path: &ImagePath, spec: &PathSpec) -> Result<(u32, u32), PathsError> {
        let unknown = |kind, name: &str| PathsError::Unknown {
            path: path.clone(),
            kind,
            name: name.to_string(),
        };
        let user = match &spec.owner {
            None => None,
            Some(Owner::Id(uid)) => Some((
                *uid,
                self.users.values().find(|(u, _)| u == uid).map(|&(_, g)| g),
            )),
            Some(Owner::Name(name)) => {
                let &(uid, gid) = self.users.get(name).ok_or_else(|| unknown("user", name))?;
                Some((uid, Some(gid)))
            }
        };
        let uid = user.map_or(0, |(uid, _)| uid);
        let gid = match (&spec.group, user) {
            (Some(Owner::Id(gid)), _) => *gid,
            (Some(Owner::Name(name)), _) => *self
                .groups
                .get(name)
                .ok_or_else(|| unknown("group", name))?,
            (None, None) => 0,
            (None, Some((_, Some(gid)))) => gid,
            (None, Some((uid, None))) => {
                return Err(PathsError::NoGroup {
                    path: path.clone(),
                    owner: uid,
                });
            }
        };
        Ok((uid, gid))
    }
}

/// Write `paths` into the rootfs, in order.
pub fn apply(rootfs: &mut Rootfs, paths: &IndexMap<ImagePath, PathSpec>) -> Result<(), PathsError> {
    let ids = Ids::of(rootfs);
    for (path, spec) in paths {
        let (uid, gid) = ids.owner(path, spec)?;
        let mode = spec.mode();
        let in_path = |source| PathsError::Rootfs {
            path: path.clone(),
            source,
        };
        let relative = path.relative();
        match spec.kind() {
            PathKind::Directory => {
                rootfs
                    .set_dir(
                        relative,
                        Meta {
                            mode,
                            uid: uid.into(),
                            gid: gid.into(),
                        },
                    )
                    .map_err(in_path)?;
            }
            kind => {
                let mut header = tar::Header::new_gnu();
                header.set_mode(mode);
                header.set_uid(uid.into());
                header.set_gid(gid.into());
                header.set_mtime(0);
                let mut tar = tar::Builder::new(Vec::new());
                let written = match kind {
                    PathKind::File(content) => {
                        header.set_entry_type(tar::EntryType::Regular);
                        header.set_size(content.len() as u64);
                        tar.append_data(&mut header, relative, content.as_bytes())
                    }
                    PathKind::Symlink(target) => {
                        header.set_entry_type(tar::EntryType::Symlink);
                        header.set_size(0);
                        tar.append_link(&mut header, relative, target)
                    }
                    PathKind::Directory => unreachable!(),
                };
                let tar = written
                    .and_then(|()| tar.into_inner())
                    .map_err(|e| in_path(e.into()))?;
                rootfs.add_tar(tar.as_slice(), relative).map_err(in_path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rootfs with accounts, a package's directory and a merged `/bin`.
    fn rootfs() -> Rootfs {
        let mut tar = tar::Builder::new(Vec::new());
        let mut add = |path: &str, kind: tar::EntryType, mode: u32, data: &str| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_mode(mode);
            header.set_uid(0);
            header.set_gid(0);
            if kind.is_symlink() {
                header.set_size(0);
                tar.append_link(&mut header, path, data).unwrap();
            } else {
                header.set_size(data.len() as u64);
                tar.append_data(&mut header, path, data.as_bytes()).unwrap();
            }
        };
        add(
            "etc/passwd",
            tar::EntryType::Regular,
            0o644,
            "root:*:0:0:root:/root:/bin/bash\npostgres:*:999:998::/var/lib/postgresql:/bin/bash\n",
        );
        add(
            "etc/group",
            tar::EntryType::Regular,
            0o644,
            "root:*:0:\nssl-cert:*:101:\npostgres:*:998:\n",
        );
        add("var/lib/postgresql/", tar::EntryType::Directory, 0o755, "");
        add("usr/bin/", tar::EntryType::Directory, 0o755, "");
        add("usr/bin/bash", tar::EntryType::Regular, 0o755, "bash");
        add("bin", tar::EntryType::Symlink, 0o777, "usr/bin");
        let mut rootfs = Rootfs::new();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        rootfs
    }

    fn paths(toml: &str) -> IndexMap<ImagePath, PathSpec> {
        let raw: IndexMap<String, PathSpec> = toml::from_str(toml).unwrap();
        raw.into_iter()
            .map(|(path, spec)| (path.parse().unwrap(), spec))
            .collect()
    }

    fn meta(rootfs: &Rootfs, path: &str) -> (u32, u64, u64) {
        let meta = rootfs
            .get(path)
            .unwrap_or_else(|| panic!("no {path}"))
            .meta();
        (meta.mode, meta.uid, meta.gid)
    }

    #[test]
    fn writes_files_directories_and_symlinks() {
        let mut rootfs = rootfs();
        apply(
            &mut rootfs,
            &paths(
                r##"
"/usr/sbin/policy-rc.d" = { content = "#!/bin/sh\nexit 101\n", mode = "0755" }
"/etc/postgresql-common/createcluster.d/mpg.conf" = { content = "create_main_cluster = false\n" }
"/var/lib/postgresql" = { owner = "postgres", mode = "0700" }
"/run/postgresql" = { owner = "postgres", group = "ssl-cert", mode = "2775" }
"/srv/data" = { owner = 1234, group = 1234 }
"/usr/bin/sh" = { target = "bash" }
"/bin/psql-wrapper" = { content = "#!/bin/sh\n", owner = "postgres" }
"##,
            ),
        )
        .unwrap();

        assert_eq!(meta(&rootfs, "usr/sbin/policy-rc.d"), (0o755, 0, 0));
        match rootfs.get("usr/sbin/policy-rc.d") {
            Some(Node::File(_, data)) => assert_eq!(&data[..], b"#!/bin/sh\nexit 101\n"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            meta(&rootfs, "etc/postgresql-common/createcluster.d/mpg.conf"),
            (0o644, 0, 0)
        );
        // A package's directory takes the owner and mode; the owner's
        // primary group comes with it.
        assert_eq!(meta(&rootfs, "var/lib/postgresql"), (0o700, 999, 998));
        assert_eq!(meta(&rootfs, "run/postgresql"), (0o2775, 999, 101));
        assert_eq!(meta(&rootfs, "srv/data"), (0o755, 1234, 1234));
        assert!(matches!(
            rootfs.get("usr/bin/sh"),
            Some(Node::Symlink(_, target)) if target == "bash"
        ));
        // Through the image's own links, as a package's paths go.
        assert_eq!(meta(&rootfs, "usr/bin/psql-wrapper"), (0o644, 999, 998));
    }

    #[test]
    fn refuses_what_it_cant_do() {
        for (toml, wanted) in [
            (
                r#""/usr/bin/bash" = { content = "x" }"#,
                "'/usr/bin/bash' is already in the image",
            ),
            (
                r#""/usr/bin/bash" = { owner = "postgres" }"#,
                "'usr/bin/bash' is not a directory",
            ),
            (r#""/x" = { owner = "mysql" }"#, "there's no user 'mysql'"),
            (r#""/x" = { group = "wheel" }"#, "there's no group 'wheel'"),
            (
                r#""/x" = { owner = 1234 }"#,
                "owner 1234 isn't a user in the image",
            ),
        ] {
            let err = apply(&mut rootfs(), &paths(toml)).unwrap_err();
            assert!(err.to_string().contains(wanted), "{toml}: {err}");
        }
    }

    #[test]
    fn names_come_from_base_passwd_without_accounts() {
        let mut tar = tar::Builder::new(Vec::new());
        let data = "root:*:0:0:root:/root:/bin/bash\nwww-data:*:33:33:www-data:/var/www:/usr/sbin/nologin\n";
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(data.len() as u64);
        tar.append_data(
            &mut header,
            "usr/share/base-passwd/passwd.master",
            data.as_bytes(),
        )
        .unwrap();
        let mut rootfs = Rootfs::new();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        apply(
            &mut rootfs,
            &paths(r#""/var/www" = { owner = "www-data" }"#),
        )
        .unwrap();
        assert_eq!(meta(&rootfs, "var/www"), (0o755, 33, 33));
    }
}
