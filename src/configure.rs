//! Configuring an image: running every package's maintainer scripts.
//!
//! paranoid never runs code from a package anywhere else. Here it hands
//! the unpacked rootfs to an OCI runtime the build machine has (podman or
//! Docker), runs `dpkg --configure` in a container with no network and
//! nothing on stdin, in the order debootstrap uses, and reads the
//! container's filesystem back. Then it removes what varies from one run
//! to the next. See `docs/configure.md`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;

use thiserror::Error;
use tokio::process::Command;

use crate::arch::Architecture;
use crate::oci::{Compression, ImageOutput, OciConfig, OciConfigError, OciError, write_image};
use crate::rootfs::{Node, Rootfs, RootfsError};

/// What runs the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Podman,
    Docker,
}

impl Runtime {
    fn program(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }

    /// What `run` needs besides the flags both runtimes share.
    ///
    /// podman adds the user it runs as to `/etc/passwd` and `/etc/group`,
    /// writing them when the image has none yet, as it doesn't before
    /// base-passwd is configured. base-passwd then keeps what it finds,
    /// and the files lack the users and groups the other packages need.
    fn run_flags(self) -> &'static [&'static str] {
        match self {
            Self::Podman => &["--passwd=false"],
            Self::Docker => &[],
        }
    }

    /// The first of podman and Docker on `PATH`.
    pub fn detect() -> Option<Self> {
        let path = std::env::var_os("PATH")?;
        [Self::Podman, Self::Docker].into_iter().find(|runtime| {
            std::env::split_paths(&path).any(|dir| is_executable(&dir.join(runtime.program())))
        })
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl FromStr for Runtime {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "podman" => Ok(Self::Podman),
            "docker" => Ok(Self::Docker),
            other => Err(format!("unknown runtime '{other}': podman or docker")),
        }
    }
}

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program())
    }
}

/// Errors configuring an image.
#[derive(Debug, Error)]
pub enum ConfigureError {
    #[error("configuring needs podman or docker on PATH, or --runtime")]
    NoRuntime,

    #[error("configuring needs {0} in the image; add the package that ships it")]
    Missing(&'static str),

    #[error("{runtime} {step} failed ({status}):\n{output}")]
    Runtime {
        runtime: Runtime,
        step: &'static str,
        status: String,
        output: String,
    },

    #[error("could not run {0}")]
    Spawn(Runtime, #[source] std::io::Error),

    #[error("interrupted by {0}")]
    Interrupted(&'static str),

    #[error("could not stage the image for the container")]
    Io(#[from] std::io::Error),

    #[error("could not write the image for the container")]
    Image(#[from] OciError),

    #[error("could not write the image for the container")]
    Config(#[from] OciConfigError),

    #[error("could not read the configured filesystem back")]
    Rootfs(#[from] RootfsError),
}

/// How many lines of a failed step's output an error shows.
const OUTPUT_LINES: usize = 40;

/// What of a failed step's output an error shows: dpkg's errors, which a
/// long list of packages it then gave up on can push out of sight, and
/// the last lines.
fn summary(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let tail = &lines[lines.len().saturating_sub(OUTPUT_LINES)..];
    let mut summary: Vec<&str> = lines[..lines.len() - tail.len()]
        .iter()
        .filter(|l| l.starts_with("dpkg: error") || l.starts_with("dpkg: ") && l.contains("error"))
        .take(OUTPUT_LINES / 4)
        .copied()
        .collect();
    if !summary.is_empty() {
        summary.push("...");
    }
    summary.extend(tail);
    summary.join("\n")
}

/// What a container runtime makes in a container's filesystem for itself:
/// the files it mounts over, and the filesystems it mounts. They go back
/// to what the image had.
///
/// `/run`, `/tmp` and `/var/tmp` go back too: what a script leaves there
/// has a name `mktemp` picked, and a container gets `/run` empty anyway.
const RUNTIME_PATHS: &[&str] = &[
    "run",
    "tmp",
    "var/tmp",
    ".dockerenv",
    "run/.containerenv",
    "run/secrets",
    "etc/hostname",
    "etc/hosts",
    "etc/resolv.conf",
    "dev",
    "proc",
    "sys",
];

/// Where Rosetta, which runs amd64 binaries on an Apple silicon Mac
/// under colima, Docker Desktop or podman machine, makes its cache:
/// `$HOME/.cache/rosetta`, empty, the first time it translates one. HOME
/// is `/` until base-passwd writes `/etc/passwd`, and `/root` after. A
/// build on Linux has neither, so they go, with the `.cache` they made.
const ROSETTA_CACHES: &[(&str, &str)] = &[
    (".cache", ".cache/rosetta"),
    ("root/.cache", "root/.cache/rosetta"),
];

const POLICY_RC_D: &str = "usr/sbin/policy-rc.d";

/// Configure `rootfs`, an image unpacked with dpkg's database marking
/// every package unpacked, and return the configured filesystem.
/// `source_date_epoch` is the build's, which the scripts see and the
/// cleanup dates `/etc/shadow` with.
pub async fn configure(
    rootfs: &Rootfs,
    runtime: Runtime,
    arch: Architecture,
    source_date_epoch: i64,
) -> Result<Rootfs, ConfigureError> {
    for (what, paths) in [
        ("/bin/sh", ["bin/sh", "usr/bin/sh"]),
        ("dpkg", ["usr/bin/dpkg", "bin/dpkg"]),
    ] {
        if !paths.iter().any(|p| rootfs.get(p).is_some()) {
            return Err(ConfigureError::Missing(what));
        }
    }

    let mut staged = rootfs.clone();
    // No service starts: there's no init in a container, and nothing
    // should run past the build.
    let wrote_policy = staged.get(POLICY_RC_D).is_none();
    if wrote_policy {
        let mut tar = tar::Builder::new(Vec::new());
        let script = b"#!/bin/sh\nexit 101\n";
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(script.len() as u64);
        tar.append_data(&mut header, POLICY_RC_D, &script[..])?;
        staged.add_tar(tar.into_inner()?.as_slice(), POLICY_RC_D)?;
    }

    let dir = tempfile::tempdir()?;
    let mut container = Container::create(runtime, &staged, arch, dir.path()).await?;
    let script = script(
        &required(rootfs),
        rootfs.get("var/lib/dpkg/info/base-passwd.list").is_some(),
    );
    // Ctrl-C or a SIGTERM while the scripts run removes the container and
    // its image before paranoid exits, rather than leaving them behind.
    let exported = tokio::select! {
        result = async {
            container.run(&script, source_date_epoch).await?;
            container.export(dir.path()).await
        } => result,
        signal = interrupted() => Err(ConfigureError::Interrupted(signal)),
    };
    container.remove().await;
    let mut configured = Rootfs::new();
    configured.apply_tar(std::io::BufReader::new(std::fs::File::open(exported?)?))?;

    restore_runtime_paths(&mut configured, rootfs);
    if wrote_policy {
        configured.remove_tree(POLICY_RC_D);
    }
    clean(&mut configured, source_date_epoch);
    Ok(configured)
}

/// Put back what the runtime, or the emulator it runs amd64 binaries
/// with, made in `configured`: as it is in `before`, the rootfs the
/// container started from.
fn restore_runtime_paths(configured: &mut Rootfs, before: &Rootfs) {
    for path in RUNTIME_PATHS {
        configured.restore(before, path);
    }
    for (parent, cache) in ROSETTA_CACHES {
        configured.restore(before, cache);
        let prefix = format!("{parent}/");
        let emptied = before.get(parent).is_none()
            && matches!(configured.get(parent), Some(Node::Dir(_)))
            && !configured.iter().any(|(p, _)| p.starts_with(&prefix));
        if emptied {
            configured.remove_tree(parent);
        }
    }
}

/// Wait for Ctrl-C or SIGTERM, and say which.
async fn interrupted() -> &'static str {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        Some(()) = async { term.as_mut()?.recv().await } => "SIGTERM",
    }
}

/// The script the container runs: base-passwd first, then the required
/// set, then everything else, then an audit, as debootstrap does.
fn script(required: &[String], base_passwd: bool) -> String {
    let mut script = String::from(
        "set -eu\n\
         exec </dev/null 2>&1\n",
    );
    // Users and groups first: other packages' scripts chown to them.
    if base_passwd {
        script.push_str(
            "dpkg --configure --force-depends --force-confdef --force-confold base-passwd\n",
        );
    }
    // Essential packages depend on each other in cycles dpkg can't order,
    // and packages assume the required set is set up without saying so.
    if !required.is_empty() {
        script.push_str(
            "dpkg --configure --force-depends --force-configure-any --force-confdef --force-confold",
        );
        for package in required {
            script.push(' ');
            script.push_str(package);
        }
        script.push('\n');
    }
    script.push_str(
        "dpkg --configure --pending --force-depends --force-configure-any \
         --force-confdef --force-confold\n\
         audit=$(dpkg --audit)\n\
         if [ -n \"$audit\" ]; then echo \"$audit\" >&2; exit 1; fi\n",
    );
    script
}

/// The packages to configure before the rest, in dpkg's database order:
/// `Priority: required` or `Essential: yes`, besides base-passwd.
fn required(rootfs: &Rootfs) -> Vec<String> {
    let Some(Node::File(_, status)) = rootfs.get("var/lib/dpkg/status") else {
        return Vec::new();
    };
    String::from_utf8_lossy(status)
        .split("\n\n")
        .filter_map(|stanza| {
            let field = |name: &str| {
                stanza
                    .lines()
                    .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
                    .map(str::trim)
            };
            let package = field("Package")?;
            let required =
                field("Priority") == Some("required") || field("Essential") == Some("yes");
            let unpacked = field("Status").is_some_and(|s| s.ends_with(" unpacked"));
            // It goes into a shell command, so only what Debian allows in
            // a package name.
            let valid = package.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.')
            });
            (required && unpacked && valid && package != "base-passwd").then(|| package.to_string())
        })
        .collect()
}

/// What configuring leaves behind that differs from one run to the next,
/// or shouldn't ship: dpkg's and alternatives' logs, the backups dpkg and
/// debconf make, ldconfig's cache of inode numbers and times, ssl-cert's
/// random snakeoil key pair, and machine IDs. `/etc/shadow`'s day of last
/// change is the build's, not the day it ran.
fn clean(rootfs: &mut Rootfs, source_date_epoch: i64) {
    for path in [
        "var/log/dpkg.log",
        "var/log/alternatives.log",
        "var/log/apt",
        "var/cache/ldconfig/aux-cache",
        "etc/ssl/private/ssl-cert-snakeoil.key",
        "etc/ssl/certs/ssl-cert-snakeoil.pem",
        "etc/machine-id",
        "var/lib/dbus/machine-id",
        // The backups shadow's tools make, with today's date in them.
        "etc/passwd-",
        "etc/group-",
        "etc/shadow-",
        "etc/gshadow-",
        "etc/subuid-",
        "etc/subgid-",
    ] {
        rootfs.remove_tree(path);
    }
    let doomed: Vec<String> = rootfs
        .iter()
        .filter(|(path, node)| {
            let backup = ["var/lib/dpkg/", "var/cache/debconf/"]
                .iter()
                .any(|dir| path.strip_prefix(dir).is_some_and(|name| !name.contains('/') && name.ends_with("-old")));
            // The hash link to the snakeoil certificate.
            let snakeoil = path.starts_with("etc/ssl/certs/")
                && matches!(node, Node::Symlink(_, target) if target.ends_with("ssl-cert-snakeoil.pem"));
            // The package's version of a conffile the manifest declares,
            // which `--force-confold` leaves beside ours.
            let dist = path.ends_with(".dpkg-dist");
            backup || snakeoil || dist
        })
        .map(|(path, _)| path.to_string())
        .collect();
    for path in doomed {
        rootfs.remove_tree(&path);
    }

    let day = (source_date_epoch.max(0) / 86_400).to_string();
    if let Some(Node::File(meta, data)) = rootfs.get("etc/shadow") {
        let shadow: String = String::from_utf8_lossy(data)
            .lines()
            .map(|line| {
                let mut fields: Vec<&str> = line.split(':').collect();
                // 0 means "change it at the next login": keep that.
                if fields.len() > 2 && !fields[2].is_empty() && fields[2] != "0" {
                    fields[2] = &day;
                }
                format!("{}\n", fields.join(":"))
            })
            .collect();
        let node = Node::File(*meta, shadow.into_bytes().into());
        rootfs
            .replace_node("etc/shadow", node)
            .expect("replacing a file with a file");
    }
}

/// A container made from a rootfs, with the image it came from.
struct Container {
    runtime: Runtime,
    image: String,
    name: String,
    platform: String,
    /// Whether [`Self::remove`] ran; if not, dropping it removes them.
    removed: bool,
}

impl Drop for Container {
    /// Remove the container and its image if a panic, or a future dropped
    /// halfway, got in the way of [`Container::remove`].
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        for args in [["rm", "-f", &self.name], ["rmi", "-f", &self.image]] {
            let _ = std::process::Command::new(self.runtime.program())
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

impl Container {
    /// Load `rootfs` into the runtime as an image.
    async fn create(
        runtime: Runtime,
        rootfs: &Rootfs,
        arch: Architecture,
        dir: &Path,
    ) -> Result<Self, ConfigureError> {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let platform = arch
            .oci_platform()
            .ok_or(ConfigureError::Missing("a concrete architecture"))?;
        let config = OciConfig::new()
            .architecture(arch)
            .compression(Compression::Uncompressed)
            .try_image_name("paranoid-configure")?
            .try_image_tag(unique.as_str())?;
        let tarball = dir.join("unpacked.tar");
        let image = write_image(&config, rootfs, &ImageOutput::Tarball(tarball.clone()), &())?;
        let container = Self {
            runtime,
            image: image.reference,
            name: format!("paranoid-configure-{unique}"),
            removed: false,
            platform: match platform.variant {
                Some(variant) => format!("linux/{}/{variant}", platform.architecture),
                None => format!("linux/{}", platform.architecture),
            },
        };
        let tarball = tarball.to_string_lossy().into_owned();
        container
            .step("load", &["load", "-q", "-i", &tarball])
            .await?;
        Ok(container)
    }

    /// Run `script` in a new container, with no network and no stdin.
    async fn run(&self, script: &str, source_date_epoch: i64) -> Result<(), ConfigureError> {
        let epoch = format!("SOURCE_DATE_EPOCH={source_date_epoch}");
        let mut args = vec![
            "run",
            "--name",
            &self.name,
            "--network=none",
            "--pull=never",
            "--platform",
            &self.platform,
            "--user=0:0",
            // A script that records the hostname records this one,
            // not the runtime's random one.
            "--hostname=localhost",
            "--env=DEBIAN_FRONTEND=noninteractive",
            "--env=LC_ALL=C.UTF-8",
            "--env=TZ=UTC",
            "--env=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "--env",
            &epoch,
            "--entrypoint=/bin/sh",
        ];
        args.extend(self.runtime.run_flags());
        args.extend([self.image.as_str(), "-c", script]);
        self.step("run", &args).await
    }

    /// The container's filesystem, as a tar file in `dir`.
    async fn export(&self, dir: &Path) -> Result<PathBuf, ConfigureError> {
        let path = dir.join("configured.tar");
        let output = path.to_string_lossy().into_owned();
        self.step("export", &["export", "-o", &output, &self.name])
            .await?;
        Ok(path)
    }

    /// Remove the container and its image, whatever happened.
    async fn remove(&mut self) {
        let _ = self.step("rm", &["rm", "-f", &self.name]).await;
        let _ = self.step("rmi", &["rmi", "-f", &self.image]).await;
        self.removed = true;
    }

    async fn step(&self, step: &'static str, args: &[&str]) -> Result<(), ConfigureError> {
        let output = Command::new(self.runtime.program())
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| ConfigureError::Spawn(self.runtime, e))?;
        if output.status.success() {
            return Ok(());
        }
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Err(ConfigureError::Runtime {
            runtime: self.runtime,
            step,
            status: output.status.to_string(),
            output: summary(&text),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rootfs::Meta;

    fn with_files(files: &[(&str, &str)]) -> Rootfs {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, data) in files {
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

    #[test]
    fn configures_the_required_set_after_base_passwd() {
        let rootfs = with_files(&[(
            "var/lib/dpkg/status",
            "Package: base-passwd\nStatus: install ok unpacked\nEssential: yes\n\
             Priority: required\n\n\
             Package: bash\nStatus: install ok unpacked\nEssential: yes\nPriority: required\n\n\
             Package: libc6\nStatus: install ok unpacked\nPriority: optional\n\n\
             Package: login\nStatus: install ok unpacked\nPriority: required\n\n\
             Package: mawk\nStatus: install ok installed\nPriority: required\n\n\
             Package: evil;rm\nStatus: install ok unpacked\nPriority: required\n\n\
             Package: postgresql-common\nStatus: install ok unpacked\nPriority: optional\n",
        )]);
        let required = required(&rootfs);
        assert_eq!(required, ["bash", "login"]);
        let script = script(&required, true);
        assert!(script.contains("exec </dev/null 2>&1"));
        assert_eq!(script.matches("--force-confdef --force-confold").count(), 3);
        let base_passwd = script.find("--force-confold base-passwd").unwrap();
        let essential = script.find("--force-confold bash login\n").unwrap();
        let rest = script.find("--configure --pending").unwrap();
        assert!(base_passwd < essential && essential < rest, "{script}");
        assert!(script.contains("dpkg --audit"));
        assert!(!super::script(&[], false).contains("base-passwd"));
    }

    #[test]
    fn cleans_what_varies() {
        let mut rootfs = with_files(&[
            ("var/log/dpkg.log", "today"),
            ("var/log/apt/history.log", "today"),
            ("var/log/wtmp", ""),
            ("var/lib/dpkg/status-old", "old"),
            ("var/lib/dpkg/status", "new"),
            ("var/cache/debconf/config.dat-old", "old"),
            ("var/cache/ldconfig/aux-cache", "inodes"),
            ("etc/ssl/certs/ssl-cert-snakeoil.pem", "cert"),
            ("etc/ssl/certs/ca-certificates.crt", "bundle"),
            ("etc/ssl/private/ssl-cert-snakeoil.key", "key"),
            ("etc/machine-id", "random"),
            (
                "etc/shadow",
                "root:*:20754:0:99999:7:::\nnobody:*::0:99999:7:::\nnew:*:0:0:99999:7:::\n",
            ),
            ("etc/shadow-", "root:*:20754:0:99999:7:::\n"),
            ("etc/passwd-", "root:x:0:0::/root:/bin/sh\n"),
            ("etc/adduser.conf", "# ours\n"),
            ("etc/adduser.conf.dpkg-dist", "# the package's\n"),
        ]);
        let link = Node::Symlink(
            Meta {
                mode: 0o777,
                uid: 0,
                gid: 0,
            },
            "ssl-cert-snakeoil.pem".to_string(),
        );
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(0);
        tar.append_link(
            &mut header,
            "etc/ssl/certs/3a1b2c3d.0",
            "ssl-cert-snakeoil.pem",
        )
        .unwrap();
        rootfs
            .apply_tar(tar.into_inner().unwrap().as_slice())
            .unwrap();
        assert_eq!(rootfs.get("etc/ssl/certs/3a1b2c3d.0"), Some(&link));

        // 2026-10-28 is day 20754 since the epoch; the build says 1000.
        clean(&mut rootfs, 1000 * 86_400);
        let left: Vec<&str> = rootfs
            .iter()
            .filter(|(_, node)| !matches!(node, Node::Dir(_)))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            left,
            [
                "etc/adduser.conf",
                "etc/shadow",
                "etc/ssl/certs/ca-certificates.crt",
                "var/lib/dpkg/status",
                "var/log/wtmp",
            ]
        );
        match rootfs.get("etc/shadow") {
            Some(Node::File(_, data)) => assert_eq!(
                &data[..],
                b"root:*:1000:0:99999:7:::\nnobody:*::0:99999:7:::\nnew:*:0:0:99999:7:::\n"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn restores_what_the_runtime_made() {
        let before = with_files(&[("etc/hosts", "ours"), ("bin/sh", "dash")]);
        let mut after = with_files(&[
            ("etc/hosts", ""),
            ("etc/hostname", ""),
            (".dockerenv", ""),
            ("dev/console", ""),
            ("bin/sh", "dash"),
        ]);
        restore_runtime_paths(&mut after, &before);
        let files: Vec<&str> = after
            .iter()
            .filter(|(_, n)| matches!(n, Node::File(..)))
            .map(|(p, _)| p)
            .collect();
        assert_eq!(files, ["bin/sh", "etc/hosts"]);
        assert!(matches!(after.get("etc/hosts"), Some(Node::File(_, d)) if &d[..] == b"ours"));
    }

    #[test]
    fn removes_rosettas_cache() {
        let dir = Meta {
            mode: 0o755,
            uid: 0,
            gid: 0,
        };
        let before = with_files(&[("root/.profile", "")]);
        let mut after = before.clone();
        for path in [".cache/rosetta", "root/.cache/rosetta"] {
            after.set_dir(path, dir).unwrap();
        }
        restore_runtime_paths(&mut after, &before);
        assert!(after.differences(&before).is_empty());

        // A `.cache` that was there stays, without Rosetta's.
        let before = with_files(&[("root/.cache/pip/keep", "")]);
        let mut after = before.clone();
        after.set_dir("root/.cache/rosetta", dir).unwrap();
        restore_runtime_paths(&mut after, &before);
        assert!(after.differences(&before).is_empty());
    }

    #[test]
    fn picks_a_runtime_by_name() {
        assert_eq!("podman".parse(), Ok(Runtime::Podman));
        assert_eq!("docker".parse(), Ok(Runtime::Docker));
        assert!("buildah".parse::<Runtime>().is_err());
    }

    #[test]
    fn an_error_shows_dpkgs_reason_first() {
        let mut output = String::from("Setting up adduser ...\n");
        output.push_str("dpkg: error processing package adduser (--configure):\n");
        output.push_str(" end of file on stdin at conffile prompt\n");
        for _ in 0..60 {
            output.push_str(" adduser\n");
        }
        let summary = summary(&output);
        assert!(
            summary.starts_with("dpkg: error processing package adduser"),
            "{summary}"
        );
        assert_eq!(summary.lines().count(), 2 + OUTPUT_LINES);
        assert_eq!(super::summary("short\n"), "short");
    }
}
