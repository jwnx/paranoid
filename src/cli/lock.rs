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

    /// Write the lock even if some dependencies can't be satisfied.
    ///
    /// They are recorded in the lock, and `build` asks for the same flag.
    #[arg(long)]
    allow_unmet: bool,
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
        // A keyring we can't read is reported by the resolution below.
        && existing.is_current(&manifest).unwrap_or(false)
    {
        check_unmet(existing, args.allow_unmet)?;
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

    let lock = LockFile::from_resolved(&manifest, resolved);
    if lock.unmet().next().is_some() && !quiet {
        // Finish the progress line before the warning or error.
        eprintln!();
    }
    check_unmet(&lock, args.allow_unmet)?;
    lock.write(&output).map_err(CliError::lock)?;

    if !quiet {
        let changes = existing.as_ref().map(|old| old.diff(&lock));
        let packages = match lock.images.len() {
            1 => format!("{} packages", lock.packages.len()),
            n => format!("{} packages in {n} images", lock.packages.len()),
        };
        match changes.as_deref() {
            None => eprintln!("({packages})"),
            Some([]) => eprintln!("({packages}, no changes)"),
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

/// Fail on unmet dependencies unless they are allowed, in which case warn
/// about each one.
pub fn check_unmet(lock: &LockFile, allow: bool) -> Result<(), CliError> {
    // Name the image only when there's more than one.
    let several = lock.images.len() > 1;
    let unmet: Vec<String> = lock
        .unmet()
        .map(|(image, unmet)| {
            if several {
                format!("image '{image}': {unmet}")
            } else {
                unmet.to_string()
            }
        })
        .collect();
    if !allow && !unmet.is_empty() {
        return Err(CliError::unmet(&unmet));
    }
    for unmet in &unmet {
        st::warning(format!("unmet dependency: {unmet}"));
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
