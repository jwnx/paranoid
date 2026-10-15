//! `paranoid build`: build an OCI image from the manifest and its lock file.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::Parser;

use paranoid::build::{BuildOptions, build};
use paranoid::config::Manifest;
use paranoid::download::DownloadConfig;
use paranoid::lock::LockFile;
use paranoid::oci::{ImageOutput, Timestamp};

use super::error::CliError;
use super::progress::DotsReporter;
use super::style as st;

#[derive(Parser)]
pub struct BuildArgs {
    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Lock file to use (must exist; run `paranoid lock` first).
    ///
    /// Defaults to `paranoid.lock` next to the manifest.
    #[arg(long, value_name = "FILE")]
    lock: Option<PathBuf>,

    /// Output path for the OCI image.
    ///
    /// Ends with `.tar` → writes a tarball loadable by `podman load` / `docker load`.
    /// Otherwise → writes an OCI image layout directory.
    #[arg(short, long, value_name = "PATH", default_value = "image.tar")]
    output: PathBuf,

    /// Directory to hold downloaded packages (defaults to system cache).
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// Re-download packages even if they are cached.
    #[arg(long)]
    force_download: bool,

    /// Maximum number of parallel downloads.
    #[arg(long, value_name = "N", default_value = "4")]
    jobs: NonZeroUsize,
}

pub async fn run(args: BuildArgs, quiet: bool) -> Result<(), CliError> {
    let manifest = Manifest::load(&args.manifest).map_err(CliError::manifest)?;
    let lock_path = args
        .lock
        .unwrap_or_else(|| Manifest::default_lock_path(&args.manifest));

    if !lock_path.exists() {
        return Err(CliError::missing_lock(&lock_path));
    }
    let lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;
    match lock.is_current(&manifest) {
        Ok(true) => {}
        Ok(false) => st::warning(format!(
            "{} may be stale — run `paranoid lock` first",
            lock_path.display()
        )),
        Err(e) => st::warning(format!(
            "could not check {} against the manifest: {e}",
            lock_path.display()
        )),
    }

    let options = BuildOptions {
        download: DownloadConfig {
            jobs: args.jobs,
            force: args.force_download,
        },
        cache_dir: args.cache_dir,
        output: ImageOutput::from_path(&args.output),
        created: source_date_epoch()?,
    };
    // Failures of individual packages are reported as they happen; the
    // returned error summarizes them.
    let report = build(&manifest, &lock, &options, &DotsReporter::new(quiet))
        .await
        .map_err(CliError::build)?;

    if !quiet {
        eprintln!("Done.");
        eprintln!();

        let output = options.output.path();
        let name = match manifest.image.as_ref().and_then(|i| i.name.as_ref()) {
            Some(_) => report.image.reference.clone(),
            None => output.display().to_string(),
        };
        eprintln!(
            "Built {} with {} packages ({} fresh, {} cached) sized {}.",
            name,
            lock.packages.len(),
            report.fresh,
            report.cached,
            st::fmt_size(report.image.total_size),
        );
        st::footer(output, &report.image.manifest_digest.to_string());
    }

    Ok(())
}

/// The image creation time: `SOURCE_DATE_EPOCH` when set, the Unix epoch
/// otherwise, so that building the same lock twice yields the same digest.
///
/// See <https://reproducible-builds.org/docs/source-date-epoch/>.
fn source_date_epoch() -> Result<Timestamp, CliError> {
    let Ok(raw) = std::env::var("SOURCE_DATE_EPOCH") else {
        return Ok(Timestamp::epoch());
    };
    raw.trim()
        .parse()
        .ok()
        .and_then(Timestamp::from_unix)
        .ok_or_else(|| CliError::config(format!("invalid SOURCE_DATE_EPOCH '{raw}'")))
}
