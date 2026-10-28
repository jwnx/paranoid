//! `paranoid cache`: fill a store with the lock's packages and files.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use url::Url;

use paranoid::config::Manifest;
use paranoid::download::{
    Blob, DownloadConfig, DownloadFailures, DownloadService, PackageCache, Sources,
};
use paranoid::http::Retry;
use paranoid::lock::LockFile;
use paranoid::store::{PushFailures, PushReport, Store};

use super::error::CliError;
use super::progress::DotsReporter;
use super::style as st;

#[derive(Parser)]
pub struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Upload the lock's packages and files that a store doesn't have yet.
    Push(PushArgs),
}

#[derive(Parser)]
struct PushArgs {
    /// The store: an http(s) URL that takes PUTs, such as an S3 bucket's,
    /// or a directory. Defaults to the manifest's store, if it lists one.
    ///
    /// Packages go to `<store>/sha256/<2 hex>/<sha256>.deb`. Requests are
    /// signed for S3 when AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY are
    /// set, with AWS_REGION (default us-east-1).
    #[arg(value_name = "URL|DIR")]
    store: Option<String>,

    /// Path to the manifest file or directory containing one.
    #[arg(short = 'f', long, value_name = "FILE|DIR", default_value = ".")]
    manifest: PathBuf,

    /// Lock file whose packages to push.
    ///
    /// Defaults to `paranoid.lock` next to the manifest.
    #[arg(long, value_name = "FILE")]
    lock: Option<PathBuf>,

    /// Directory holding downloaded packages (defaults to system cache).
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// Maximum number of parallel requests.
    #[arg(long, value_name = "N", default_value = "4")]
    jobs: NonZeroUsize,
}

pub async fn run(args: CacheArgs, quiet: bool) -> Result<(), CliError> {
    match args.command {
        CacheCommand::Push(args) => push(args, quiet).await,
    }
}

async fn push(args: PushArgs, quiet: bool) -> Result<(), CliError> {
    let manifest = Manifest::load(&args.manifest).map_err(CliError::manifest)?;
    let lock_path = args
        .lock
        .unwrap_or_else(|| Manifest::default_lock_path(&args.manifest));
    if !lock_path.exists() {
        return Err(CliError::missing_lock(&lock_path));
    }
    let lock = LockFile::from_file(&lock_path).map_err(CliError::lock)?;

    let target = match (args.store, manifest.stores.as_slice()) {
        (Some(store), _) => store,
        (None, [store]) => store.to_string(),
        (None, []) => {
            return Err(CliError::config(
                "name a store to push to: the manifest lists none under [download]",
            ));
        }
        (None, _) => {
            return Err(CliError::config(
                "name a store to push to: the manifest lists several under [download]",
            ));
        }
    };
    let store = Store::open(&target, Retry::default()).map_err(CliError::push)?;
    let unsigned = matches!(
        store,
        Store::Http {
            credentials: None,
            ..
        }
    );
    let refused = |e: PushFailures| {
        if unsigned {
            CliError::push_unsigned(e)
        } else {
            CliError::push(e)
        }
    };

    let reporter = DotsReporter::new(quiet);
    let cache = match &args.cache_dir {
        Some(dir) => PackageCache::with_base_dir(dir.clone()),
        None => PackageCache::new(),
    }
    .map_err(CliError::push)?;
    let config = DownloadConfig {
        jobs: args.jobs,
        ..DownloadConfig::default()
    };
    let downloader = DownloadService::with_cache(cache, config).map_err(CliError::push)?;
    // Not from the store we're filling: it may hold a broken copy, which
    // is why we're pushing.
    let stores: Vec<Url> = manifest
        .stores
        .iter()
        .filter(|url| !store.is_at(url))
        .cloned()
        .collect();
    let filler = Filler {
        store: &store,
        downloader: &downloader,
        sources: Sources {
            stores: &stores,
            repositories: &manifest.repositories,
        },
        jobs: args.jobs,
        reporter: &reporter,
    };
    let fail = |e| match e {
        FillError::Download(e) => CliError::push(e),
        FillError::Push(e) => refused(e),
    };
    let packages = filler.fill(&lock.packages).await.map_err(fail)?;
    let files = filler.fill(&lock.files).await.map_err(fail)?;

    if !quiet {
        let unsigned = if unsigned { ", unsigned" } else { "" };
        let present = packages.present + files.present;
        let pushed = |f: &Filled| f.pushed.as_ref().map_or(0, |r| r.pushed);
        let bytes = |f: &Filled| f.pushed.as_ref().map_or(0, |r| r.bytes);
        if packages.pushed.is_none() && files.pushed.is_none() {
            eprintln!(
                "{store} has all {}.",
                counted(lock.packages.len(), lock.files.len())
            );
        } else {
            eprintln!(
                "Pushed {} ({}) to {store}{unsigned}; {present} were there already.",
                counted(pushed(&packages), pushed(&files)),
                st::fmt_size(bytes(&packages) + bytes(&files)),
            );
        }
    }
    Ok(())
}

/// "3 packages", or "3 packages and 1 file" when there are files.
fn counted(packages: usize, files: usize) -> String {
    match files {
        0 => format!("{packages} packages"),
        1 => format!("{packages} packages and 1 file"),
        n => format!("{packages} packages and {n} files"),
    }
}

/// Fills a store with what it lacks of a lock's packages or files.
struct Filler<'a> {
    store: &'a Store,
    downloader: &'a DownloadService,
    /// Where to download what the cache doesn't have.
    sources: Sources<'a>,
    jobs: NonZeroUsize,
    reporter: &'a DotsReporter,
}

/// Why a [`Filler`] failed.
enum FillError {
    /// What was missing couldn't be downloaded.
    Download(DownloadFailures),
    /// The store refused what was.
    Push(PushFailures),
}

/// What a [`Filler`] did.
struct Filled {
    /// How many the store had already.
    present: usize,
    /// What was pushed, unless nothing was missing.
    pushed: Option<PushReport>,
}

impl Filler<'_> {
    async fn fill<B: Blob + Sync>(&self, blobs: &[B]) -> Result<Filled, FillError> {
        let blobs: Vec<&B> = blobs.iter().collect();
        let missing = self
            .store
            .missing(&blobs, self.jobs)
            .await
            .map_err(FillError::Push)?;
        let present = blobs.len() - missing.len();
        if missing.is_empty() {
            return Ok(Filled {
                present,
                pushed: None,
            });
        }
        let fetched = self
            .downloader
            .download_all(&missing, self.sources, self.reporter)
            .await
            .map_err(FillError::Download)?;
        let pushed = self
            .store
            .push(&fetched, self.jobs, self.reporter)
            .await
            .map_err(FillError::Push)?;
        Ok(Filled {
            present,
            pushed: Some(pushed),
        })
    }
}
