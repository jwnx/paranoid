//! `paranoid lock`: resolve dependencies and write the lock file.

use std::path::PathBuf;

use clap::Parser;

use paranoid::config::Manifest;
use paranoid::lock::{LockDiff, LockFile};
use paranoid::resolve::ResolveService;

use super::diff::{self, Shown};
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

    /// Print what changed to stdout as JSON, as `paranoid diff --json`
    /// does: `"changed": false` when no package or image moved.
    #[arg(long)]
    json: bool,
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
        if args.json {
            diff::print_json(&LockDiff::default());
        }
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

    let diff = match &existing {
        Some(old) => old.diff(&lock),
        None => LockDiff::from_nothing(&lock),
    };
    if args.json {
        diff::print_json(&diff);
    }
    if !quiet {
        let packages = match lock.images.len() {
            1 => format!("{} packages", lock.packages.len()),
            n => format!("{} packages in {n} images", lock.packages.len()),
        };
        match &existing {
            None => eprintln!("({packages})"),
            Some(_) if diff.is_empty() => eprintln!("({packages}, no changes)"),
            Some(old) => eprint!("{}", diff::render(&diff, &Shown::of(&[old, &lock]))),
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
