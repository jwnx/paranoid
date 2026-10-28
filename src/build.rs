//! Building an image from a manifest and its lock file.
//!
//! The pipeline is: download every locked package and file into the cache,
//! apply the packages to an in-memory [`Rootfs`] in lock order, record them
//! in the dpkg database (`/var/lib/dpkg`), place the files, and write a
//! single-layer OCI image from it.

use std::path::PathBuf;

use thiserror::Error;

use crate::config::{FileSpec, Manifest};
use crate::deb::{DebArchive, DebError};
use crate::download::{
    DownloadConfig, DownloadError, DownloadFailures, DownloadService, Fetched, Origin,
    PackageCache, Sources,
};
use crate::dpkg::{DpkgDatabase, DpkgError, PackageState};
use crate::files::PlaceError;
use crate::lock::{LockFile, LockedFile};
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
    /// Packages and files downloaded during this build.
    pub fresh: usize,
    /// How many of the fresh ones came from a store.
    pub from_stores: usize,
    /// Packages and files served from the cache.
    pub cached: usize,
    /// Entries in the assembled rootfs.
    pub entries: usize,
}

#[derive(Debug, Error)]
pub enum BuildError {
    #[error("no image '{0}' in the manifest and the lock")]
    UnknownImage(String),

    #[error("the lock has file '{0}', which the manifest doesn't")]
    UnknownFile(String),

    #[error("invalid image configuration")]
    Config(#[from] OciConfigError),

    #[error("failed to set up the package cache")]
    Cache(#[from] DownloadError),

    #[error(transparent)]
    Download(#[from] DownloadFailures),

    #[error("failed to place file {file}")]
    Place {
        file: String,
        #[source]
        source: PlaceError,
    },

    #[error("the cached copy of {package} failed verification")]
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

/// Build one image of a manifest, by id, from its packages in `lock`.
pub async fn build(
    manifest: &Manifest,
    lock: &LockFile,
    image: &str,
    options: &BuildOptions,
    reporter: &dyn Reporter,
) -> Result<BuildReport, BuildError> {
    let unknown = || BuildError::UnknownImage(image.to_string());
    let spec = manifest.images.get(image).ok_or_else(unknown)?;
    let locked = lock.image(image).ok_or_else(unknown)?;
    let packages = lock.image_packages(locked);
    let files: Vec<(&LockedFile, &FileSpec)> = lock
        .image_files(locked)
        .into_iter()
        .map(|file| {
            let spec = manifest.files.get(&file.name);
            spec.map(|spec| (file, spec))
                .ok_or_else(|| BuildError::UnknownFile(file.name.clone()))
        })
        .collect::<Result<_, _>>()?;

    // Validate the image configuration before doing any work.
    let config = OciConfig::from_image(spec, manifest.architecture, options.created.clone())?;

    let cache = match &options.cache_dir {
        Some(dir) => PackageCache::with_base_dir(dir.clone())?,
        None => PackageCache::new()?,
    };
    let downloader = DownloadService::with_cache(cache, options.download.clone())?;
    let fetched = downloader
        .download_all(&packages, Sources::of(manifest), reporter)
        .await?;
    let locked_files: Vec<&LockedFile> = files.iter().map(|(file, _)| *file).collect();
    let fetched_files = if locked_files.is_empty() {
        Vec::new()
    } else {
        downloader
            .download_all(&locked_files, Sources::of(manifest), reporter)
            .await?
    };

    let state = if spec.unconfigured {
        PackageState::Unpacked
    } else {
        PackageState::Installed
    };
    let mut rootfs = assemble(&fetched, state, reporter)?;
    for (fetched, (_, spec)) in fetched_files.iter().zip(&files) {
        place(&mut rootfs, fetched, spec)?;
    }
    let image = crate::oci::write_image(&config, &rootfs, &options.output, reporter)?;

    let origins = fetched
        .iter()
        .map(|f| f.origin)
        .chain(fetched_files.iter().map(|f| f.origin));
    let count = |origin| origins.clone().filter(|&o| o == origin).count();
    let cached = count(Origin::Cache);
    Ok(BuildReport {
        image,
        fresh: fetched.len() + fetched_files.len() - cached,
        from_stores: count(Origin::Store),
        cached,
        entries: rootfs.len(),
    })
}

/// Place a file in the rootfs, checking it against the lock as it's read.
fn place(
    rootfs: &mut Rootfs,
    fetched: &Fetched<'_, LockedFile>,
    spec: &FileSpec,
) -> Result<(), BuildError> {
    let name = &fetched.item.name;
    let data =
        PackageCache::read(&fetched.path, fetched.item).map_err(|source| BuildError::Verify {
            package: name.clone(),
            source,
        })?;
    crate::files::place(rootfs, spec, &data).map_err(|source| BuildError::Place {
        file: name.clone(),
        source,
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
        let name = &fetched.item.name;
        let result = PackageCache::read(&fetched.path, fetched.item)
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
