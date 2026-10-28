//! Files fetched from a URL rather than a repository (`[files.<id>]`).
//!
//! `lock` checks each file against the hash the manifest pins and records
//! its size; from then on a file is a blob like a package: `build` fetches
//! it from the stores or its URL, checks it against the lock, and places it
//! in the image after the packages.

use crate::config::Manifest;
use crate::download::{DownloadFailures, DownloadService, PackageCache, Sources};
use crate::lock::{LockedFile, Trust};
use crate::report::Reporter;

/// The most `lock` reads of a file before it knows the file's size.
pub const MAX_FILE_SIZE: u64 = 4 << 30;

/// Lock the manifest's files, in manifest order: check each against the
/// hash the manifest pins, and record its size. A file the cache has isn't
/// downloaded again, and one that's downloaded goes into the cache for
/// `build`.
pub async fn lock(
    manifest: &Manifest,
    downloads: &DownloadService,
    reporter: &dyn Reporter,
) -> Result<Vec<LockedFile>, DownloadFailures> {
    let mut files: Vec<LockedFile> = manifest
        .files
        .iter()
        .map(|(id, spec)| LockedFile {
            name: id.clone(),
            url: spec.url.clone(),
            sha256: spec.sha256,
            size: MAX_FILE_SIZE,
            trust: Trust::Pinned,
        })
        .collect();

    let cache = downloads.cache();
    let cached = |file: &LockedFile| {
        let data = PackageCache::read(&cache.path_for(file), file).ok()?;
        Some(data.len() as u64)
    };
    let mut missing = Vec::new();
    for (i, file) in files.iter_mut().enumerate() {
        match cached(file) {
            Some(size) => file.size = size,
            None => missing.push(i),
        }
    }

    let wanted: Vec<LockedFile> = missing.iter().map(|&i| files[i].clone()).collect();
    let wanted: Vec<&LockedFile> = wanted.iter().collect();
    let fetched = downloads
        .download_all(&wanted, Sources::of(manifest), reporter)
        .await?;
    for (&i, fetched) in missing.iter().zip(fetched) {
        // The download was checked against the hash on its way into the
        // cache, so its length is the file's size.
        files[i].size = std::fs::metadata(&fetched.path)
            .map(|m| m.len())
            .map_err(|e| DownloadFailures(vec![(files[i].name.clone(), e.into())]))?;
    }
    Ok(files)
}
