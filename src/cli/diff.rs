//! `paranoid diff`: show how two lock files differ, and the rendering of a
//! diff that `paranoid lock` shares.

use std::fmt::Write;
use std::path::PathBuf;

use clap::Parser;

use paranoid::lock::{Change, ChangeKind, FileChange, LockDiff, LockFile, LockedFile};

use super::error::CliError;
use super::style as st;

#[derive(Parser)]
pub struct DiffArgs {
    /// The older lock file.
    #[arg(value_name = "OLD")]
    old: PathBuf,

    /// The newer lock file.
    #[arg(value_name = "NEW")]
    new: PathBuf,

    /// Print the changes as JSON, in a format that stays stable across
    /// releases.
    #[arg(long)]
    json: bool,
}

pub fn run(args: DiffArgs) -> Result<(), CliError> {
    let load = |path: &PathBuf| LockFile::from_file(path).map_err(|e| CliError::read_lock(path, e));
    let (old, new) = (load(&args.old)?, load(&args.new)?);
    let diff = old.diff(&new);
    if args.json {
        print_json(&diff);
    } else if diff.is_empty() {
        println!("No changes.");
    } else {
        print!("{}", render(&diff, &Shown::of(&[&old, &new])));
    }
    Ok(())
}

/// Print a diff to stdout as JSON.
pub fn print_json(diff: &LockDiff<'_>) {
    println!(
        "{}",
        serde_json::to_string_pretty(diff).expect("a diff always serializes")
    );
}

/// What to name on each change: only what tells changes apart.
pub struct Shown {
    /// The images a change is in, when there's more than one.
    pub images: bool,
    /// The repository a change comes from, when there's more than one.
    pub repositories: bool,
}

impl Shown {
    /// Name images and repositories when any of these locks has several.
    pub fn of(locks: &[&LockFile]) -> Self {
        Self {
            images: locks.iter().any(|l| l.images.len() > 1),
            repositories: locks.iter().any(|l| l.repositories.len() > 1),
        }
    }
}

/// A diff for the terminal: counts, then a line per change.
pub fn render(diff: &LockDiff<'_>, shown: &Shown) -> String {
    let count = |pred: fn(Option<&str>, Option<&str>) -> bool| {
        let packages = diff.changes.iter().map(|c| c.kind.versions());
        let files = diff.files.iter().map(|f| {
            let (from, to) = (
                f.from.map(|f| f.name.as_str()),
                f.to.map(|f| f.name.as_str()),
            );
            (from, to)
        });
        packages.chain(files).filter(|&(a, b)| pred(a, b)).count()
    };
    let mut out = format!(
        "(+{} ~{} -{})\n",
        count(|from, _| from.is_none()),
        count(|from, to| from.is_some() && to.is_some()),
        count(|_, to| to.is_none()),
    );
    for image in &diff.added_images {
        let _ = writeln!(out, "  {} image {image}", st::added().apply_to("+"));
    }
    for image in &diff.removed_images {
        let _ = writeln!(out, "  {} image {image}", st::removed().apply_to("-"));
    }
    for change in &diff.changes {
        let _ = writeln!(out, "{}", render_change(change, shown));
    }
    for change in &diff.files {
        let _ = writeln!(out, "{}", render_file(change, shown));
    }
    out
}

fn render_file(change: &FileChange<'_>, shown: &Shown) -> String {
    // The last part of the URL usually names the release; when that's the
    // same before and after, the hash is what changed.
    let named = |file: &LockedFile| {
        let url = file.url.path().trim_end_matches('/');
        url.rsplit('/').next().unwrap_or(url).to_string()
    };
    let hashed = |file: &LockedFile| format!("{} ({:.12})", named(file), file.sha256.to_string());
    let (sigil, style, detail) = match (change.from, change.to) {
        (None, Some(to)) => ("+", st::added(), named(to)),
        (Some(from), None) => ("-", st::removed(), named(from)),
        (Some(from), Some(to)) if named(from) == named(to) => (
            "~",
            st::changed(),
            format!("{} → {}", hashed(from), hashed(to)),
        ),
        (Some(from), Some(to)) => (
            "~",
            st::changed(),
            format!("{} → {}", named(from), named(to)),
        ),
        (None, None) => unreachable!("a file change has a before or an after"),
    };
    let images = if shown.images {
        format!(
            " {}",
            st::dim().apply_to(format!("in {}", change.images.join(", ")))
        )
    } else {
        String::new()
    };
    format!(
        "  {} file {} {detail}{images}",
        style.apply_to(sigil),
        style.apply_to(change.name),
    )
}

fn render_change(change: &Change<'_>, shown: &Shown) -> String {
    let (sigil, style, detail) = match &change.kind {
        ChangeKind::Added { version } => ("+", st::added(), version.to_string()),
        ChangeKind::Changed { from, to } => ("~", st::changed(), format!("{from} → {to}")),
        ChangeKind::Removed { version } => ("-", st::removed(), version.to_string()),
    };
    let mut notes = Vec::new();
    if shown.repositories {
        notes.push(format!("from {}", change.repository));
    }
    if shown.images {
        notes.push(format!("in {}", change.images.join(", ")));
    }
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(" {}", st::dim().apply_to(notes.join(", ")))
    };
    format!(
        "  {} {} {detail}{notes}",
        style.apply_to(sigil),
        style.apply_to(change.name),
    )
}
