//! Configuring apt in an image for the manifest's repositories.
//!
//! An image that will install more packages later, say in a Dockerfile,
//! should get them from where its own came from: the same snapshot, with
//! the same keys and priorities. We write what apt needs for that from the
//! manifest:
//!
//! - `/etc/apt/sources.list.d/paranoid.sources`, a deb822 stanza per
//!   repository, at its snapshot if it has one, with `Check-Valid-Until:
//!   no` there, since an old snapshot's Release has expired;
//! - each repository's `signed_by` keys in `/etc/apt/keyrings/`, which its
//!   stanza names with `Signed-By`, so apt trusts each repository's keys
//!   for it alone, as paranoid does;
//! - `/etc/apt/preferences.d/paranoid`, pinning each repository with an
//!   explicit `priority` to it.

use std::collections::BTreeMap;
use std::fmt::Write;

use thiserror::Error;

use crate::config::Manifest;
use crate::rootfs::{Rootfs, RootfsError};

const SOURCES: &str = "etc/apt/sources.list.d/paranoid.sources";
const PREFERENCES: &str = "etc/apt/preferences.d/paranoid";
const KEYRINGS: &str = "etc/apt/keyrings";

/// Errors configuring apt.
#[derive(Debug, Error)]
pub enum AptError {
    #[error("repository '{repository}': could not read {path}")]
    Keyring {
        repository: String,
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "repositories '{a}' and '{b}' are both on {host} with different priorities, \
         which apt can't tell apart: it pins by host"
    )]
    SharedHost { host: String, a: String, b: String },

    #[error("could not write the apt configuration")]
    Rootfs(#[from] RootfsError),
}

/// The files that configure apt for the manifest's repositories, by path
/// relative to the root.
pub fn files(manifest: &Manifest) -> Result<Vec<(String, Vec<u8>)>, AptError> {
    let mut files = Vec::new();
    let mut sources = String::new();
    let mut preferences = String::new();
    // Each host's priority, and the repository that set it.
    let mut pins: BTreeMap<String, (Option<i32>, &str)> = BTreeMap::new();

    for (name, repo) in &manifest.repositories {
        let path = manifest.resolve_path(&repo.signed_by);
        let keys = std::fs::read(&path).map_err(|source| AptError::Keyring {
            repository: name.clone(),
            path: path.display().to_string(),
            source,
        })?;
        // apt reads armored keys only from a `.asc` file.
        let extension = if keys.starts_with(b"-----BEGIN") {
            "asc"
        } else {
            "gpg"
        };
        let keyring = format!("{KEYRINGS}/paranoid-{}.{extension}", file_name(name));

        if !sources.is_empty() {
            sources.push('\n');
        }
        let _ = writeln!(sources, "# {name}");
        let _ = writeln!(sources, "Types: deb");
        let _ = writeln!(sources, "URIs: {}/", repo.base_url());
        let _ = writeln!(sources, "Suites: {}", repo.distribution);
        let _ = writeln!(sources, "Components: {}", repo.components.join(" "));
        let _ = writeln!(sources, "Signed-By: /{keyring}");
        if repo.snapshot.is_some() {
            let _ = writeln!(sources, "Check-Valid-Until: no");
        }
        files.push((keyring, keys));

        let host = repo.url.host_str().unwrap_or_default().to_string();
        match pins.get(&host) {
            Some((priority, other)) if *priority != repo.priority => {
                return Err(AptError::SharedHost {
                    host,
                    a: other.to_string(),
                    b: name.clone(),
                });
            }
            Some(_) => continue,
            None => {
                pins.insert(host.clone(), (repo.priority, name));
            }
        }
        if let Some(priority) = repo.priority {
            if !preferences.is_empty() {
                preferences.push('\n');
            }
            let _ = writeln!(preferences, "# {name}");
            let _ = writeln!(preferences, "Package: *");
            let _ = writeln!(preferences, "Pin: origin \"{host}\"");
            let _ = writeln!(preferences, "Pin-Priority: {priority}");
        }
    }

    files.push((SOURCES.to_string(), sources.into_bytes()));
    if !preferences.is_empty() {
        files.push((PREFERENCES.to_string(), preferences.into_bytes()));
    }
    Ok(files)
}

/// Write apt's configuration for the manifest's repositories into the
/// rootfs. Nothing a package shipped is replaced.
pub fn write(rootfs: &mut Rootfs, manifest: &Manifest) -> Result<(), AptError> {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, data) in files(manifest)? {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(data.len() as u64);
        tar.append_data(&mut header, &path, data.as_slice())
            .map_err(RootfsError::from)?;
    }
    let tar = tar.into_inner().map_err(RootfsError::from)?;
    rootfs.add_tar(tar.as_slice(), "etc/apt")?;
    Ok(())
}

/// A repository's name as part of a file name.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn manifest(dir: &Path, repositories: &str) -> Manifest {
        std::fs::write(
            dir.join("debian.asc"),
            "-----BEGIN PGP PUBLIC KEY BLOCK-----\n",
        )
        .unwrap();
        std::fs::write(dir.join("pgdg.gpg"), [0x99, 0x01]).unwrap();
        let toml = format!("{repositories}\n[[packages]]\nname = \"bash\"\n");
        Manifest::from_file_content(&dir.join("paranoid.toml"), &toml).unwrap()
    }

    const REPOSITORIES: &str = r#"
[repositories.debian]
url          = "https://snapshot.debian.org/archive/debian"
snapshot     = "20261020T000000Z"
distribution = "bookworm"
components   = ["main", "contrib"]
signed_by    = "debian.asc"

[repositories.pgdg]
url          = "https://apt.postgresql.org/pub/repos/apt"
distribution = "bookworm-pgdg"
components   = ["main", "17"]
signed_by    = "pgdg.gpg"

[repositories."pgdg archive"]
url          = "https://apt-archive.postgresql.org/pub/repos/apt"
distribution = "bookworm-pgdg-archive"
components   = ["main"]
signed_by    = "pgdg.gpg"
priority     = 100
"#;

    fn text(files: &[(String, Vec<u8>)], path: &str) -> String {
        let (_, data) = files.iter().find(|(p, _)| p == path).unwrap();
        String::from_utf8(data.clone()).unwrap()
    }

    #[test]
    fn configures_apt_for_the_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(&manifest(dir.path(), REPOSITORIES)).unwrap();
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            [
                "etc/apt/keyrings/paranoid-debian.asc",
                "etc/apt/keyrings/paranoid-pgdg.gpg",
                "etc/apt/keyrings/paranoid-pgdg-archive.gpg",
                SOURCES,
                PREFERENCES,
            ]
        );
        assert_eq!(
            text(&files, SOURCES),
            "# debian\n\
             Types: deb\n\
             URIs: https://snapshot.debian.org/archive/debian/20261020T000000Z/\n\
             Suites: bookworm\n\
             Components: main contrib\n\
             Signed-By: /etc/apt/keyrings/paranoid-debian.asc\n\
             Check-Valid-Until: no\n\
             \n\
             # pgdg\n\
             Types: deb\n\
             URIs: https://apt.postgresql.org/pub/repos/apt/\n\
             Suites: bookworm-pgdg\n\
             Components: main 17\n\
             Signed-By: /etc/apt/keyrings/paranoid-pgdg.gpg\n\
             \n\
             # pgdg archive\n\
             Types: deb\n\
             URIs: https://apt-archive.postgresql.org/pub/repos/apt/\n\
             Suites: bookworm-pgdg-archive\n\
             Components: main\n\
             Signed-By: /etc/apt/keyrings/paranoid-pgdg-archive.gpg\n"
        );
        assert_eq!(
            text(&files, PREFERENCES),
            "# pgdg archive\n\
             Package: *\n\
             Pin: origin \"apt-archive.postgresql.org\"\n\
             Pin-Priority: 100\n"
        );
    }

    #[test]
    fn repositories_on_one_host_need_one_priority() {
        let dir = tempfile::tempdir().unwrap();
        let shared = REPOSITORIES.replace("apt-archive.postgresql.org", "apt.postgresql.org");
        let err = files(&manifest(dir.path(), &shared)).unwrap_err();
        assert!(
            err.to_string()
                .contains("'pgdg' and 'pgdg archive' are both on apt.postgresql.org"),
            "{err}"
        );

        let alike = shared.replace("priority     = 100\n", "");
        let files = files(&manifest(dir.path(), &alike)).unwrap();
        assert!(files.iter().all(|(p, _)| p != PREFERENCES));
    }
}
