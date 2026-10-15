//! `paranoid inspect`: show what's in a lock file or a built image.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use paranoid::lock::LockFile;
use paranoid::oci::read_image;

use super::error::CliError;
use super::style as st;

#[derive(Parser)]
pub struct InspectArgs {
    /// What to inspect.
    #[command(subcommand)]
    target: InspectTarget,
}

#[derive(Subcommand)]
enum InspectTarget {
    /// Show packages in a lock file.
    Lock {
        /// Lock file to inspect.
        #[arg(value_name = "FILE", default_value = "paranoid.lock")]
        path: PathBuf,

        /// Show dependency tree instead of flat table.
        #[arg(long)]
        tree: bool,
    },

    /// Show the contents of a built OCI image (tar or directory layout).
    Image {
        /// Path to the OCI image (`.tar` tarball or OCI layout directory).
        #[arg(value_name = "PATH", default_value = "image.tar")]
        path: PathBuf,
    },
}

pub fn run(args: InspectArgs) -> Result<(), CliError> {
    match args.target {
        InspectTarget::Lock { path, tree } => {
            let lock = LockFile::from_file(&path).map_err(CliError::lock)?;
            print_lock_summary(&lock);
            if tree {
                print_lock_tree(&lock);
            } else {
                print_lock_table(&lock);
            }
            println!();
            st::footer_stdout(&path, &lock.content_hash().map_err(CliError::lock)?);
            Ok(())
        }
        InspectTarget::Image { path } => inspect_image(&path),
    }
}

// ── lock ─────────────────────────────────────────────────────────────────────

fn print_lock_summary(lock: &LockFile) {
    let total = lock.packages.len();
    let direct = lock.packages.iter().filter(|p| p.direct).count();
    let size = st::fmt_size(lock.packages.iter().map(|p| p.size).sum());

    println!("Inspect Lockfile");
    if direct < total {
        println!(
            "{total} packages, {direct} direct + {} transitive ({size})",
            total - direct
        );
    } else {
        println!("{total} packages ({size})");
    }
    for repo in &lock.repositories {
        let signers: Vec<_> = repo.signed_by.iter().map(ToString::to_string).collect();
        println!(
            "{}",
            st::dim().apply_to(format!(
                "{}: Release of {}, InRelease {}, signed by {}",
                repo.name,
                repo.date.format("%Y-%m-%d %H:%M:%S UTC"),
                repo.inrelease,
                signers.join(", ")
            ))
        );
    }
    println!();
}

/// Packages as a table: direct ones first (bold), then transitive (dimmed).
fn print_lock_table(lock: &LockFile) {
    const W_NAME: usize = 28;
    const W_VER: usize = 20;
    const W_ARCH: usize = 8;
    const W_SIZE: usize = 8;

    let row = |name: &str, version: &str, arch: &str, size: &str| {
        format!("{name:<W_NAME$} {version:<W_VER$} {arch:<W_ARCH$} {size:>W_SIZE$}")
    };
    println!(
        "{}",
        st::dim().apply_to(row("Package", "Version", "Arch", "Size"))
    );

    let mut packages: Vec<_> = lock.packages.iter().collect();
    packages.sort_by(|a, b| b.direct.cmp(&a.direct).then(a.name.cmp(&b.name)));
    for pkg in packages {
        let line = row(
            &st::truncate(&pkg.name, W_NAME),
            &st::truncate(&pkg.version, W_VER),
            pkg.architecture.as_str(),
            &st::fmt_size(pkg.size),
        );
        let style = if pkg.direct { st::bold() } else { st::dim() };
        println!("{}", style.apply_to(line));
    }
}

/// Packages as a dependency tree rooted at the direct packages. A package
/// already shown elsewhere is marked `(*)` and not expanded again.
fn print_lock_tree(lock: &LockFile) {
    let tree = Tree {
        info: lock
            .packages
            .iter()
            .map(|p| (p.name.as_str(), (p.version.as_str(), p.size)))
            .collect(),
        children: lock
            .deps
            .iter()
            .map(|e| {
                let mut children: Vec<_> = e.children.iter().map(String::as_str).collect();
                children.sort();
                (e.parent.as_str(), children)
            })
            .collect(),
    };

    let mut roots: Vec<&str> = lock
        .packages
        .iter()
        .filter(|p| p.direct)
        .map(|p| p.name.as_str())
        .collect();
    roots.sort();

    let mut seen = HashSet::new();
    for (i, root) in roots.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{} {}", st::bold().apply_to(root), tree.meta(root));
        seen.insert(*root);
        tree.print_children(root, "", &mut seen);
    }
}

struct Tree<'a> {
    /// name → (version, size)
    info: HashMap<&'a str, (&'a str, u64)>,
    /// name → sorted direct dependencies
    children: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Tree<'a> {
    fn meta(&self, name: &str) -> impl std::fmt::Display {
        let (version, size) = self.info.get(name).copied().unwrap_or(("?", 0));
        st::dim().apply_to(format!("{version} ({})", st::fmt_size(size)))
    }

    fn print_children(&self, parent: &str, prefix: &str, seen: &mut HashSet<&'a str>) {
        let Some(children) = self.children.get(parent) else {
            return;
        };
        for (i, &child) in children.iter().enumerate() {
            let last = i + 1 == children.len();
            let (branch, indent) = if last {
                ("└── ", "    ")
            } else {
                ("├── ", "│   ")
            };

            let first_time = seen.insert(child);
            let name = if first_time {
                st::added().apply_to(child).to_string()
            } else {
                format!(
                    "{} {}",
                    st::changed().apply_to(child),
                    st::dim().apply_to("(*)")
                )
            };
            println!(
                "{prefix}{}{name} {}",
                st::dim().apply_to(branch),
                self.meta(child)
            );

            if first_time {
                self.print_children(child, &format!("{prefix}{indent}"), seen);
            }
        }
    }
}

// ── image ────────────────────────────────────────────────────────────────────

fn inspect_image(path: &Path) -> Result<(), CliError> {
    const LABEL_W: usize = 13;
    const TOTAL_W: usize = 60;

    let image = read_image(path).map_err(CliError::inspect_image)?;
    let container = image.config.config.as_ref();
    let label = |key: &str| {
        container
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get(key))
            .cloned()
    };

    let rows: [(&str, Option<String>); 7] = [
        ("Name", image.reference.clone()),
        ("Architecture", Some(image.config.architecture.to_string())),
        (
            "Created At",
            image
                .config
                .created
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| label("org.opencontainers.image.created")),
        ),
        ("Revision", label("org.opencontainers.image.revision")),
        (
            "Entrypoint",
            container
                .and_then(|c| c.entrypoint.as_ref())
                .map(|e| e.join(" ")),
        ),
        (
            "Cmd",
            container.and_then(|c| c.cmd.as_ref()).map(|c| c.join(" ")),
        ),
        (
            "WorkingDir",
            container
                .and_then(|c| c.working_dir.as_ref())
                .map(|w| w.as_str().to_string()),
        ),
    ];

    println!("Inspect Image");
    println!();
    for (name, value) in rows {
        if let Some(value) = value {
            st::dots_row(name, &value, LABEL_W, TOTAL_W);
        }
    }
    st::dots_row("Size", &st::fmt_size(image.layers_size), LABEL_W, TOTAL_W);
    println!();
    st::footer_stdout(path, &image.manifest_digest.to_string());

    Ok(())
}
