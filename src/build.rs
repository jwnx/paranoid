//! Building an image from a manifest and its lock file.
//!
//! The pipeline is: download every locked package and file into the cache,
//! apply the packages to an in-memory [`Rootfs`] in lock order, record them
//! in the dpkg database (`/var/lib/dpkg`), keep the locales the image asks
//! for out of `locales-all`, write the manifest's accounts,
//! place the files, configure apt for the repositories, write the paths,
//! and write a single-layer OCI image from it.

use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use crate::accounts::{self, AccountsError};
use crate::apt::{self, AptError};
use crate::arch::Architecture;
use crate::config::{FileSpec, ImageSpec, Manifest};
use crate::configure::{self, ConfigureError, Runtime};
use crate::deb::{DebArchive, DebError};
use crate::download::{
    DownloadConfig, DownloadError, DownloadFailures, DownloadService, Fetched, Origin,
    PackageCache, Sources,
};
use crate::dpkg::{DpkgDatabase, DpkgError, PackageState};
use crate::files::{PlaceError, render_extract};
use crate::locales::{self, LocalesError};
use crate::lock::{LockFile, LockedFile, LockedPackage};
use crate::oci::{
    Digest, ImageOutput, OciConfig, OciConfigError, OciError, OciImageResult, Timestamp,
};
use crate::paths::{self, PathsError};
use crate::report::{Event, Phase, Reporter};
use crate::rootfs::{Rootfs, RootfsError};

/// The version of the bytes paranoid writes for an image.
///
/// Builds with the same inputs and creation time give the same image as
/// long as this stays the same, whichever paranoid makes them, so a
/// recorded digest holds across paranoid versions that share it. Bump it
/// with any change to what a build writes for the same inputs: the files
/// it places and how, the tar headers, the OCI JSON, what configuring
/// leaves behind.
pub const IMAGE_FORMAT: u32 = 1;

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
    /// What configures an image that asks for it; podman or Docker on
    /// `PATH` when `None`.
    pub runtime: Option<Runtime>,
    /// Configure twice and fail if the results differ, naming the paths.
    pub check_reproducible: bool,
    /// The digest the image must come out with, from the lock. A build
    /// that gets another fails without writing anything.
    pub expected: Option<Digest>,
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

    #[error("failed to keep the locales")]
    Locales(#[from] LocalesError),

    #[error("failed to write the accounts")]
    Accounts(#[from] AccountsError),

    #[error("failed to configure the image")]
    Configure(#[from] ConfigureError),

    #[error(
        "configuring twice gave different images; these paths differ:\n  {}",
        .0.join("\n  ")
    )]
    NotReproducible(Vec<String>),

    #[error("failed to configure apt")]
    Apt(#[from] AptError),

    #[error("failed to write a path")]
    Paths(#[from] PathsError),

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

    #[error(
        "the image's digest is {got}, but the lock records {expected} for the same inputs, \
         image format and SOURCE_DATE_EPOCH: this build didn't reproduce it"
    )]
    NotAsRecorded { expected: Digest, got: Digest },
}

/// A hash of everything a build of `image` reads besides the paranoid
/// binary and the creation time: the image as the manifest describes it,
/// the packages and files the lock has for it, and the apt files it
/// writes. Two builds with the same inputs, [`IMAGE_FORMAT`] and creation
/// time give the same image, so a digest recorded with all three holds for
/// a later build only while they match.
///
/// The hash is of the manifest's and the lock's types serialized, which
/// leave out what's unset, so it stays the same from one paranoid to the
/// next unless the image changes: a release that adds an option keeps the
/// inputs of every image that doesn't use it.
pub fn inputs(manifest: &Manifest, lock: &LockFile, image: &str) -> Result<Digest, BuildError> {
    let unknown = || BuildError::UnknownImage(image.to_string());
    let spec = manifest.images.get(image).ok_or_else(unknown)?;
    let locked = lock.image(image).ok_or_else(unknown)?;
    let files = lock
        .image_files(locked)
        .into_iter()
        .map(|file| {
            manifest
                .files
                .get(&file.name)
                .map(|spec| (file, spec))
                .ok_or_else(|| BuildError::UnknownFile(file.name.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let apt = if spec.apt_sources {
        apt::files(manifest)?
    } else {
        Vec::new()
    };

    #[derive(Serialize)]
    struct Inputs<'a> {
        architecture: String,
        image: &'a ImageSpec,
        packages: Vec<&'a LockedPackage>,
        files: Vec<(&'a LockedFile, &'a FileSpec)>,
        apt: Vec<(String, Vec<u8>)>,
    }
    let inputs = Inputs {
        architecture: manifest.architecture.to_string(),
        image: spec,
        packages: lock.image_packages(locked),
        files,
        apt,
    };
    let json = serde_json::to_vec(&inputs).expect("build inputs always serialize");
    Ok(Digest::from_bytes(&json))
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
    let mut config = OciConfig::from_image(spec, manifest.architecture, options.created.clone())?;
    if let Some(expected) = options.expected {
        config = config.expect_digest(expected);
    }

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

    let state = if spec.unconfigured || spec.configure {
        PackageState::Unpacked
    } else {
        PackageState::Installed
    };
    let mut rootfs = assemble(&fetched, state, reporter)?;
    if !spec.locales.is_empty() {
        locales::keep(&mut rootfs, &spec.locales)?;
    }
    if !spec.accounts.is_empty() {
        accounts::write(&mut rootfs, &spec.accounts)?;
    }
    for (fetched, (_, spec)) in fetched_files.iter().zip(&files) {
        place(&mut rootfs, fetched, spec, manifest.architecture)?;
    }
    if spec.apt_sources {
        apt::write(&mut rootfs, manifest)?;
    }
    if !spec.paths.is_empty() {
        paths::apply(&mut rootfs, &spec.paths)?;
    }
    if spec.configure {
        let runtime = options
            .runtime
            .or_else(Runtime::detect)
            .ok_or(ConfigureError::NoRuntime)?;
        let packages = packages.len() as u64;
        let runs = if options.check_reproducible { 2 } else { 1 };
        reporter.report(Event::Begin {
            phase: Phase::Configure,
            total: runs,
        });
        let epoch = options.created.as_datetime().timestamp();
        let unpacked = rootfs;
        let mut configured = Vec::new();
        for _ in 0..runs {
            let mut rootfs =
                configure::configure(&unpacked, runtime, manifest.architecture, epoch).await?;
            // What the manifest declares wins over what a script did.
            if !spec.accounts.is_empty() {
                accounts::verify(&rootfs, &spec.accounts)?;
            }
            if !spec.paths.is_empty() {
                paths::reapply(&mut rootfs, &spec.paths)?;
            }
            reporter.report(Event::Advance {
                phase: Phase::Configure,
                units: 1,
                item: &runtime.to_string(),
            });
            configured.push(rootfs);
        }
        if let [first, second] = &configured[..] {
            let differ = first.differences(second);
            if !differ.is_empty() {
                return Err(BuildError::NotReproducible(differ));
            }
        }
        rootfs = configured.swap_remove(0);
        reporter.report(Event::End {
            phase: Phase::Configure,
            summary: packages,
        });
    }
    let image = crate::oci::write_image(&config, &rootfs, &options.output, reporter).map_err(
        |e| match e {
            OciError::UnexpectedDigest { expected, got } => {
                BuildError::NotAsRecorded { expected, got }
            }
            e => BuildError::Image(e),
        },
    )?;

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
    arch: Architecture,
) -> Result<(), BuildError> {
    let name = &fetched.item.name;
    let data =
        PackageCache::read(&fetched.path, fetched.item).map_err(|source| BuildError::Verify {
            package: name.clone(),
            source,
        })?;
    // An extracted path can name the release, as `etcd-v{version}/etcdctl`.
    let extract = render_extract(spec, fetched.item.version.as_deref(), arch).map_err(|e| {
        BuildError::Place {
            file: name.clone(),
            source: PlaceError::Extract(e),
        }
    })?;
    let spec = FileSpec {
        extract,
        ..spec.clone()
    };
    crate::files::place(rootfs, &spec, &data).map_err(|source| BuildError::Place {
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const MANIFEST: &str = r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;

    const LOCK: &str = r#"
config_hash = "sha256:aaaa"

[[repository]]
name = "debian"
date = "2026-07-11T10:16:37Z"
inrelease = "b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0"
signed_by = []

[[package]]
name = "bash"
version = "5.0"
architecture = "amd64"
sha256 = "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3"
filename = "pool/main/b/bash/bash_5.0_amd64.deb"
size = 1000
repository = "debian"

[[image]]
name = "default"
packages = ["bash"]
direct = ["bash"]
"#;

    fn inputs_of(manifest: &str, lock: &str) -> Digest {
        let manifest = Manifest::from_str(manifest).unwrap();
        let lock = LockFile::from_str(lock).unwrap();
        inputs(&manifest, &lock, "default").unwrap()
    }

    #[test]
    fn inputs_follow_what_the_image_is_built_from() {
        let base = inputs_of(MANIFEST, LOCK);
        assert_eq!(inputs_of(MANIFEST, LOCK), base);

        // Where the image goes doesn't change it.
        let elsewhere = MANIFEST.replace(
            "architecture = \"amd64\"",
            "architecture = \"amd64\"\noutput = \"out.tar\"",
        );
        assert_eq!(inputs_of(&elsewhere, LOCK), base);

        // What's in it does, whether the manifest or the lock says so.
        let with_path = format!("{MANIFEST}\n[paths.\"/etc/x\"]\ncontent = \"x\"\n");
        assert_ne!(inputs_of(&with_path, LOCK), base);
        let with_env = format!("{MANIFEST}\n[entrypoint.env]\nA = \"b\"\n");
        assert_ne!(inputs_of(&with_env, LOCK), base);
        let rebuilt = LOCK.replace("version = \"5.0\"", "version = \"5.0+b1\"");
        assert_ne!(inputs_of(MANIFEST, &rebuilt), base);
    }

    /// An image's settings serialize without the options it doesn't use,
    /// so an option a later paranoid adds leaves its inputs alone.
    #[test]
    fn inputs_leave_out_unset_options() {
        assert_eq!(
            serde_json::to_value(ImageSpec::default()).unwrap(),
            serde_json::json!({})
        );
        let manifest = Manifest::from_str(MANIFEST).unwrap();
        assert_eq!(
            serde_json::to_value(&manifest.images["default"]).unwrap(),
            serde_json::json!({
                "packages": [{"name": "bash", "version": null, "repository": null}],
            })
        );
    }

    /// Inputs are compared with ones recorded by other paranoid versions,
    /// so the same manifest and lock have to hash the same in all of them.
    /// If this fails, the change makes every recorded digest stop being
    /// checked: keep the old hash instead, by leaving out what's unset.
    #[test]
    fn inputs_stay_the_same_across_versions() {
        let manifest = format!(
            r#"{MANIFEST}
[accounts.users.app]
uid = 999
groups = ["staff"]

[paths."/srv/app"]
owner = "app"
mode = "0750"

[paths."/etc/app.conf"]
content = "x = 1\n"

[entrypoint]
cmd = ["/bin/sh"]
env = {{ PATH = "/usr/bin", LANG = "C.UTF-8" }}
"#
        );
        assert_eq!(
            inputs_of(MANIFEST, LOCK).to_string(),
            "sha256:e5a60bf2b00189b423359720dc0b6f755903306fe596ff18f18b37b0bb997a71"
        );
        assert_eq!(
            inputs_of(&manifest, LOCK).to_string(),
            "sha256:4a2a5fa4e4f06a695dcb821868c4804a9a63ee27f4a55598778045a96c8b1cf5"
        );
    }
}
