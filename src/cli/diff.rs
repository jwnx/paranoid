//! `paranoid diff`: show how two lock files differ, and the rendering of a
//! diff that `paranoid lock` shares.

use std::fmt::Write;
use std::path::PathBuf;

use clap::Parser;

use paranoid::lock::{Change, ChangeKind, LockDiff, LockFile};

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
    let count =
        |pred: fn(&ChangeKind) -> bool| diff.changes.iter().filter(|c| pred(&c.kind)).count();
    let mut out = format!(
        "(+{} ~{} -{})\n",
        count(|k| matches!(k, ChangeKind::Added { .. })),
        count(|k| matches!(k, ChangeKind::Changed { .. })),
        count(|k| matches!(k, ChangeKind::Removed { .. })),
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
    out
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
