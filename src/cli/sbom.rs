//! `paranoid sbom`: describe an image of the lock as an SBOM.

use std::fs;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use paranoid::config::Manifest;
use paranoid::lock::LockFile;
use paranoid::sbom::{Format, Sbom};

use super::error::CliError;

#[derive(Parser)]
pub struct SbomArgs {
    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Lock file to describe. Defaults to `paranoid.lock` next to the
    /// manifest.
    #[arg(long, value_name = "FILE")]
    lock: Option<PathBuf>,

    /// The SBOM format, as JSON.
    #[arg(long, value_enum, default_value_t = SbomFormat::Spdx)]
    format: SbomFormat,

    /// The image of a set to describe. A single image needs none.
    #[arg(long, value_name = "ID")]
    image: Option<String>,

    /// Where to write the SBOM. Defaults to stdout.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SbomFormat {
    /// SPDX 2.3.
    Spdx,
    /// CycloneDX 1.6.
    Cyclonedx,
}

pub fn run(args: SbomArgs) -> Result<(), CliError> {
    let manifest = Manifest::load(&args.manifest).map_err(CliError::manifest)?;
    let lock_path = args
        .lock
        .unwrap_or_else(|| Manifest::default_lock_path(&args.manifest));
    if !lock_path.exists() {
        return Err(CliError::missing_lock(&lock_path));
    }
    let lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;
    // A stale lock describes some other image than the manifest's.
    if !lock.is_current(&manifest).map_err(CliError::lock)? {
        return Err(CliError::stale(&lock_path));
    }

    let image = match args.image {
        Some(image) => image,
        None if manifest.images.len() == 1 => manifest.images.keys().next().unwrap().clone(),
        None => {
            let known: Vec<&str> = manifest.images.keys().map(String::as_str).collect();
            return Err(CliError::config(format!(
                "an SBOM describes one image: pick one with --image ({})",
                known.join(", ")
            )));
        }
    };
    if !manifest.images.contains_key(&image) {
        let known: Vec<&str> = manifest.images.keys().map(String::as_str).collect();
        return Err(CliError::config(format!(
            "no image '{image}' in the manifest (it has {})",
            known.join(", ")
        )));
    }

    let sbom = Sbom::new(&manifest, &lock, &image).map_err(CliError::sbom)?;
    let json = sbom.to_json(match args.format {
        SbomFormat::Spdx => Format::Spdx,
        SbomFormat::Cyclonedx => Format::CycloneDx,
    });
    match &args.output {
        Some(path) => fs::write(path, json + "\n").map_err(|e| CliError::write(path, e))?,
        None => println!("{json}"),
    }
    Ok(())
}
