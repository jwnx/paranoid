//! `paranoid build`: build an OCI image from the manifest and its lock file.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::Parser;

use paranoid::build::IMAGE_FORMAT;
use paranoid::build::{BuildOptions, build};
use paranoid::config::Manifest;
use paranoid::configure::Runtime;
use paranoid::download::DownloadConfig;
use paranoid::lock::{ExpectedDigest, LockFile, Unchecked};
use paranoid::oci::{ImageOutput, Timestamp};

use super::error::CliError;
use super::lock::check_unmet;
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
    /// Otherwise → writes an OCI image layout directory. `{image}` is
    /// replaced by the image's id, and is required when building several.
    /// Defaults to the manifest's `output`, then to `image.tar`, or
    /// `{image}.tar` for a set.
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Build only this image of a set; repeat for several. Defaults to
    /// every image.
    #[arg(long = "image", value_name = "ID")]
    images: Vec<String>,

    /// Directory to hold downloaded packages (defaults to system cache).
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// What runs an image's maintainer scripts when it asks to be
    /// configured: podman or docker. Defaults to the first on PATH.
    #[arg(long, value_name = "podman|docker")]
    runtime: Option<Runtime>,

    /// Configure each configured image twice, and fail naming every path
    /// that came out different.
    #[arg(long)]
    check_reproducible: bool,

    /// Record each image's digest in the lock, for later builds to check
    /// theirs against, replacing one recorded for the same inputs.
    #[arg(long)]
    record: bool,

    /// Re-download packages even if they are cached.
    #[arg(long)]
    force_download: bool,

    /// Build even if the lock is stale: the manifest changed since it was
    /// written, or the keys that signed it are no longer trusted.
    #[arg(long)]
    allow_stale: bool,

    /// Build even though the lock leaves dependencies unmet.
    #[arg(long)]
    allow_unmet: bool,

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
    let mut lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;
    match lock.is_current(&manifest) {
        Ok(true) => {}
        // A digest from a stale lock would be one no later build checks.
        Ok(false) | Err(_) if args.record => {
            return Err(CliError::config(format!(
                "--record needs a lock that's current, and {} isn't; run `paranoid lock` first",
                lock_path.display()
            )));
        }
        Ok(false) if args.allow_stale => {
            st::warning(format!("{} is stale", lock_path.display()));
        }
        Ok(false) => return Err(CliError::stale(&lock_path)),
        // Without the keys there's no telling whether the lock is current,
        // which is the same question --allow-stale answers.
        Err(e) if args.allow_stale => st::warning(format!(
            "could not check {} against the manifest: {e}",
            lock_path.display()
        )),
        Err(e) => return Err(CliError::lock(e)),
    }
    check_unmet(&lock, args.allow_unmet)?;

    let images: Vec<String> = if args.images.is_empty() {
        manifest.images.keys().cloned().collect()
    } else {
        args.images
    };
    if let Some(unknown) = images.iter().find(|id| !manifest.images.contains_key(*id)) {
        let known: Vec<&str> = manifest.images.keys().map(String::as_str).collect();
        return Err(CliError::config(format!(
            "no image '{unknown}' in the manifest (it has {})",
            known.join(", ")
        )));
    }
    // `-o` wins, then each image's `output`, then `{image}.tar` here.
    let default = PathBuf::from(if manifest.is_set() {
        "{image}.tar"
    } else {
        "image.tar"
    });
    let template = args.output.as_ref();
    if let Some(template) = template
        && images.len() > 1
        && !template.to_string_lossy().contains("{image}")
    {
        return Err(CliError::output_template(template));
    }
    let output_for = |id: &str| match template {
        Some(t) => PathBuf::from(t.to_string_lossy().replace("{image}", id)),
        None => manifest
            .output(id)
            .unwrap_or_else(|| PathBuf::from(default.to_string_lossy().replace("{image}", id))),
    };
    let created = source_date_epoch()?;
    let epoch = created.as_datetime().timestamp();
    let mut changed = false;

    for id in &images {
        let output = output_for(id);
        let inputs = paranoid::build::inputs(&manifest, &lock, id).map_err(CliError::build)?;
        let previous = lock.image(id).and_then(|image| image.expected.clone());
        let unchecked = previous
            .as_ref()
            .and_then(|previous| previous.unchecked(&inputs, epoch));
        let expected = previous
            .as_ref()
            .filter(|_| unchecked.is_none())
            .map(|previous| previous.digest);
        let options = BuildOptions {
            download: DownloadConfig {
                jobs: args.jobs,
                force: args.force_download,
                ..DownloadConfig::default()
            },
            cache_dir: args.cache_dir.clone(),
            output: ImageOutput::from_path(&output),
            created: created.clone(),
            runtime: args.runtime,
            check_reproducible: args.check_reproducible,
            expected: expected.filter(|_| !args.record),
        };
        if !quiet && images.len() > 1 {
            eprintln!("== {id}");
        }
        // Failures of individual packages are reported as they happen; the
        // returned error summarizes them.
        let reporter = DotsReporter::new(quiet);
        let report = build(&manifest, &lock, id, &options, &reporter)
            .await
            .map_err(|e| {
                reporter.interrupt();
                CliError::build(e)
            })?;
        let digest = report.image.manifest_digest;
        if args.record {
            let current = ExpectedDigest {
                digest,
                format: Some(IMAGE_FORMAT),
                paranoid: env!("CARGO_PKG_VERSION").to_string(),
                epoch,
                inputs,
            };
            // A lock holds one digest per image, so say what goes.
            match &previous {
                Some(previous) if previous.digest != digest && expected.is_some() => {
                    st::warning(format!(
                        "{id}: replacing {} recorded for the same inputs",
                        previous.digest
                    ));
                }
                Some(previous) if previous.digest != digest => st::warning(format!(
                    "{id}: replacing {}, recorded by paranoid {} at epoch {}",
                    previous.digest, previous.paranoid, previous.epoch
                )),
                _ => {}
            }
            if previous.as_ref() != Some(&current) {
                let image = lock
                    .images
                    .iter_mut()
                    .find(|image| &image.name == id)
                    .expect("built images are in the lock");
                image.expected = Some(current);
                // Each image's, as it's built, so a later image failing
                // doesn't lose the earlier ones'.
                lock.write(&lock_path).map_err(CliError::lock)?;
                changed = true;
            }
        }

        if !quiet {
            eprintln!("Done.");
            eprintln!();

            let name = match &manifest.images[id].name {
                Some(_) => report.image.reference.clone(),
                None => output.display().to_string(),
            };
            let contents = match lock.image(id) {
                Some(image) if !image.files.is_empty() => format!(
                    "{} packages and {} file{}",
                    image.packages.len(),
                    image.files.len(),
                    if image.files.len() == 1 { "" } else { "s" },
                ),
                image => format!("{} packages", image.map_or(0, |i| i.packages.len())),
            };
            // With stores, say how many they served, so a store that
            // never has anything doesn't go unnoticed.
            let from_stores = if manifest.stores.is_empty() {
                String::new()
            } else {
                format!(", {} of them from a store", report.from_stores)
            };
            eprintln!(
                "Built {} with {} ({} fresh{}, {} cached) sized {}.",
                name,
                contents,
                report.fresh,
                from_stores,
                report.cached,
                st::fmt_size(report.image.total_size),
            );
            if !args.record {
                let why = match (&previous, unchecked) {
                    (Some(_), None) => "The digest matches the one the lock records.".to_string(),
                    (Some(previous), Some(Unchecked::Format)) => match previous.format {
                        Some(format) => format!(
                            "The lock's digest is for image format {format}, and this paranoid \
                             writes {IMAGE_FORMAT}, so it wasn't checked."
                        ),
                        None => format!(
                            "The lock's digest was recorded by paranoid {}, before image \
                             formats, so it wasn't checked; record it again.",
                            previous.paranoid
                        ),
                    },
                    (Some(previous), Some(Unchecked::Epoch)) => format!(
                        "The lock's digest is for SOURCE_DATE_EPOCH {}, not {epoch}, so it \
                         wasn't checked.",
                        previous.epoch
                    ),
                    (Some(_), Some(Unchecked::Inputs)) => {
                        "The lock's digest is for other inputs, so it wasn't checked.".to_string()
                    }
                    (None, _) => "The lock records no digest for this build to check.".to_string(),
                };
                eprintln!("{why}");
            }
            st::footer(&output, &digest.to_string());
        }
    }
    if changed && !quiet {
        eprintln!("Recorded the digests in {}.", lock_path.display());
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
