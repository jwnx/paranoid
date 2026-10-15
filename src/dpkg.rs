//! The dpkg database of an image built without dpkg.
//!
//! dpkg records what it installed in `/var/lib/dpkg`: one stanza per package
//! in `status`, and per-package files in `info/` (`<pkg>.list` with every
//! path the package installed, `<pkg>.md5sums`, `<pkg>.conffiles`). We never
//! run dpkg, so we write the same files ourselves. Without them the image
//! looks empty to `dpkg -l`, to apt, and to vulnerability scanners, which
//! read `status` to find out what's installed.
//!
//! We write what dpkg itself would: `status` fields in dpkg's order, and
//! `Conffiles` with the MD5 of each conffile as installed.
//!
//! Ref.: dpkg(1) FILES, <https://manpages.debian.org/trixie/dpkg/dpkg.1.en.html>

use std::collections::BTreeMap;

use md5::{Digest as _, Md5};
use thiserror::Error;

use crate::deb::{ControlFiles, MAINTAINER_FILES};
use crate::rootfs::{InstalledPath, Node, Rootfs};

/// Errors adding a package to a [`DpkgDatabase`].
#[derive(Debug, Error)]
pub enum DpkgError {
    #[error("control file has no {0} field")]
    MissingField(&'static str),

    #[error("package {0} is installed twice")]
    Duplicate(String),

    #[error("conffile {0} is not a regular file the package installed")]
    MissingConffile(String),
}

/// The fields dpkg knows, in the order it writes them to `status`. Any
/// other field follows them, in the order the package gave it.
///
/// Ref.: `fieldinfos` in dpkg's lib/dpkg/parse.c.
const FIELD_ORDER: &[&str] = &[
    "Package",
    "Essential",
    "Protected",
    "Status",
    "Priority",
    "Section",
    "Installed-Size",
    "Origin",
    "Maintainer",
    "Bugs",
    "Architecture",
    "Multi-Arch",
    "Source",
    "Version",
    "Config-Version",
    "Replaces",
    "Provides",
    "Depends",
    "Pre-Depends",
    "Recommends",
    "Suggests",
    "Breaks",
    "Conflicts",
    "Enhances",
    "Conffiles",
    "Filename",
    "MSDOS-Filename",
    "Size",
    "MD5sum",
    "Description",
    "Triggers-Pending",
    "Triggers-Awaited",
];

/// One installed package.
#[derive(Debug, Clone)]
struct Installed {
    /// The name its `info/` files go by: `<pkg>:<arch>` for
    /// `Multi-Arch: same` packages, like dpkg.
    info_name: String,
    /// The `status` stanza.
    stanza: String,
    control: ControlFiles,
    paths: Vec<String>,
}

/// The dpkg database for a set of installed packages.
#[derive(Debug, Default, Clone)]
pub struct DpkgDatabase {
    /// By package name, so `status` comes out sorted like dpkg writes it.
    packages: BTreeMap<String, Installed>,
}

impl DpkgDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a package as installed, given its control files, the paths
    /// it installed, and the rootfs right after installing it, which holds
    /// its conffiles as shipped.
    pub fn add(
        &mut self,
        control: ControlFiles,
        paths: &[InstalledPath],
        rootfs: &Rootfs,
    ) -> Result<(), DpkgError> {
        let fields = fields(&control.control);
        let name = value(&fields, "Package")
            .ok_or(DpkgError::MissingField("Package"))?
            .to_string();
        let arch = value(&fields, "Architecture").ok_or(DpkgError::MissingField("Architecture"))?;
        let info_name = if value(&fields, "Multi-Arch") == Some("same") {
            format!("{name}:{arch}")
        } else {
            name.clone()
        };

        let conffiles = conffile_hashes(control.conffiles.as_deref(), paths, rootfs)?;
        let installed = Installed {
            info_name,
            stanza: status_stanza(fields, "install ok installed", &conffiles),
            paths: paths.iter().map(|p| p.listed.clone()).collect(),
            control,
        };
        match self.packages.entry(name) {
            std::collections::btree_map::Entry::Occupied(slot) => {
                Err(DpkgError::Duplicate(slot.key().clone()))
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(installed);
                Ok(())
            }
        }
    }

    /// The database as an uncompressed tar rooted at `/`, ready for
    /// [`crate::rootfs::Rootfs::apply_tar`].
    pub fn to_tar(&self) -> std::io::Result<Vec<u8>> {
        let mut tar = tar::Builder::new(Vec::new());
        for dir in [
            "var/lib/dpkg/",
            "var/lib/dpkg/info/",
            "var/lib/dpkg/updates/",
            "var/lib/dpkg/alternatives/",
        ] {
            append(&mut tar, dir, tar::EntryType::Directory, 0o755, b"")?;
        }

        let status: String = self
            .packages
            .values()
            .map(|p| format!("{}\n", p.stanza))
            .collect();
        append_file(&mut tar, "var/lib/dpkg/status", status.as_bytes())?;
        append_file(&mut tar, "var/lib/dpkg/available", b"")?;
        // Database layout 1 names info files `<pkg>:<arch>` for
        // `Multi-Arch: same` packages. Without this file dpkg assumes the
        // pre-multiarch layout 0 and can't find them.
        append_file(&mut tar, "var/lib/dpkg/info/format", b"1\n")?;

        for package in self.packages.values() {
            let info = format!("var/lib/dpkg/info/{}", package.info_name);
            let list: String = package.paths.iter().map(|p| format!("{p}\n")).collect();
            append_file(&mut tar, &format!("{info}.list"), list.as_bytes())?;
            if let Some(md5sums) = &package.control.md5sums {
                append_file(&mut tar, &format!("{info}.md5sums"), md5sums)?;
            }
            if let Some(conffiles) = &package.control.conffiles {
                append_file(&mut tar, &format!("{info}.conffiles"), conffiles.as_bytes())?;
            }
            for (name, data) in &package.control.scripts {
                let executable = MAINTAINER_FILES
                    .iter()
                    .any(|(file, script)| file == name && *script);
                let mode = if executable { 0o755 } else { 0o644 };
                let path = format!("{info}.{name}");
                append(&mut tar, &path, tar::EntryType::Regular, mode, data)?;
            }
        }
        tar.into_inner()
    }
}

/// A control stanza split into fields, each with its raw text (the name,
/// the colon, and every continuation line, without the final newline).
fn fields(stanza: &str) -> Vec<(&str, String)> {
    let mut fields: Vec<(&str, String)> = Vec::new();
    for line in stanza.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, raw)) = fields.last_mut() {
                raw.push('\n');
                raw.push_str(line);
            }
        } else if let Some((name, _)) = line.split_once(':') {
            fields.push((name, line.to_string()));
        }
    }
    fields
}

/// The value of a single-line field.
fn value<'a>(fields: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .and_then(|(_, raw)| raw.split_once(':'))
        .map(|(_, value)| value.trim())
}

/// `(path, md5)` for each conffile, as dpkg records them once installed.
fn conffile_hashes(
    conffiles: Option<&str>,
    paths: &[InstalledPath],
    rootfs: &Rootfs,
) -> Result<Vec<(String, String)>, DpkgError> {
    let Some(conffiles) = conffiles else {
        return Ok(Vec::new());
    };
    conffiles
        .lines()
        .map(str::trim)
        // `remove-on-upgrade /etc/x` names a conffile the package no longer
        // ships, for dpkg to remove on upgrade. There's nothing to record.
        .filter(|line| line.starts_with('/'))
        .map(|path| {
            let data = paths
                .iter()
                .find(|p| p.listed == path)
                .and_then(|p| p.key.as_deref())
                .and_then(|key| match rootfs.get(key) {
                    Some(Node::File(_, data) | Node::Hardlink { data, .. }) => Some(data),
                    _ => None,
                })
                .ok_or_else(|| DpkgError::MissingConffile(path.to_string()))?;
            Ok((path.to_string(), hex::encode(Md5::digest(data))))
        })
        .collect()
}

/// A control stanza as dpkg keeps it in `status`: with `Status` and
/// `Conffiles` added, and fields in dpkg's order.
fn status_stanza(
    mut fields: Vec<(&str, String)>,
    status: &str,
    conffiles: &[(String, String)],
) -> String {
    fields.push(("Status", format!("Status: {status}")));
    if !conffiles.is_empty() {
        let mut raw = "Conffiles:".to_string();
        for (path, md5) in conffiles {
            raw.push_str(&format!("\n {path} {md5}"));
        }
        fields.push(("Conffiles", raw));
    }
    let rank = |name: &str| {
        FIELD_ORDER
            .iter()
            .position(|known| known.eq_ignore_ascii_case(name))
            .unwrap_or(FIELD_ORDER.len())
    };
    // Stable, so unknown fields keep the package's order.
    fields.sort_by_key(|(name, _)| rank(name));
    fields.iter().map(|(_, raw)| format!("{raw}\n")).collect()
}

fn append_file(tar: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) -> std::io::Result<()> {
    append(tar, path, tar::EntryType::Regular, 0o644, data)
}

fn append(
    tar: &mut tar::Builder<Vec<u8>>,
    path: &str,
    kind: tar::EntryType,
    mode: u32,
    data: &[u8],
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_size(data.len() as u64);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, path, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(stanza: &str) -> ControlFiles {
        ControlFiles {
            control: stanza.to_string(),
            md5sums: None,
            conffiles: None,
            scripts: Vec::new(),
        }
    }

    fn path(listed: &str) -> InstalledPath {
        InstalledPath {
            listed: listed.to_string(),
            key: (listed != "/.").then(|| listed.trim_start_matches('/').to_string()),
        }
    }

    fn file(fs: &Rootfs, path: &str) -> String {
        match fs.get(path) {
            Some(Node::File(_, data)) => String::from_utf8(data.to_vec()).unwrap(),
            other => panic!("{path}: {other:?}"),
        }
    }

    /// A rootfs holding `files`, and the paths that put them there.
    fn installed(files: &[(&str, &[u8])]) -> (Rootfs, Vec<InstalledPath>) {
        let tar = crate::deb::test_utils::tar(files);
        let mut fs = Rootfs::new();
        let stats = fs.apply_tar(tar.as_slice()).unwrap();
        (fs, stats.paths)
    }

    fn database(db: &DpkgDatabase) -> Rootfs {
        let mut fs = Rootfs::new();
        fs.apply_tar(db.to_tar().unwrap().as_slice()).unwrap();
        fs
    }

    #[test]
    fn writes_status_sorted_by_name_in_dpkgs_field_order() {
        let mut db = DpkgDatabase::new();
        let empty = Rootfs::new();
        db.add(
            control(
                "Package: zlib1g\nVersion: 1\nArchitecture: amd64\nMaintainer: m\n\
                 Installed-Size: 9\nPriority: optional\nSection: libs\nDepends: libc6\n\
                 Description: z\n more\n  indented\n .\nHomepage: https://zlib.net\n",
            ),
            &[],
            &empty,
        )
        .unwrap();
        db.add(
            control("Package: bash\nVersion: 5.2\nArchitecture: amd64\nEssential: yes\n\n"),
            &[],
            &empty,
        )
        .unwrap();

        assert_eq!(
            file(&database(&db), "var/lib/dpkg/status"),
            "Package: bash\nEssential: yes\nStatus: install ok installed\n\
             Architecture: amd64\nVersion: 5.2\n\n\
             Package: zlib1g\nStatus: install ok installed\nPriority: optional\n\
             Section: libs\nInstalled-Size: 9\nMaintainer: m\nArchitecture: amd64\n\
             Version: 1\nDepends: libc6\n\
             Description: z\n more\n  indented\n .\nHomepage: https://zlib.net\n\n"
        );
    }

    #[test]
    fn records_conffiles_with_their_md5() {
        let (fs, paths) = installed(&[
            ("etc/protocols", b"ip 0 IP\n"),
            ("usr/share/doc/netbase/copyright", b"c"),
        ]);
        let mut netbase = control("Package: netbase\nArchitecture: all\nVersion: 6.4\n");
        netbase.conffiles = Some("/etc/protocols\nremove-on-upgrade /etc/gone\n".into());

        let mut db = DpkgDatabase::new();
        db.add(netbase, &paths, &fs).unwrap();
        let md5 = hex::encode(Md5::digest(b"ip 0 IP\n"));
        let db = database(&db);
        assert_eq!(
            file(&db, "var/lib/dpkg/status"),
            format!(
                "Package: netbase\nStatus: install ok installed\nArchitecture: all\n\
                 Version: 6.4\nConffiles:\n /etc/protocols {md5}\n\n"
            )
        );
        assert_eq!(
            file(&db, "var/lib/dpkg/info/netbase.conffiles"),
            "/etc/protocols\nremove-on-upgrade /etc/gone\n"
        );
    }

    #[test]
    fn a_conffile_the_package_didnt_install_is_an_error() {
        let (fs, paths) = installed(&[("etc/other", b"x")]);
        let mut pkg = control("Package: p\nArchitecture: all\n");
        pkg.conffiles = Some("/etc/missing\n".into());
        assert!(matches!(
            DpkgDatabase::new().add(pkg, &paths, &fs),
            Err(DpkgError::MissingConffile(path)) if path == "/etc/missing"
        ));
    }

    #[test]
    fn writes_info_files_under_the_multiarch_name() {
        let mut db = DpkgDatabase::new();
        let empty = Rootfs::new();
        let mut libc = control("Package: libc6\nArchitecture: amd64\nMulti-Arch: same\n");
        libc.md5sums = Some(b"abc  usr/lib/libc.so.6\n".to_vec());
        db.add(
            libc,
            &[path("/."), path("/usr"), path("/usr/lib/libc.so.6")],
            &empty,
        )
        .unwrap();

        let fs = database(&db);
        assert_eq!(
            file(&fs, "var/lib/dpkg/info/libc6:amd64.list"),
            "/.\n/usr\n/usr/lib/libc.so.6\n"
        );
        assert_eq!(
            file(&fs, "var/lib/dpkg/info/libc6:amd64.md5sums"),
            "abc  usr/lib/libc.so.6\n"
        );
        assert_eq!(file(&fs, "var/lib/dpkg/info/format"), "1\n");
    }

    #[test]
    fn rejects_a_package_installed_twice() {
        let mut db = DpkgDatabase::new();
        let empty = Rootfs::new();
        db.add(control("Package: bash\nArchitecture: amd64\n"), &[], &empty)
            .unwrap();
        assert!(matches!(
            db.add(control("Package: bash\nArchitecture: amd64\n"), &[], &empty),
            Err(DpkgError::Duplicate(_))
        ));
    }

    #[test]
    fn rejects_a_control_file_without_package() {
        assert!(matches!(
            DpkgDatabase::new().add(control("Architecture: amd64\n"), &[], &Rootfs::new()),
            Err(DpkgError::MissingField("Package"))
        ));
    }
}
