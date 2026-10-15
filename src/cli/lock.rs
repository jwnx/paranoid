//! `paranoid lock`: resolve dependencies and write the lock file.

use std::path::PathBuf;

use clap::Parser;

use paranoid::config::Manifest;
use paranoid::lock::{Change, LockFile};
use paranoid::resolve::ResolveService;

use super::error::CliError;
use super::style as st;

#[derive(Parser)]
pub struct LockArgs {
    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Where to write the lock file.
    ///
    /// Defaults to `paranoid.lock` next to the manifest.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Re-resolve even if the lock file is already up to date.
    #[arg(long)]
    force: bool,
}

pub async fn run(args: LockArgs, quiet: bool) -> Result<(), CliError> {
    let manifest = Manifest::load(&args.manifest).map_err(CliError::manifest)?;
    let output = args
        .output
        .unwrap_or_else(|| Manifest::default_lock_path(&args.manifest));

    // The existing lock, for the up-to-date check and the diff.
    let existing = match output.try_exists() {
        Ok(true) => match LockFile::from_file(&output) {
            Ok(lock) => Some(lock),
            Err(e) => {
                st::warning(format!("ignoring unreadable {}: {e}", output.display()));
                None
            }
        },
        _ => None,
    };

    if !args.force
        && let Some(existing) = &existing
        && existing.matches_config(&manifest)
    {
        if !quiet {
            let hash = existing.content_hash().map_err(CliError::lock)?;
            eprintln!("Lockfile up to date.");
            st::footer(&output, &hash);
        }
        return Ok(());
    }

    if !quiet {
        eprint!("Resolving dependencies...");
    }
    let resolved = ResolveService::new()
        .map_err(CliError::resolve)?
        .resolve(&manifest)
        .await
        .map_err(CliError::resolve)?;
    if !quiet {
        eprint!(" Done ");
    }

    for dep in &resolved.unmet {
        st::warning(format!("unmet dependency: {dep}"));
    }

    let lock = LockFile::from_resolved(&manifest, resolved);
    lock.write(&output).map_err(CliError::lock)?;

    if !quiet {
        let changes = existing.as_ref().map(|old| old.diff(&lock));
        match changes.as_deref() {
            None => eprintln!("({} packages)", lock.packages.len()),
            Some([]) => eprintln!("({} packages, no changes)", lock.packages.len()),
            Some(changes) => {
                let count = |pred: fn(&Change) -> bool| changes.iter().filter(|c| pred(c)).count();
                eprintln!(
                    "(+{} ~{} -{})",
                    count(|c| matches!(c, Change::Added { .. })),
                    count(|c| matches!(c, Change::Changed { .. })),
                    count(|c| matches!(c, Change::Removed { .. })),
                );
                changes.iter().for_each(print_change);
            }
        }
        st::footer(&output, &lock.content_hash().map_err(CliError::lock)?);
    }

    Ok(())
}

fn print_change(change: &Change<'_>) {
    let (sigil, style, detail) = match change {
        Change::Added { version, .. } => ("+", st::added(), version.to_string()),
        Change::Changed { from, to, .. } => ("~", st::changed(), format!("{from} → {to}")),
        Change::Removed { version, .. } => ("-", st::removed(), version.to_string()),
    };
    eprintln!(
        "  {} {} {}",
        style.apply_to(sigil),
        style.apply_to(change.name()),
        detail
    );
}
