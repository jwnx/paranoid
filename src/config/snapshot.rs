//! Moving a manifest's snapshots.

use super::{ManifestError, SnapshotTime};

/// A repository whose snapshot moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedSnapshot {
    pub repository: String,
    /// The snapshot as it was written.
    pub from: String,
}

/// Point every repository of a TOML manifest that has a `snapshot` at
/// `time`, leaving the rest of the text as it was written, comments and
/// all. Returns the new text and the repositories whose snapshot changed.
pub fn set_snapshots(
    toml: &str,
    time: SnapshotTime,
) -> Result<(String, Vec<MovedSnapshot>), ManifestError> {
    let mut doc: toml_edit::DocumentMut = toml.parse()?;
    let to = time.to_string();
    let mut moved = Vec::new();
    let repositories = doc
        .get_mut("repositories")
        .and_then(|r| r.as_table_like_mut());
    for (name, repo) in repositories.into_iter().flat_map(|r| r.iter_mut()) {
        let Some(value) = repo
            .as_table_like_mut()
            .and_then(|r| r.get_mut("snapshot"))
            .and_then(|s| s.as_value_mut())
        else {
            continue;
        };
        let from = value.as_str().unwrap_or_default().to_string();
        if from == to {
            continue;
        }
        let decor = value.decor().clone();
        *value = to.as_str().into();
        *value.decor_mut() = decor;
        moved.push(MovedSnapshot {
            repository: name.get().to_string(),
            from,
        });
    }
    Ok((doc.to_string(), moved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> SnapshotTime {
        s.parse().unwrap()
    }

    #[test]
    fn moves_every_snapshot_and_keeps_the_rest_as_written() {
        let toml = r#"# Our repositories.
[repositories.debian]
url      = "https://snapshot.debian.org/archive/debian"
snapshot = "20261005T000000Z"   # moved by the bot

[repositories.security]
url = "https://snapshot.debian.org/archive/debian-security"
snapshot = "20261001T000000Z"

[repositories.pgdg]
url = "https://apt.postgresql.org/pub/repos/apt"
"#;
        let (text, moved) = set_snapshots(toml, at("20261020T120000Z")).unwrap();
        assert_eq!(
            text,
            toml.replace("20261005T000000Z", "20261020T120000Z")
                .replace("20261001T000000Z", "20261020T120000Z")
        );
        assert_eq!(
            moved,
            [
                MovedSnapshot {
                    repository: "debian".to_string(),
                    from: "20261005T000000Z".to_string(),
                },
                MovedSnapshot {
                    repository: "security".to_string(),
                    from: "20261001T000000Z".to_string(),
                },
            ]
        );
    }

    #[test]
    fn moves_snapshots_in_inline_and_dotted_tables() {
        let toml = r#"repositories.a.snapshot = "20261005T000000Z"
repositories.b = { url = "https://x", snapshot = "20261005T000000Z" }
"#;
        let (text, moved) = set_snapshots(toml, at("20261020T000000Z")).unwrap();
        assert_eq!(text, toml.replace("20261005T000000Z", "20261020T000000Z"));
        assert_eq!(moved.len(), 2);
    }

    #[test]
    fn leaves_a_snapshot_already_there_alone() {
        let toml = "[repositories.debian]\nsnapshot = \"20261005T000000Z\"\n";
        let (text, moved) = set_snapshots(toml, at("20261005T000000Z")).unwrap();
        assert_eq!(text, toml);
        assert!(moved.is_empty());
    }
}
