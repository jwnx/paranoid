//! `paranoid lock`: resolve dependencies and write the lock file.

use std::path::{Path, PathBuf};

use clap::Parser;

use paranoid::config::{ConfigFormat, Manifest, MovedSnapshot, SnapshotTime, set_snapshots};
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

    /// Move every repository that has a `snapshot` to TIME and re-lock.
    ///
    /// TIME is `now` or a time like 20261020T000000Z. The manifest is
    /// rewritten only once the new lock resolves.
    #[arg(long, value_name = "TIME", value_parser = parse_snapshot)]
    snapshot: Option<SnapshotTime>,
}

/// `--snapshot`: `now`, or a snapshot time that isn't in the future.
fn parse_snapshot(s: &str) -> Result<SnapshotTime, String> {
    let now = SnapshotTime::now();
    if s == "now" {
        return Ok(now);
    }
    let time: SnapshotTime = s.parse()?;
    if time > now {
        return Err(format!("{time} is in the future"));
    }
    Ok(time)
}

/// The manifest's text with its snapshots moved, to write once the lock
/// resolves.
struct Rewrite {
    path: PathBuf,
    time: SnapshotTime,
    text: String,
    moved: Vec<MovedSnapshot>,
}

impl Rewrite {
    /// Move the snapshots of the manifest at `path` to `time`, in memory.
    fn new(path: &Path, time: SnapshotTime) -> Result<Self, CliError> {
        if !matches!(Manifest::detect_format(path), Ok(ConfigFormat::Toml)) {
            return Err(CliError::config(format!(
                "--snapshot can only rewrite a TOML manifest, and {} isn't one",
                path.display()
            )));
        }
        let content = std::fs::read_to_string(path).map_err(CliError::manifest)?;
        let (text, moved) = set_snapshots(&content, time).map_err(CliError::manifest)?;
        Ok(Self {
            path: path.to_path_buf(),
            time,
            text,
            moved,
        })
    }

    fn save(&self) -> Result<(), CliError> {
        if self.moved.is_empty() {
            return Ok(());
        }
        std::fs::write(&self.path, &self.text)
            .map_err(|e| CliError::config(format!("could not write {}: {e}", self.path.display())))
    }
}

pub async fn run(args: LockArgs, quiet: bool) -> Result<(), CliError> {
    let path = Manifest::locate(&args.manifest).map_err(CliError::manifest)?;
    let rewrite = args
        .snapshot
        .map(|time| Rewrite::new(&path, time))
        .transpose()?;
    let manifest = match &rewrite {
        Some(rewrite) => Manifest::from_file_content(&path, &rewrite.text),
        None => Manifest::from_file(&path),
    }
    .map_err(CliError::manifest)?;
    if let Some(rewrite) = &rewrite {
        if !manifest.repositories.values().any(|r| r.snapshot.is_some()) {
            return Err(CliError::config(format!(
                "--snapshot moves the repositories that have a `snapshot`, and {} has none",
                path.display()
            )));
        }
        if !quiet && !rewrite.moved.is_empty() {
            let names: Vec<_> = rewrite
                .moved
                .iter()
                .map(|m| m.repository.as_str())
                .collect();
            eprintln!("Moving {} to snapshot {}.", names.join(", "), rewrite.time);
        }
    }
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
        if let Some(rewrite) = &rewrite {
            rewrite.save()?;
        }
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
    if let Some(rewrite) = &rewrite {
        rewrite.save()?;
    }
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
