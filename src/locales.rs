//! Keeping only the locales an image asks for out of `locales-all`.
//!
//! `locales-all` ships every locale glibc supports, already compiled by
//! `localedef`, in `/usr/lib/locale/<name>/`: 225 MB across about 500
//! locales, which share files through symlinks between them. An image that
//! asks for a few keeps those, with the files they link to copied in, and
//! drops the rest. Nothing runs, and the bytes are Debian's.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::config::LOCALES_PACKAGE;
use crate::rootfs::{Node, Rootfs, RootfsError};

const LOCALE_DIR: &str = "usr/lib/locale";

/// How many symlinks we follow resolving one, as Linux does.
const MAX_HOPS: usize = 40;

/// Errors keeping locales.
#[derive(Debug, Error)]
pub enum LocalesError {
    #[error("the image has no dpkg file list for {LOCALES_PACKAGE}")]
    NoList,

    #[error("{LOCALES_PACKAGE} has no locale '{wanted}'{}", close_ones(.close))]
    NotShipped { wanted: String, close: Vec<String> },

    #[error("'/{0}' in {LOCALES_PACKAGE} links to nothing it has")]
    Broken(String),

    #[error(transparent)]
    Rootfs(#[from] RootfsError),
}

fn close_ones(close: &[String]) -> String {
    match close {
        [] => String::new(),
        close => format!("; it has {}", close.join(", ")),
    }
}

/// Keep the locales `wanted` names, by `LANG` spelling, and drop the rest
/// of `locales-all`'s. `all` keeps every one.
pub fn keep(rootfs: &mut Rootfs, wanted: &[String]) -> Result<(), LocalesError> {
    if wanted.iter().any(|w| w == "all") {
        return Ok(());
    }
    let shipped = shipped(rootfs)?;

    let mut kept = BTreeSet::new();
    for wanted in wanted {
        let names = [wanted.clone(), normalize(wanted)];
        if let Some(name) = names.iter().find(|n| shipped.contains(*n)) {
            kept.insert(name.clone());
            continue;
        }
        // One another package ships, like libc-bin's C.utf8.
        let elsewhere = names
            .iter()
            .any(|n| matches!(rootfs.get(&format!("{LOCALE_DIR}/{n}")), Some(Node::Dir(_))));
        if !elsewhere {
            let language = wanted
                .split(['_', '.', '@'])
                .next()
                .unwrap_or_default()
                .to_string();
            let close = shipped
                .iter()
                .filter(|s| s.split(['_', '.', '@']).next() == Some(language.as_str()))
                .take(8)
                .cloned()
                .collect();
            return Err(LocalesError::NotShipped {
                wanted: wanted.clone(),
                close,
            });
        }
    }

    // A kept locale's links into a locale that goes become copies.
    let inside = |path: &str| {
        kept.iter()
            .any(|k| path.starts_with(&format!("{LOCALE_DIR}/{k}/")))
    };
    let mut copies = Vec::new();
    for (path, node) in rootfs.iter() {
        if !inside(path) || !matches!(node, Node::Symlink(..)) {
            continue;
        }
        let target = resolve(rootfs, path)?;
        if !inside(&target) {
            let Some(Node::File(meta, data)) = rootfs.get(&target) else {
                return Err(LocalesError::Broken(path.to_string()));
            };
            copies.push((path.to_string(), Node::File(*meta, data.clone())));
        }
    }
    for (path, node) in copies {
        rootfs.replace_node(&path, node)?;
    }
    for name in shipped.difference(&kept) {
        rootfs.remove_tree(&format!("{LOCALE_DIR}/{name}"));
    }
    Ok(())
}

/// The locales `locales-all` ships, by directory name, from its dpkg file
/// list.
fn shipped(rootfs: &Rootfs) -> Result<BTreeSet<String>, LocalesError> {
    let list = format!("var/lib/dpkg/info/{LOCALES_PACKAGE}.list");
    let Some(Node::File(_, data)) = rootfs.get(&list) else {
        return Err(LocalesError::NoList);
    };
    let prefix = format!("/{LOCALE_DIR}/");
    Ok(String::from_utf8_lossy(data)
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .filter_map(|rest| rest.split('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

/// Where the symlink at `path` ends up, following links.
fn resolve(rootfs: &Rootfs, path: &str) -> Result<String, LocalesError> {
    let mut current = path.to_string();
    for _ in 0..MAX_HOPS {
        let Some(Node::Symlink(_, target)) = rootfs.get(&current) else {
            return Ok(current);
        };
        let base: Vec<&str> = match target.strip_prefix('/') {
            Some(_) => Vec::new(),
            None => {
                let mut dir: Vec<&str> = current.split('/').collect();
                dir.pop();
                dir
            }
        };
        let mut parts: Vec<String> = base.iter().map(|s| s.to_string()).collect();
        for part in target.trim_start_matches('/').split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part.to_string()),
            }
        }
        current = parts.join("/");
    }
    Err(LocalesError::Broken(path.to_string()))
}

/// A locale name the way glibc looks it up on disk: the codeset in lower
/// case without punctuation, and `iso` in front of a codeset that's only
/// digits. `en_US.UTF-8` is `en_US.utf8`; `de_DE.ISO-8859-15@euro` is
/// `de_DE.iso885915@euro`.
pub fn normalize(name: &str) -> String {
    let (name, modifier) = match name.split_once('@') {
        Some((name, modifier)) => (name, Some(modifier)),
        None => (name, None),
    };
    let mut normalized = match name.split_once('.') {
        Some((language, codeset)) => {
            let codeset: String = codeset
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect();
            let codeset = if codeset.chars().all(|c| c.is_ascii_digit()) {
                format!("iso{codeset}")
            } else {
                codeset
            };
            format!("{language}.{codeset}")
        }
        None => name.to_string(),
    };
    if let Some(modifier) = modifier {
        normalized.push('@');
        normalized.push_str(modifier);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rootfs as `locales-all` and libc-bin leave it: `en_US.utf8` links
    /// its LC_COLLATE to `aa_DJ.utf8`'s and its LC_CTYPE to `de_DE.utf8`'s.
    fn rootfs() -> Rootfs {
        let dir = tar::EntryType::Directory;
        let file = tar::EntryType::Regular;
        let link = tar::EntryType::Symlink;
        // (path, kind, contents or target, in locales-all's list)
        let mut entries: Vec<(String, tar::EntryType, String, bool)> = Vec::new();
        for locale in [
            "aa_DJ.utf8",
            "de_DE.utf8",
            "en_US.utf8",
            "de_DE.iso885915@euro",
        ] {
            entries.push((
                format!("usr/lib/locale/{locale}/"),
                dir,
                String::new(),
                true,
            ));
            let time = format!("usr/lib/locale/{locale}/LC_TIME");
            entries.push((time, file, locale.to_string(), true));
        }
        for (path, kind, data, listed) in [
            (
                "usr/lib/locale/aa_DJ.utf8/LC_COLLATE",
                file,
                "collate",
                true,
            ),
            ("usr/lib/locale/de_DE.utf8/LC_CTYPE", file, "ctype", true),
            (
                "usr/lib/locale/en_US.utf8/LC_COLLATE",
                link,
                "../aa_DJ.utf8/LC_COLLATE",
                true,
            ),
            // A link to a link.
            ("usr/lib/locale/de_DE.utf8/LC_NAME", link, "LC_CTYPE", true),
            (
                "usr/lib/locale/en_US.utf8/LC_CTYPE",
                link,
                "../de_DE.utf8/LC_NAME",
                true,
            ),
            ("usr/lib/locale/C.utf8/", dir, "", false),
            ("usr/lib/locale/C.utf8/LC_CTYPE", file, "C", false),
        ] {
            entries.push((path.to_string(), kind, data.to_string(), listed));
        }
        let mut list = String::from("/.\n/usr\n/usr/lib\n/usr/lib/locale\n");
        for (path, _, _, listed) in &entries {
            if *listed {
                list.push_str(&format!("/{}\n", path.trim_end_matches('/')));
            }
        }
        entries.push((
            "var/lib/dpkg/info/locales-all.list".to_string(),
            file,
            list,
            false,
        ));

        let mut tar = tar::Builder::new(Vec::new());
        for (path, kind, data, _) in &entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
            header.set_uid(0);
            header.set_gid(0);
            if kind.is_symlink() {
                header.set_size(0);
                tar.append_link(&mut header, path, data).unwrap();
            } else {
                header.set_size(data.len() as u64);
                tar.append_data(&mut header, path, data.as_bytes()).unwrap();
            }
        }
        let mut rootfs = Rootfs::new();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        rootfs
    }

    fn locales(rootfs: &Rootfs) -> Vec<&str> {
        rootfs
            .iter()
            .filter_map(|(path, _)| path.strip_prefix("usr/lib/locale/"))
            .filter(|rest| !rest.contains('/'))
            .collect()
    }

    fn data<'a>(rootfs: &'a Rootfs, path: &str) -> &'a [u8] {
        match rootfs.get(path) {
            Some(Node::File(_, data)) => data,
            other => panic!("{path}: {other:?}"),
        }
    }

    #[test]
    fn normalizes_names_as_glibc_does() {
        assert_eq!(normalize("en_US.UTF-8"), "en_US.utf8");
        assert_eq!(normalize("de_DE.ISO-8859-15@euro"), "de_DE.iso885915@euro");
        assert_eq!(normalize("ja_JP.eucJP"), "ja_JP.eucjp");
        assert_eq!(normalize("en_US"), "en_US");
        assert_eq!(normalize("C.UTF-8"), "C.utf8");
    }

    #[test]
    fn keeps_what_it_asks_for_and_what_that_links_to() {
        let mut rootfs = rootfs();
        let wanted = ["en_US.UTF-8".to_string(), "C.UTF-8".to_string()];
        keep(&mut rootfs, &wanted).unwrap();
        assert_eq!(locales(&rootfs), ["C.utf8", "en_US.utf8"]);
        // Links into locales that went are copies now.
        assert_eq!(
            data(&rootfs, "usr/lib/locale/en_US.utf8/LC_COLLATE"),
            b"collate"
        );
        assert_eq!(
            data(&rootfs, "usr/lib/locale/en_US.utf8/LC_CTYPE"),
            b"ctype"
        );
        assert_eq!(data(&rootfs, "usr/lib/locale/C.utf8/LC_CTYPE"), b"C");
    }

    #[test]
    fn a_link_between_kept_locales_stays_a_link() {
        let mut rootfs = rootfs();
        let wanted = ["en_US.UTF-8".to_string(), "aa_DJ.UTF-8".to_string()];
        keep(&mut rootfs, &wanted).unwrap();
        assert!(matches!(
            rootfs.get("usr/lib/locale/en_US.utf8/LC_COLLATE"),
            Some(Node::Symlink(..))
        ));
        assert_eq!(
            data(&rootfs, "usr/lib/locale/en_US.utf8/LC_CTYPE"),
            b"ctype"
        );
        assert_eq!(locales(&rootfs), ["C.utf8", "aa_DJ.utf8", "en_US.utf8"]);
    }

    #[test]
    fn all_keeps_every_locale_and_a_stranger_is_refused() {
        let mut rootfs = rootfs();
        keep(&mut rootfs, &["all".to_string()]).unwrap();
        assert_eq!(locales(&rootfs).len(), 5);

        let err = keep(&mut rootfs, &["de_AT.UTF-8".to_string()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "locales-all has no locale 'de_AT.UTF-8'; it has de_DE.iso885915@euro, de_DE.utf8"
        );
        let kept = keep(&mut rootfs, &["de_DE.ISO-8859-15@euro".to_string()]);
        assert!(kept.is_ok());
    }
}
