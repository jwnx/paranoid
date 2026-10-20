//! `paranoid cache`: fill a store with the lock's packages.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use url::Url;

use paranoid::config::Manifest;
use paranoid::download::{DownloadConfig, DownloadService, PackageCache, Sources};
use paranoid::http::Retry;
use paranoid::lock::{LockFile, LockedPackage};
use paranoid::store::{PushFailures, Store};

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
    /// Upload the lock's packages that a store doesn't have yet.
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

    let packages: Vec<&LockedPackage> = lock.packages.iter().collect();
    let missing = store.missing(&packages, args.jobs).await.map_err(refused)?;
    let present = packages.len() - missing.len();

    let reporter = DotsReporter::new(quiet);
    let pushed = if missing.is_empty() {
        None
    } else {
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
        // Not from the store we're filling: it may hold a broken copy,
        // which is why we're pushing.
        let stores: Vec<Url> = manifest
            .stores
            .iter()
            .filter(|url| !store.is_at(url))
            .cloned()
            .collect();
        let sources = Sources {
            stores: &stores,
            repositories: &manifest.repositories,
        };
        let fetched = downloader
            .download_all(&missing, sources, &reporter)
            .await
            .map_err(CliError::push)?;
        Some(
            store
                .push(&fetched, args.jobs, &reporter)
                .await
                .map_err(refused)?,
        )
    };

    if !quiet {
        let unsigned = if unsigned { ", unsigned" } else { "" };
        match pushed {
            Some(report) => eprintln!(
                "Pushed {} packages ({}) to {store}{unsigned}; {present} were there already.",
                report.pushed,
                st::fmt_size(report.bytes),
            ),
            None => eprintln!("{store} has all {present} packages."),
        }
    }
    Ok(())
}
