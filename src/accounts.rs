//! Writing `/etc/passwd` and `/etc/group` with the manifest's accounts.
//!
//! base-passwd's `postinst` copies its master files to `/etc/passwd` and
//! `/etc/group`, but only when they aren't there yet, and checks them with
//! `update-passwd` only on upgrades. So we write base-passwd's entries and
//! the manifest's together: in an unconfigured image, base-passwd's
//! `postinst` keeps them, and so does `adduser --system` in the `postinst`
//! of a package that would otherwise pick the next free ID for its user.
//! In a configured image nothing else writes them at all.

use thiserror::Error;

use crate::config::Accounts;
use crate::rootfs::{Node, Rootfs, RootfsError};

const PASSWD_MASTER: &str = "usr/share/base-passwd/passwd.master";
const GROUP_MASTER: &str = "usr/share/base-passwd/group.master";

/// Errors writing the accounts.
#[derive(Debug, Error)]
pub enum AccountsError {
    #[error(
        "declaring accounts needs base-passwd in the image, for root and the other system \
         accounts; add it to the packages"
    )]
    NoBasePasswd,

    #[error("{0} in base-passwd isn't UTF-8 text")]
    Master(&'static str),

    #[error("{kind} '{name}' is base-passwd's already")]
    NameTaken { kind: &'static str, name: String },

    #[error("{kind} '{name}' can't have ID {id}: base-passwd gives it to '{owner}'")]
    IdTaken {
        kind: &'static str,
        name: String,
        id: u32,
        owner: String,
    },

    #[error(
        "user '{user}' is in group '{group}', which neither [accounts.groups] nor base-passwd has"
    )]
    UnknownGroup { user: String, group: String },

    #[error("could not write the accounts")]
    Rootfs(#[from] RootfsError),

    #[error("configuring the image changed {0}, which the manifest declares")]
    Changed(String),
}

/// One line of `/etc/group`.
struct Group {
    name: String,
    gid: u32,
    members: Vec<String>,
}

/// Write `/etc/passwd` and `/etc/group`: base-passwd's entries, then the
/// manifest's, in manifest order.
pub fn write(rootfs: &mut Rootfs, accounts: &Accounts) -> Result<(), AccountsError> {
    let passwd = master(rootfs, PASSWD_MASTER)?;
    let group = master(rootfs, GROUP_MASTER)?;

    // base-passwd's names and IDs, which ours can't take.
    let users: Vec<(&str, u32)> = entries(&passwd).collect();
    let mut groups: Vec<Group> = entries(&group)
        .map(|(name, gid)| Group {
            name: name.to_string(),
            gid,
            members: Vec::new(),
        })
        .collect();

    let add_group = |groups: &mut Vec<Group>, name: &str, gid: u32| {
        if let Some(taken) = groups.iter().find(|g| g.name == name) {
            return Err(AccountsError::NameTaken {
                kind: "group",
                name: taken.name.clone(),
            });
        }
        if let Some(owner) = groups.iter().find(|g| g.gid == gid) {
            return Err(AccountsError::IdTaken {
                kind: "group",
                name: name.to_string(),
                id: gid,
                owner: owner.name.clone(),
            });
        }
        groups.push(Group {
            name: name.to_string(),
            gid,
            members: Vec::new(),
        });
        Ok(())
    };
    for (name, &gid) in &accounts.groups {
        add_group(&mut groups, name, gid)?;
    }
    // A user's own group, unless it's declared or base-passwd's, gets the
    // user's UID as its GID, as `adduser --system --group` would pick.
    for (name, user) in &accounts.users {
        if user.group(name) == name && !groups.iter().any(|g| g.name == *name) {
            add_group(&mut groups, name, user.uid)?;
        }
    }

    let mut passwd = passwd.trim_end().to_string();
    passwd.push('\n');
    for (name, user) in &accounts.users {
        if users.iter().any(|(taken, _)| taken == name) {
            return Err(AccountsError::NameTaken {
                kind: "user",
                name: name.clone(),
            });
        }
        if let Some((owner, _)) = users.iter().find(|(_, uid)| *uid == user.uid) {
            return Err(AccountsError::IdTaken {
                kind: "user",
                name: name.clone(),
                id: user.uid,
                owner: owner.to_string(),
            });
        }
        let gid = gid_of(&groups, name, user.group(name))?;
        // `*` as base-passwd writes its own: no password, and `pwconv`
        // moves it to /etc/shadow when the passwd package is configured.
        passwd.push_str(&format!(
            "{name}:*:{}:{gid}:{}:{}:{}\n",
            user.uid,
            user.gecos.as_deref().unwrap_or(""),
            user.home(),
            user.shell(),
        ));
        for member_of in &user.groups {
            gid_of(&groups, name, member_of)?;
            let group = groups
                .iter_mut()
                .find(|g| g.name == *member_of)
                .expect("just found it");
            group.members.push(name.clone());
        }
    }

    // `*` as base-passwd writes its own; `grpconv` moves it to
    // /etc/gshadow when the passwd package is configured.
    let mut group_file = String::new();
    for group in &groups {
        group_file.push_str(&format!(
            "{}:*:{}:{}\n",
            group.name,
            group.gid,
            group.members.join(",")
        ));
    }

    let mut tar = tar::Builder::new(Vec::new());
    for (path, data) in [("etc/passwd", passwd), ("etc/group", group_file)] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(data.len() as u64);
        tar.append_data(&mut header, path, data.as_bytes())
            .map_err(RootfsError::from)?;
    }
    let tar = tar.into_inner().map_err(RootfsError::from)?;
    // Not over anything a package shipped: no package ships these.
    rootfs.add_tar(tar.as_slice(), "etc")?;
    Ok(())
}

/// After configuring: make sure every declared account is still what the
/// manifest says. `adduser --system` keeps a user that exists, so this
/// only fails when a script does something unusual, which is what this is
/// for.
pub fn verify(rootfs: &Rootfs, accounts: &Accounts) -> Result<(), AccountsError> {
    let text = |path: &str| match rootfs.get(path) {
        Some(Node::File(_, data)) => String::from_utf8_lossy(data).into_owned(),
        _ => String::new(),
    };
    let passwd = text("etc/passwd");
    let group = text("etc/group");
    let users: Vec<Vec<&str>> = passwd.lines().map(|l| l.split(':').collect()).collect();
    let groups: Vec<Vec<&str>> = group.lines().map(|l| l.split(':').collect()).collect();
    let gid_of = |name: &str| {
        groups
            .iter()
            .find(|g| g.first() == Some(&name))
            .and_then(|g| g.get(2)?.parse::<u32>().ok())
    };

    for (name, &gid) in &accounts.groups {
        if gid_of(name) != Some(gid) {
            return Err(AccountsError::Changed(format!(
                "group '{name}' (GID {gid})"
            )));
        }
    }
    for (name, user) in &accounts.users {
        let entry = users.iter().find(|u| u.first() == Some(&name.as_str()));
        let ids = entry.and_then(|u| {
            Some((
                u.get(2)?.parse::<u32>().ok()?,
                u.get(3)?.parse::<u32>().ok()?,
            ))
        });
        let primary = gid_of(user.group(name));
        if ids.is_none()
            || ids.map(|(uid, _)| uid) != Some(user.uid)
            || ids.map(|(_, gid)| gid) != primary
        {
            return Err(AccountsError::Changed(format!(
                "user '{name}' (UID {})",
                user.uid
            )));
        }
        for member_of in &user.groups {
            let member = groups.iter().any(|g| {
                g.first() == Some(&member_of.as_str())
                    && g.get(3).is_some_and(|m| m.split(',').any(|m| m == name))
            });
            if !member {
                return Err(AccountsError::Changed(format!(
                    "user '{name}' being in group '{member_of}'"
                )));
            }
        }
    }
    Ok(())
}

/// The GID of `group`, which `user` is in.
fn gid_of(groups: &[Group], user: &str, group: &str) -> Result<u32, AccountsError> {
    groups
        .iter()
        .find(|g| g.name == group)
        .map(|g| g.gid)
        .ok_or_else(|| AccountsError::UnknownGroup {
            user: user.to_string(),
            group: group.to_string(),
        })
}

/// A base-passwd master file's text.
fn master(rootfs: &Rootfs, path: &'static str) -> Result<String, AccountsError> {
    let data = match rootfs.get(path) {
        Some(Node::File(_, data) | Node::Hardlink { data, .. }) => data,
        _ => return Err(AccountsError::NoBasePasswd),
    };
    String::from_utf8(data.to_vec()).map_err(|_| AccountsError::Master(path))
}

/// Each line's name and ID, from `/etc/passwd` or `/etc/group` text.
fn entries(text: &str) -> impl Iterator<Item = (&str, u32)> {
    text.lines().filter_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let id = fields.nth(1)?.parse().ok()?;
        Some((name, id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:*:0:0:root:/root:/bin/bash\n\
                          _apt:*:42:65534::/nonexistent:/usr/sbin/nologin\n\
                          nobody:*:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n";
    const GROUP: &str = "root:*:0:\nstaff:*:50:\nusers:*:100:\nnogroup:*:65534:\n";

    fn with_base_passwd() -> Rootfs {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, data) in [(PASSWD_MASTER, PASSWD), (GROUP_MASTER, GROUP)] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_size(data.len() as u64);
            tar.append_data(&mut header, path, data.as_bytes()).unwrap();
        }
        let mut rootfs = Rootfs::new();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        rootfs
    }

    fn accounts(toml: &str) -> Accounts {
        toml::from_str(toml).unwrap()
    }

    fn text(rootfs: &Rootfs, path: &str) -> String {
        match rootfs.get(path) {
            Some(Node::File(meta, data)) => {
                assert_eq!((meta.mode, meta.uid, meta.gid), (0o644, 0, 0));
                String::from_utf8(data.to_vec()).unwrap()
            }
            other => panic!("{path}: {other:?}"),
        }
    }

    #[test]
    fn writes_base_passwd_and_the_manifests_accounts() {
        let mut rootfs = with_base_passwd();
        let accounts = accounts(
            r#"
groups = { ssl-cert = 101 }

[users.postgres]
uid    = 999
groups = ["ssl-cert", "staff"]
home   = "/var/lib/postgresql"
shell  = "/bin/bash"
gecos  = "PostgreSQL administrator"

[users.pgbackrest]
uid   = 100
group = "pgbackrest"

[users.backup-agent]
uid   = 998
group = "nogroup"
"#,
        );
        // pgbackrest's own group isn't declared, so it gets its UID: 100,
        // which is base-passwd's `users`.
        let err = write(&mut rootfs.clone(), &accounts).unwrap_err();
        assert_eq!(
            err.to_string(),
            "group 'pgbackrest' can't have ID 100: base-passwd gives it to 'users'"
        );

        let mut accounts = accounts;
        accounts.groups.insert("pgbackrest".to_string(), 102);
        write(&mut rootfs, &accounts).unwrap();
        assert_eq!(
            text(&rootfs, "etc/passwd"),
            format!(
                "{PASSWD}\
                 postgres:*:999:999:PostgreSQL administrator:/var/lib/postgresql:/bin/bash\n\
                 pgbackrest:*:100:102::/nonexistent:/usr/sbin/nologin\n\
                 backup-agent:*:998:65534::/nonexistent:/usr/sbin/nologin\n"
            )
        );
        assert_eq!(
            text(&rootfs, "etc/group"),
            "root:*:0:\nstaff:*:50:postgres\nusers:*:100:\nnogroup:*:65534:\n\
             ssl-cert:*:101:postgres\npgbackrest:*:102:\npostgres:*:999:\n"
        );
    }

    #[test]
    fn refuses_base_passwds_names_and_ids() {
        for (toml, wanted) in [
            (
                "users.root = { uid = 500 }",
                "user 'root' is base-passwd's already",
            ),
            (
                "users.app = { uid = 42, group = \"staff\" }",
                "user 'app' can't have ID 42: base-passwd gives it to '_apt'",
            ),
            (
                "groups = { staff = 500 }",
                "group 'staff' is base-passwd's already",
            ),
            ("groups = { app = 50 }", "group 'app' can't have ID 50"),
            (
                "users.app = { uid = 500, groups = [\"wheel\"] }",
                "in group 'wheel', which neither",
            ),
            (
                "users.app = { uid = 500, group = \"wheel\" }",
                "in group 'wheel', which neither",
            ),
        ] {
            let err = write(&mut with_base_passwd(), &accounts(toml)).unwrap_err();
            assert!(err.to_string().contains(wanted), "{toml}: {err}");
        }
    }

    #[test]
    fn needs_base_passwd() {
        let err = write(&mut Rootfs::new(), &accounts("users.app = { uid = 500 }")).unwrap_err();
        assert!(matches!(err, AccountsError::NoBasePasswd), "{err}");
    }

    #[test]
    fn verifies_the_accounts_after_configuring() {
        let declared = accounts(
            "groups = { ssl-cert = 101 }\n\
             [users.postgres]\nuid = 999\ngroups = [\"ssl-cert\"]\n",
        );
        let mut rootfs = with_base_passwd();
        write(&mut rootfs, &declared).unwrap();
        verify(&rootfs, &declared).unwrap();

        // A script moves postgres to the next free ID.
        let Some(Node::File(meta, data)) = rootfs.get("etc/passwd").cloned() else {
            panic!();
        };
        let moved = String::from_utf8_lossy(&data).replace(":999:999:", ":100:999:");
        rootfs
            .replace_node("etc/passwd", Node::File(meta, moved.into_bytes().into()))
            .unwrap();
        let err = verify(&rootfs, &declared).unwrap_err();
        assert_eq!(
            err.to_string(),
            "configuring the image changed user 'postgres' (UID 999), which the manifest declares"
        );
    }
}
