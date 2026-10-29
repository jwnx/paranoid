//! Users and groups with fixed IDs: `[accounts]`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Users and groups an image gets, on top of base-passwd's.
///
/// Like [`crate::config::ImageSpec`], it serializes to what a build's
/// inputs hash, leaving out what's unset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounts {
    /// Groups by name, with their GIDs.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub groups: IndexMap<String, u32>,
    /// Users by name.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub users: IndexMap<String, UserSpec>,
}

/// One user, as `adduser --system` would make it, but with its IDs fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSpec {
    pub uid: u32,
    /// Its primary group, by name. Defaults to a group named after the
    /// user. A group named after the user is made with the user's UID as
    /// its GID unless `[accounts.groups]` or base-passwd has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Groups it's a member of besides its primary one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Defaults to `/nonexistent`, as for `adduser --system`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// Defaults to `/usr/sbin/nologin`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// The comment field: the user's full name, or what it's for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gecos: Option<String>,
}

impl UserSpec {
    /// Its primary group's name.
    pub fn group<'a>(&'a self, name: &'a str) -> &'a str {
        self.group.as_deref().unwrap_or(name)
    }

    pub fn home(&self) -> &str {
        self.home.as_deref().unwrap_or("/nonexistent")
    }

    pub fn shell(&self) -> &str {
        self.shell.as_deref().unwrap_or("/usr/sbin/nologin")
    }
}

/// The highest ID an account may have: 65534 is `nobody`, and 65535 means
/// "no ID" to many tools.
const MAX_ID: u32 = 65533;

impl Accounts {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.users.is_empty()
    }

    /// These accounts, with `other`'s added: a group or user `other` also
    /// declares takes `other`'s declaration.
    pub(super) fn merged(&self, other: Accounts) -> Accounts {
        let mut merged = self.clone();
        merged.groups.extend(other.groups);
        merged.users.extend(other.users);
        merged
    }

    /// Whether the accounts make sense on their own. Whether they get
    /// along with base-passwd's is up to the image, at build.
    pub(super) fn check(&self) -> Result<(), String> {
        let mut gids: IndexMap<u32, &str> = IndexMap::new();
        for (name, &gid) in &self.groups {
            check_name("group", name)?;
            check_id("group", name, gid)?;
            if let Some(other) = gids.insert(gid, name) {
                return Err(format!("groups '{other}' and '{name}' both have GID {gid}"));
            }
        }
        let mut uids: IndexMap<u32, &str> = IndexMap::new();
        for (name, user) in &self.users {
            check_name("user", name)?;
            check_id("user", name, user.uid)?;
            if let Some(other) = uids.insert(user.uid, name) {
                return Err(format!(
                    "users '{other}' and '{name}' both have UID {}",
                    user.uid
                ));
            }
            for group in user.group.iter().chain(&user.groups) {
                check_name("group", group)?;
            }
            for (field, path) in [("home", &user.home), ("shell", &user.shell)] {
                if let Some(path) = path
                    && (!path.starts_with('/') || has_separator(path))
                {
                    return Err(format!(
                        "user '{name}': `{field}` must be an absolute path without ':'"
                    ));
                }
            }
            if user.gecos.as_deref().is_some_and(has_separator) {
                return Err(format!(
                    "user '{name}': `gecos` can't have ':' or a newline"
                ));
            }
        }
        Ok(())
    }
}

/// Names as Debian's `adduser` takes them for system accounts.
fn check_name(kind: &str, name: &str) -> Result<(), String> {
    let valid = (1..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "'{name}' is not a {kind} name: lower-case letters, digits, '_' and '-', \
             starting with a letter or '_'"
        ))
    }
}

fn check_id(kind: &str, name: &str, id: u32) -> Result<(), String> {
    if id == 0 || id > MAX_ID {
        return Err(format!(
            "{kind} '{name}': {id} is not an ID to declare; use 1 to {MAX_ID}"
        ));
    }
    Ok(())
}

/// Whether a field would break the line it's on in `/etc/passwd`.
fn has_separator(field: &str) -> bool {
    field.contains([':', '\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts(toml: &str) -> Result<Accounts, String> {
        let accounts: Accounts = toml::from_str(toml).map_err(|e| e.to_string())?;
        accounts.check().map(|()| accounts)
    }

    #[test]
    fn parses_users_and_groups() {
        let accounts = accounts(
            r#"
groups = { ssl-cert = 101 }

[users.postgres]
uid    = 999
groups = ["ssl-cert"]
home   = "/var/lib/postgresql"
shell  = "/bin/bash"
gecos  = "PostgreSQL administrator"

[users.pgbackrest]
uid   = 100
group = "pgbackrest-group"
"#,
        )
        .unwrap();
        assert_eq!(accounts.groups["ssl-cert"], 101);
        let postgres = &accounts.users["postgres"];
        assert_eq!(postgres.group("postgres"), "postgres");
        assert_eq!(postgres.home(), "/var/lib/postgresql");
        let pgbackrest = &accounts.users["pgbackrest"];
        assert_eq!(pgbackrest.group("pgbackrest"), "pgbackrest-group");
        assert_eq!(pgbackrest.home(), "/nonexistent");
        assert_eq!(pgbackrest.shell(), "/usr/sbin/nologin");
    }

    #[test]
    fn rejects_what_would_break_the_files() {
        for (toml, wanted) in [
            ("groups = { Admins = 200 }", "not a group name"),
            ("groups = { a = 200, b = 200 }", "both have GID 200"),
            ("groups = { a = 0 }", "not an ID to declare"),
            ("groups = { a = 65534 }", "not an ID to declare"),
            (
                "users.a = { uid = 300 }\nusers.b = { uid = 300 }",
                "both have UID 300",
            ),
            ("users.a = { uid = 300, home = \"x\" }", "absolute path"),
            (
                "users.a = { uid = 300, shell = \"/bin/sh:x\" }",
                "without ':'",
            ),
            ("users.a = { uid = 300, gecos = \"a:b\" }", "can't have ':'"),
            (
                "users.a = { uid = 300, groups = [\"Bad\"] }",
                "not a group name",
            ),
            ("users.\"1a\" = { uid = 300 }", "not a user name"),
        ] {
            let err = accounts(toml).unwrap_err();
            assert!(err.contains(wanted), "{toml}: {err}");
        }
    }
}
