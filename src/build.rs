//! Building an image from a manifest and its lock file.
//!
//! The pipeline is: download every locked package into the cache, apply them
//! to an in-memory [`Rootfs`] in lock order, record them in the dpkg database
//! (`/var/lib/dpkg`), and write a single-layer OCI image from it.

use std::path::PathBuf;

use thiserror::Error;

use crate::config::Manifest;
use crate::deb::{DebArchive, DebError};
use crate::download::{
    DownloadConfig, DownloadError, DownloadFailures, DownloadService, Fetched, PackageCache,
};
use crate::dpkg::{DpkgDatabase, DpkgError, PackageState};
use crate::lock::LockFile;
use crate::oci::{ImageOutput, OciConfig, OciConfigError, OciError, OciImageResult, Timestamp};
use crate::report::{Event, Phase, Reporter};
use crate::rootfs::{Rootfs, RootfsError};

/// How to build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub download: DownloadConfig,
    /// Cache directory; the user cache directory when `None`.
    pub cache_dir: Option<PathBuf>,
    pub output: ImageOutput,
    /// Image creation time. Pin it (e.g. from `SOURCE_DATE_EPOCH`) for
    /// reproducible images.
    pub created: Timestamp,
}

/// What a build produced.
#[derive(Debug, Clone)]
pub struct BuildReport {
    pub image: OciImageResult,
    /// Packages downloaded during this build.
    pub fresh: usize,
    /// Packages served from the cache.
    pub cached: usize,
    /// Entries in the assembled rootfs.
    pub entries: usize,
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("invalid image configuration")]
    Config(#[from] OciConfigError),

    #[error("failed to set up the package cache")]
    Cache(#[from] DownloadError),

    #[error(transparent)]
    Download(#[from] DownloadFailures),

    #[error("cached package {package} failed verification")]
    Verify {
        package: String,
        #[source]
        source: DownloadError,
    },

    #[error("failed to install {package}")]
    Install {
        package: String,
        #[source]
        source: RootfsError,
    },

    #[error("failed to read the control files of {package}")]
    Control {
        package: String,
        #[source]
        source: DebError,
    },

    #[error("failed to record {package} in the dpkg database")]
    Dpkg {
        package: String,
        #[source]
        source: DpkgError,
    },

    #[error("failed to write the dpkg database")]
    DpkgDatabase(#[source] RootfsError),

    #[error("failed to write image")]
    Image(#[from] OciError),
}

/// Build the image a manifest describes, from the packages in `lock`.
pub async fn build(
    manifest: &Manifest,
    lock: &LockFile,
    options: &BuildOptions,
    reporter: &dyn Reporter,
) -> Result<BuildReport, BuildError> {
    // Validate the image configuration before doing any work.
    let config = OciConfig::from_manifest(manifest, options.created.clone())?;

    let cache = match &options.cache_dir {
        Some(dir) => PackageCache::with_base_dir(dir.clone())?,
        None => PackageCache::new()?,
    };
    let downloader = DownloadService::with_cache(cache, options.download.clone())?;
    let fetched = downloader
        .download_all(lock, &manifest.repositories, reporter)
        .await?;

    let state = if manifest.image.as_ref().is_some_and(|i| i.unconfigured) {
        PackageState::Unpacked
    } else {
        PackageState::Installed
    };
    let rootfs = assemble(&fetched, state, reporter)?;
    let image = crate::oci::write_image(&config, &rootfs, &options.output, reporter)?;

    let cached = fetched.iter().filter(|f| f.from_cache).count();
    Ok(BuildReport {
        image,
        fresh: fetched.len() - cached,
        cached,
        entries: rootfs.len(),
    })
}

/// Apply packages to a fresh rootfs in the given (lock) order, verifying
/// each one against the lock as it's read.
pub fn assemble(
    packages: &[Fetched<'_>],
    state: PackageState,
    reporter: &dyn Reporter,
) -> Result<Rootfs, BuildError> {
    reporter.report(Event::Begin {
        phase: Phase::Assemble,
        total: packages.len() as u64,
    });

    let mut rootfs = Rootfs::new();
    let mut dpkg = DpkgDatabase::new(state);
    for fetched in packages {
        let name = &fetched.package.name;
        let result = PackageCache::read(&fetched.path, fetched.package)
            .map_err(|source| BuildError::Verify {
                package: name.clone(),
                source,
            })
            .and_then(|deb| {
                let stats = rootfs
                    .apply_deb(&deb)
                    .map_err(|source| BuildError::Install {
                        package: name.clone(),
                        source,
                    })?;
                let control = DebArchive::new(&deb)
                    .and_then(|deb| deb.control())
                    .map_err(|source| BuildError::Control {
                        package: name.clone(),
                        source,
                    })?;
                dpkg.add(control, &stats.paths, &mut rootfs)
                    .map_err(|source| BuildError::Dpkg {
                        package: name.clone(),
                        source,
                    })
            });

        if let Err(error) = result {
            reporter.report(Event::Failed {
                phase: Phase::Assemble,
                item: name,
                error: &error,
            });
            return Err(error);
        }
        reporter.report(Event::Advance {
            phase: Phase::Assemble,
            units: 1,
            item: name,
        });
    }

    let database = dpkg
        .to_tar()
        .map_err(|e| BuildError::DpkgDatabase(e.into()))?;
    rootfs
        .apply_tar(database.as_slice())
        .map_err(BuildError::DpkgDatabase)?;

    reporter.report(Event::End {
        phase: Phase::Assemble,
        summary: rootfs.len() as u64,
    });
    Ok(rootfs)
}
