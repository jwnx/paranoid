//! Lock file format and parsing.

use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::arch::Architecture;
use crate::build::IMAGE_FORMAT;
use crate::config::{FileSpec, HoldSpec, Manifest, PackageSpec, RepositoryConfig};
use crate::files::Indexes;
use crate::hash::Sha256;
use crate::oci::Digest;
use crate::repository::debian::{Fingerprint, Keyring, KeyringError};
use crate::resolve::{ResolvedManifest, ResolvedRepository};

/// A locked package entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub architecture: Architecture,
    pub sha256: Sha256,
    pub filename: String,
    pub size: u64,
    pub repository: String,
    /// The source package it was built from, as Debian writes it:
    /// `openssl`, or `glibc (2.36-9)` when the source's version differs.
    /// Left out when the source has the package's own name and version,
    /// and by locks written before paranoid kept it; see
    /// [`LockedPackage::source`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl LockedPackage {
    /// The source package's name and version. A lock written before
    /// paranoid kept them gives the package's own, which is right only
    /// for a package built from a source of its name.
    #[must_use]
    pub fn source(&self) -> (&str, &str) {
        match self.source.as_deref() {
            None => (&self.name, &self.version),
            Some(source) => match source.split_once(" (") {
                Some((name, version)) => (name, version.trim_end_matches(')')),
                None => (source, &self.version),
            },
        }
    }
}

/// A locked file, fetched from a URL rather than a repository.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedFile {
    /// Its id in the manifest.
    pub name: String,
    pub url: Url,
    pub sha256: Sha256,
    pub size: u64,
    /// What vouches for its hash.
    pub trust: Trust,
    /// For a signed file, primary fingerprints of the keys whose signatures
    /// checked out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signed_by: Vec<Fingerprint>,
    /// The detached signature that was checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Url>,
    /// The checksums file the hash came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksums: Option<Url>,
    /// The release an index resolved it to, by its publisher's name for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What vouches for a file's hash.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    /// The manifest pins it, so whoever wrote the manifest does.
    Pinned,
    /// A key the manifest trusts signed it, or a checksums file listing it.
    Signed,
    /// Its publisher's index lists its hash, fetched over HTTPS. Nothing
    /// signs it, so whoever controls the index does.
    Index,
}

/// A dependency nothing in the repositories satisfied, which the lock was
/// written without because `--allow-unmet` said so. Recording it keeps that
/// decision visible in review, and lets `build` ask for the same flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockedUnmet {
    pub required_by: String,
    pub dependency: String,
}

impl std::fmt::Display for LockedUnmet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (required by {})", self.dependency, self.required_by)
    }
}

/// A single dependency edge in the lock file.
///
/// Represents "parent depends on children".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DepEdge {
    pub parent: String,
    pub children: Vec<String>,
}

/// One image's part of the lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedImage {
    /// The image's id in the manifest.
    pub name: String,
    /// Every package in the image, sorted.
    pub packages: Vec<String>,
    /// The packages the manifest asks for, sorted.
    pub direct: Vec<String>,
    /// The files it gets, in manifest order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Dependencies left unmet, sorted. Empty unless the lock was written
    /// with `--allow-unmet`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmet: Vec<LockedUnmet>,
    /// Dependency graph edges: parent → [children]. An empty graph
    /// serializes to nothing, hence the default.
    #[serde(rename = "dep", default)]
    pub deps: Vec<DepEdge>,
    /// The digest `build --record` got for the image, and what it got it
    /// from. `build` checks a later build against it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<ExpectedDigest>,
}

/// An image's digest, as one build got it.
///
/// The digest follows from the image's inputs, the image format paranoid
/// wrote it in and the creation time, so it's recorded with all three and
/// holds only for a build that has the same.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedDigest {
    /// The image manifest's digest, which a registry knows it by.
    pub digest: Digest,
    /// The [`IMAGE_FORMAT`] it was built with. Digests recorded before
    /// paranoid had one leave it out, and no build checks them: their
    /// inputs were hashed another way too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    /// The paranoid version that built it, for people: whether the digest
    /// holds doesn't depend on it.
    pub paranoid: String,
    /// The creation time it was built with, `SOURCE_DATE_EPOCH`, in
    /// seconds.
    pub epoch: i64,
    /// A hash of what the build read; see [`crate::build::inputs`].
    pub inputs: Digest,
}

impl ExpectedDigest {
    /// Why a build of `inputs` at `epoch` by the running paranoid can't
    /// be checked against this digest, or `None` if it can.
    #[must_use]
    pub fn unchecked(&self, inputs: &Digest, epoch: i64) -> Option<Unchecked> {
        if self.format != Some(IMAGE_FORMAT) {
            Some(Unchecked::Format)
        } else if self.epoch != epoch {
            Some(Unchecked::Epoch)
        } else if &self.inputs != inputs {
            Some(Unchecked::Inputs)
        } else {
            None
        }
    }

    /// Whether this digest is the one a build of `inputs` at `epoch` by
    /// the running paranoid should get.
    #[must_use]
    pub fn applies(&self, inputs: &Digest, epoch: i64) -> bool {
        self.unchecked(inputs, epoch).is_none()
    }
}

/// What a build differs from a recorded digest in, so it can't be checked
/// against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unchecked {
    /// It writes another image format.
    Format,
    /// It's at another `SOURCE_DATE_EPOCH`.
    Epoch,
    /// It reads other inputs.
    Inputs,
}

/// The lock file structure.
///
/// Packages are listed once, with one version across every image; each
/// `[[image]]` says which of them it contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    /// Hash of the config that generated this lock file.
    /// Used to detect when the lock file is stale.
    pub config_hash: String,

    /// The signed repository snapshots the packages were resolved from.
    #[serde(rename = "repository", default)]
    pub repositories: Vec<ResolvedRepository>,

    /// Every locked package of every image, sorted by name.
    #[serde(rename = "package", default)]
    pub packages: Vec<LockedPackage>,

    /// Every locked file of every image, in manifest order.
    #[serde(rename = "file", default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<LockedFile>,

    /// The images, in manifest order.
    #[serde(rename = "image", default)]
    pub images: Vec<LockedImage>,
}

/// Errors that can occur when working with lock files.
#[derive(Debug, Error)]
pub enum LockFileError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error(
        "the lock records no signed repository snapshots; it predates signature checking, run `paranoid lock` again"
    )]
    Unsigned,

    #[error("the lock records no images; it predates image sets, run `paranoid lock` again")]
    NoImages,

    #[error("image '{image}' lists package '{package}', which the lock doesn't record")]
    UnknownPackage { image: String, package: String },

    #[error("image '{image}' lists file '{file}', which the lock doesn't record")]
    UnknownFile { image: String, file: String },

    #[error("the lock records file '{0}' twice")]
    DuplicateFile(String),

    #[error(
        "package '{package}' comes from repository '{repository}', which the lock doesn't record"
    )]
    UnknownRepository { package: String, repository: String },

    #[error("file '{file}': {source}")]
    FileKeyring {
        file: String,
        #[source]
        source: Box<KeyringError>,
    },

    #[error("repository '{repository}': {source}")]
    Keyring {
        repository: String,
        #[source]
        source: Box<KeyringError>,
    },
}

impl LockFile {
    /// Create a new lock file from a resolved manifest.
    ///
    /// Takes ownership of the resolved manifest to avoid cloning.
    #[must_use]
    pub fn from_resolved(manifest: &Manifest, resolved: ResolvedManifest) -> Self {
        let config_hash = Self::compute_config_hash(manifest);

        let mut packages: Vec<LockedPackage> = resolved
            .packages
            .into_iter()
            .map(|p| LockedPackage {
                name: p.name,
                version: p.version,
                architecture: p.architecture,
                sha256: p.sha256,
                filename: p.filename,
                size: p.size,
                repository: p.repository,
                source: p.source,
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        let images = resolved
            .images
            .into_iter()
            .map(|image| {
                let mut deps: Vec<DepEdge> = image
                    .graph
                    .into_iter()
                    .map(|(parent, mut children)| {
                        children.sort();
                        DepEdge { parent, children }
                    })
                    .collect();
                deps.sort_by(|a, b| a.parent.cmp(&b.parent));
                let mut unmet: Vec<LockedUnmet> = image
                    .unmet
                    .into_iter()
                    .map(|u| LockedUnmet {
                        required_by: u.required_by,
                        dependency: u.dependency.to_string(),
                    })
                    .collect();
                unmet.sort();
                let sorted = |mut names: Vec<String>| {
                    names.sort();
                    names
                };
                let files = manifest
                    .images
                    .get(&image.id)
                    .map(|spec| spec.files.clone())
                    .unwrap_or_default();
                LockedImage {
                    name: image.id,
                    packages: sorted(image.packages),
                    direct: sorted(image.direct),
                    files,
                    unmet,
                    deps,
                    expected: None,
                }
            })
            .collect();

        Self {
            config_hash,
            repositories: resolved.repositories,
            packages,
            files: Vec::new(),
            images,
        }
    }

    /// The lock with these files, which [`Self::from_resolved`] leaves to
    /// the caller since resolution doesn't fetch them.
    #[must_use]
    pub fn with_files(self, files: Vec<LockedFile>) -> Self {
        Self { files, ..self }
    }

    /// An image's files, in manifest order.
    pub fn image_files<'a>(&'a self, image: &'a LockedImage) -> Vec<&'a LockedFile> {
        image
            .files
            .iter()
            .filter_map(|name| self.files.iter().find(|f| &f.name == name))
            .collect()
    }

    /// The image with this id.
    pub fn image(&self, id: &str) -> Option<&LockedImage> {
        self.images.iter().find(|i| i.name == id)
    }

    /// An image's packages, in lock order.
    pub fn image_packages<'a>(&'a self, image: &'a LockedImage) -> Vec<&'a LockedPackage> {
        self.packages
            .iter()
            .filter(|p| image.packages.contains(&p.name))
            .collect()
    }

    /// Every unmet dependency, with the image it's in.
    pub fn unmet(&self) -> impl Iterator<Item = (&str, &LockedUnmet)> {
        self.images
            .iter()
            .flat_map(|i| i.unmet.iter().map(move |u| (i.name.as_str(), u)))
    }

    /// Load a lock file from disk.
    pub fn from_file(path: &Path) -> Result<Self, LockFileError> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse a lock file from a string.
    ///
    /// Every package must come from a signed snapshot the lock records.
    fn parse(content: &str) -> Result<Self, LockFileError> {
        let lock: LockFile = toml::from_str(content)?;
        if lock.repositories.is_empty() {
            return Err(LockFileError::Unsigned);
        }
        if lock.images.is_empty() {
            return Err(LockFileError::NoImages);
        }
        for (i, file) in lock.files.iter().enumerate() {
            if lock.files[..i].iter().any(|f| f.name == file.name) {
                return Err(LockFileError::DuplicateFile(file.name.clone()));
            }
        }
        for image in &lock.images {
            let missing = image
                .packages
                .iter()
                .find(|name| !lock.packages.iter().any(|p| &p.name == *name))
                .or_else(|| image.direct.iter().find(|d| !image.packages.contains(d)));
            if let Some(name) = missing {
                return Err(LockFileError::UnknownPackage {
                    image: image.name.clone(),
                    package: name.clone(),
                });
            }
            if let Some(name) = image
                .files
                .iter()
                .find(|name| !lock.files.iter().any(|f| &f.name == *name))
            {
                return Err(LockFileError::UnknownFile {
                    image: image.name.clone(),
                    file: name.clone(),
                });
            }
        }
        for pkg in &lock.packages {
            if !lock.repositories.iter().any(|r| r.name == pkg.repository) {
                return Err(LockFileError::UnknownRepository {
                    package: pkg.name.clone(),
                    repository: pkg.repository.clone(),
                });
            }
        }
        Ok(lock)
    }

    /// Write the lock file to disk.
    pub fn write(&self, path: &Path) -> Result<(), LockFileError> {
        let content = self.serialize()?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Serialize the lock file to a TOML string.
    pub fn serialize(&self) -> Result<String, LockFileError> {
        let mut output = String::new();
        output.push_str("# paranoid.lock\n");
        output.push_str("# Generated by paranoid - do not edit manually\n");
        output.push_str("#\n");
        output.push_str(&format!("# Config hash: {}\n", self.config_hash));
        output.push('\n');

        // Serialize the rest
        let toml_content = toml::to_string_pretty(self)?;
        output.push_str(&toml_content);

        Ok(output)
    }

    /// Compute the SHA256 hash of the lock file content.
    ///
    /// Returns an error if the lock file cannot be serialized.
    pub fn content_hash(&self) -> Result<String, LockFileError> {
        let content = toml::to_string(self)?;
        Ok(Digest::from_bytes(content.as_bytes()).to_string())
    }

    /// Whether the lock still stands for the manifest: it was resolved from
    /// this manifest, each snapshot and signed file it records was signed
    /// by a key the manifest still trusts for it and that isn't revoked,
    /// and each file is recorded the way the manifest asks for it: from
    /// the same URLs, with the same pin, signed if and only if the
    /// manifest names keys, and at a release the manifest allows for a file
    /// from an index. The config hash covers the `signed_by` paths but not
    /// what's in the files, nor what the lock says, hence the rest.
    pub fn is_current(&self, manifest: &Manifest) -> Result<bool, LockFileError> {
        if !self.matches_config(manifest) {
            return Ok(false);
        }
        for repo in &self.repositories {
            let Some(config) = manifest.repositories.get(&repo.name) else {
                return Ok(false);
            };
            let keyring =
                Keyring::load(&manifest.resolve_path(&config.signed_by)).map_err(|source| {
                    LockFileError::Keyring {
                        repository: repo.name.clone(),
                        source: Box::new(source),
                    }
                })?;
            let trusted = keyring.vouching();
            if !repo.signed_by.iter().any(|fp| trusted.contains(fp)) {
                return Ok(false);
            }
        }
        for file in &self.files {
            let Some(spec) = manifest.files.get(&file.name) else {
                return Ok(false);
            };
            // What the file should be at the release the lock records.
            let expected =
                Indexes::default().expected(spec, file.version.as_deref(), manifest.architecture);
            let as_asked = expected.is_some_and(|e| {
                e.url == file.url
                    && e.signature == file.signature
                    && e.checksums == file.checksums
                    && e.trust == file.trust
            }) && spec.sha256.is_none_or(|sha256| sha256 == file.sha256);
            if !as_asked {
                return Ok(false);
            }
            let Some(signed_by) = &spec.signed_by else {
                continue;
            };
            let keyring = Keyring::load(&manifest.resolve_path(signed_by)).map_err(|source| {
                LockFileError::FileKeyring {
                    file: file.name.clone(),
                    source: Box::new(source),
                }
            })?;
            let trusted = keyring.vouching();
            if !file.signed_by.iter().any(|fp| trusted.contains(fp)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Check if this lock file matches a manifest's config hash.
    #[must_use]
    pub fn matches_config(&self, manifest: &Manifest) -> bool {
        let current_hash = Self::compute_config_hash(manifest);
        self.config_hash == current_hash
    }

    /// Compute a hash of the manifest's resolution inputs.
    ///
    /// We hash a canonical JSON serialization of exactly the fields that
    /// affect resolution: the target architecture, the repositories in
    /// declaration order (order breaks version ties), and the requested
    /// packages, and the files and which images get them. Serializing
    /// rather than concatenating keeps field boundaries, so `["ab", "c"]`
    /// and `["a", "bc"]` hash differently. What a manifest doesn't use is
    /// left out, so adding a kind of input doesn't change the hash of a
    /// manifest without it.
    fn compute_config_hash(manifest: &Manifest) -> String {
        #[derive(Serialize)]
        struct ResolutionInputs<'a> {
            architecture: String,
            repositories: Vec<(&'a str, &'a RepositoryConfig)>,
            images: Vec<(&'a str, Vec<&'a PackageSpec>)>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            holds: Vec<(&'a str, &'a HoldSpec)>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            files: Vec<(&'a str, &'a FileSpec)>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            image_files: Vec<(&'a str, &'a [String])>,
        }

        let images = manifest
            .images
            .iter()
            .map(|(id, image)| {
                let mut packages: Vec<_> = image.packages.iter().collect();
                packages.sort_by(|a, b| a.name.cmp(&b.name));
                (id.as_str(), packages)
            })
            .collect();
        let mut holds_sorted: Vec<_> = manifest
            .holds
            .iter()
            .map(|(n, h)| (n.as_str(), h))
            .collect();
        holds_sorted.sort_by_key(|(name, _)| *name);

        let inputs = ResolutionInputs {
            architecture: manifest.architecture.to_string(),
            repositories: manifest
                .repositories
                .iter()
                .map(|(name, repo)| (name.as_str(), repo))
                .collect(),
            images,
            holds: holds_sorted,
            files: manifest
                .files
                .iter()
                .map(|(id, file)| (id.as_str(), file))
                .collect(),
            image_files: manifest
                .images
                .iter()
                .filter(|(_, image)| !image.files.is_empty())
                .map(|(id, image)| (id.as_str(), image.files.as_slice()))
                .collect(),
        };

        let json = serde_json::to_vec(&inputs).expect("resolution inputs always serialize");
        Digest::from_bytes(&json).to_string()
    }

    /// Get a package by name.
    #[must_use]
    pub fn get_package(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Get the total number of packages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Check if the lock file has no packages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

impl FromStr for LockFile {
    type Err = LockFileError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::config::Manifest;
    use crate::resolve::{ResolvedImage, ResolvedPackage};

    fn make_manifest() -> Manifest {
        Manifest::from_str(&make_manifest_toml()).unwrap()
    }

    fn make_manifest_toml() -> String {
        r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"

[[packages]]
name = "coreutils"
"#
        .to_string()
    }

    // Valid SHA256 hashes for tests (64 hex chars)
    const TEST_HASH_1: &str = "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3";
    const TEST_HASH_2: &str = "b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0";

    fn make_repository() -> ResolvedRepository {
        ResolvedRepository {
            name: "debian".to_string(),
            date: "2026-07-11T10:16:37Z".parse().unwrap(),
            inrelease: TEST_HASH_2.parse().unwrap(),
            signed_by: vec![],
        }
    }

    fn make_resolved() -> ResolvedManifest {
        ResolvedManifest {
            repositories: vec![make_repository()],
            packages: vec![
                ResolvedPackage {
                    name: "bash".to_string(),
                    version: "5.2.15-2".to_string(),
                    architecture: Architecture::Amd64,
                    sha256: TEST_HASH_1.parse().unwrap(),
                    filename: "pool/main/b/bash/bash_5.2.15-2_amd64.deb".to_string(),
                    size: 1234567,
                    repository: "debian".to_string(),
                    source: None,
                },
                ResolvedPackage {
                    name: "coreutils".to_string(),
                    version: "9.1-1".to_string(),
                    architecture: Architecture::Amd64,
                    sha256: TEST_HASH_2.parse().unwrap(),
                    filename: "pool/main/c/coreutils/coreutils_9.1-1_amd64.deb".to_string(),
                    size: 2345678,
                    repository: "debian".to_string(),
                    source: None,
                },
            ],
            images: vec![ResolvedImage {
                id: "default".to_string(),
                packages: vec!["bash".to_string(), "coreutils".to_string()],
                direct: vec!["bash".to_string()],
                graph: std::collections::BTreeMap::from([(
                    "bash".to_string(),
                    vec!["coreutils".to_string()],
                )]),
                unmet: vec![],
            }],
        }
    }

    #[test]
    fn test_create_lock_file() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);

        assert_eq!(lock.packages.len(), 2);
        assert!(!lock.config_hash.is_empty());

        // Packages should be sorted by name
        assert_eq!(lock.packages[0].name, "bash");
        assert_eq!(lock.packages[1].name, "coreutils");

        // The image and its graph edge should be preserved
        let image = &lock.images[0];
        assert_eq!(image.packages, ["bash", "coreutils"]);
        assert_eq!(image.direct, ["bash"]);
        assert_eq!(image.deps.len(), 1);
        assert_eq!(image.deps[0].parent, "bash");
        assert_eq!(image.deps[0].children, vec!["coreutils"]);
    }

    #[test]
    fn test_dep_graph_round_trip() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);
        let serialized = lock.serialize().unwrap();
        let parsed = LockFile::from_str(&serialized).unwrap();

        assert_eq!(parsed.images, lock.images);
    }

    const ONE_PACKAGE: &str = r#"
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

    #[test]
    fn records_an_expected_digest() {
        let mut lock = LockFile::from_resolved(&make_manifest(), make_resolved());
        let inputs = Digest::from_bytes(b"inputs");
        let expected = ExpectedDigest {
            digest: Digest::from_bytes(b"manifest"),
            format: Some(IMAGE_FORMAT),
            paranoid: env!("CARGO_PKG_VERSION").to_string(),
            epoch: 1700000000,
            inputs,
        };
        lock.images[0].expected = Some(expected.clone());
        let serialized = lock.serialize().unwrap();
        assert!(serialized.contains("[image.expected]"), "{serialized}");
        let parsed = LockFile::from_str(&serialized).unwrap();
        assert_eq!(parsed.images[0].expected.as_ref(), Some(&expected));

        // It holds only for the same inputs, epoch and image format.
        assert!(expected.applies(&inputs, 1700000000));
        let other = Digest::from_bytes(b"other");
        assert_eq!(
            expected.unchecked(&other, 1700000000),
            Some(Unchecked::Inputs)
        );
        assert_eq!(expected.unchecked(&inputs, 0), Some(Unchecked::Epoch));
        let newer = ExpectedDigest {
            format: Some(IMAGE_FORMAT + 1),
            ..expected.clone()
        };
        assert_eq!(
            newer.unchecked(&inputs, 1700000000),
            Some(Unchecked::Format)
        );

        // Which paranoid recorded it doesn't matter.
        let elsewhere = ExpectedDigest {
            paranoid: "0.0.0-dev".to_string(),
            ..expected
        };
        assert!(elsewhere.applies(&inputs, 1700000000));
    }

    #[test]
    fn reads_digests_recorded_before_image_formats() {
        let older: ExpectedDigest = toml::from_str(&format!(
            "digest = \"sha256:{TEST_HASH_1}\"\nparanoid = \"0.7.2\"\n\
             epoch = 0\ninputs = \"sha256:{TEST_HASH_2}\"\n"
        ))
        .unwrap();
        assert_eq!(older.format, None);
        assert_eq!(older.unchecked(&older.inputs, 0), Some(Unchecked::Format));
    }

    #[test]
    fn records_source_packages() {
        let mut resolved = make_resolved();
        resolved.packages[1].source = Some("coreutils-src (9.1-0)".to_string());
        let lock = LockFile::from_resolved(&make_manifest(), resolved);
        let serialized = lock.serialize().unwrap();
        assert!(
            serialized.contains("source = \"coreutils-src (9.1-0)\""),
            "{serialized}"
        );
        assert_eq!(serialized.matches("source = ").count(), 1, "{serialized}");

        let parsed = LockFile::from_str(&serialized).unwrap();
        let source = |name: &str| parsed.get_package(name).unwrap().source();
        assert_eq!(source("bash"), ("bash", "5.2.15-2"));
        assert_eq!(source("coreutils"), ("coreutils-src", "9.1-0"));
        let renamed = LockedPackage {
            source: Some("shells".to_string()),
            ..parsed.get_package("bash").unwrap().clone()
        };
        assert_eq!(renamed.source(), ("shells", "5.2.15-2"));
    }

    #[test]
    fn records_repository_snapshots() {
        let lock = LockFile::from_resolved(&make_manifest(), make_resolved());
        let serialized = lock.serialize().unwrap();
        assert!(serialized.contains(&format!(
            "[[repository]]\nname = \"debian\"\ndate = \"2026-07-11T10:16:37Z\"\ninrelease = \"{TEST_HASH_2}\""
        )));
        let parsed = LockFile::from_str(&serialized).unwrap();
        assert_eq!(parsed.repositories, [make_repository()]);
    }

    #[test]
    fn rejects_locks_without_signed_snapshots() {
        let unsigned = format!("config_hash = \"sha256:aaaa\"\n{ONE_PACKAGE}");
        assert!(matches!(
            LockFile::from_str(&unsigned),
            Err(LockFileError::Unsigned)
        ));

        let elsewhere = format!(
            "config_hash = \"sha256:aaaa\"\n\n[[repository]]\nname = \"other\"\n\
             date = \"2026-07-11T10:16:37Z\"\ninrelease = \"{TEST_HASH_2}\"\nsigned_by = []\n{ONE_PACKAGE}"
        );
        assert_eq!(
            LockFile::from_str(&elsewhere).unwrap_err().to_string(),
            "package 'bash' comes from repository 'debian', which the lock doesn't record"
        );
    }

    #[test]
    fn test_serialize_and_parse() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);
        let serialized = lock.serialize().unwrap();

        // Should contain header comment
        assert!(serialized.contains("# paranoid.lock"));
        assert!(serialized.contains("# Config hash:"));

        // Should be parseable
        let parsed = LockFile::from_str(&serialized).unwrap();
        assert_eq!(parsed.packages.len(), 2);
        assert_eq!(parsed.config_hash, lock.config_hash);
    }

    fn with_tini() -> (Manifest, LockFile) {
        let manifest = Manifest::from_str(&format!(
            "{}\n[files.tini]\nurl = \"https://example.com/tini\"\n\
             sha256 = \"{TEST_HASH_1}\"\npath = \"/usr/bin/tini\"\n",
            make_manifest_toml()
        ))
        .unwrap();
        let lock =
            LockFile::from_resolved(&manifest, make_resolved()).with_files(vec![LockedFile {
                name: "tini".to_string(),
                url: "https://example.com/tini".parse().unwrap(),
                sha256: TEST_HASH_1.parse().unwrap(),
                size: 42,
                trust: Trust::Pinned,
                signed_by: Vec::new(),
                signature: None,
                checksums: None,
                version: None,
            }]);
        (manifest, lock)
    }

    #[test]
    fn records_files_and_the_images_that_get_them() {
        let (manifest, lock) = with_tini();
        let serialized = lock.serialize().unwrap();
        assert!(
            serialized.contains("[[file]]\nname = \"tini\""),
            "{serialized}"
        );
        assert!(serialized.contains("trust = \"pinned\""), "{serialized}");

        let parsed = LockFile::from_str(&serialized).unwrap();
        assert_eq!(parsed.files, lock.files);
        let image = &parsed.images[0];
        assert_eq!(image.files, ["tini"]);
        assert_eq!(parsed.image_files(image), [&lock.files[0]]);
        assert!(parsed.matches_config(&manifest));
    }

    #[test]
    fn leaves_files_out_when_there_are_none() {
        let serialized = LockFile::from_resolved(&make_manifest(), make_resolved())
            .serialize()
            .unwrap();
        assert!(!serialized.contains("[[file]]"), "{serialized}");
        assert!(!serialized.contains("files ="), "{serialized}");
    }

    #[test]
    fn a_signed_file_needs_a_signer_the_manifest_still_trusts() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/repository/debian/testdata/files");
        let manifest = |keys: &str| {
            let toml = format!(
                "{}\n[files.tool]\nurl = \"https://example.com/tool\"\npath = \"/tool\"\n\
                 signed_by = \"{keys}\"\nsignature = \"https://example.com/tool.asc\"\n",
                make_manifest_toml().replace("keys/debian.asc", "release.asc")
            );
            Manifest::from_file_content(&dir.join("paranoid.toml"), &toml).unwrap()
        };
        let trusted = manifest("release.asc");
        let mut lock = LockFile::from_resolved(&trusted, make_resolved());
        lock.repositories.clear();
        lock.files = vec![LockedFile {
            name: "tool".to_string(),
            url: "https://example.com/tool".parse().unwrap(),
            sha256: TEST_HASH_1.parse().unwrap(),
            size: 7,
            trust: Trust::Signed,
            signed_by: vec![
                serde_json::from_str("\"7B6CFBF9384A130F6053229331B1E3B66D0E9BF6\"").unwrap(),
            ],
            signature: Some("https://example.com/tool.asc".parse().unwrap()),
            checksums: None,
            version: None,
        }];
        assert!(lock.is_current(&trusted).unwrap());

        // A lock edited to say the file is only pinned.
        lock.files[0].trust = Trust::Pinned;
        assert!(!lock.is_current(&trusted).unwrap());
        lock.files[0].trust = Trust::Signed;

        // The same keyring path with other keys in it.
        let stranger = manifest("stranger.asc");
        lock.config_hash = LockFile::compute_config_hash(&stranger);
        assert!(!lock.is_current(&stranger).unwrap());

        // A signer that has been revoked since.
        let revoked = manifest("revoked.asc");
        lock.config_hash = LockFile::compute_config_hash(&revoked);
        lock.files[0].signed_by =
            vec![serde_json::from_str("\"42D7C88C41C585732BDAEFACB8BA386B1127FC7C\"").unwrap()];
        assert!(!lock.is_current(&revoked).unwrap());
    }

    #[test]
    fn a_resolved_file_has_to_be_at_a_release_the_manifest_allows() {
        let manifest = |version: &str| {
            Manifest::from_str(&format!(
                "{}\n[files.etcdctl]\ngcs = \"etcd\"\nversion = \"{version}\"\n\
                 asset = \"etcd-v{{version}}-linux-{{arch}}.tar.gz\"\npath = \"/etcdctl\"\n",
                make_manifest_toml()
            ))
            .unwrap()
        };
        let three_six = manifest("3.6");
        let base = "https://storage.googleapis.com/etcd/v3.6.15/";
        let mut lock =
            LockFile::from_resolved(&three_six, make_resolved()).with_files(vec![LockedFile {
                name: "etcdctl".to_string(),
                url: format!("{base}etcd-v3.6.15-linux-amd64.tar.gz")
                    .parse()
                    .unwrap(),
                sha256: TEST_HASH_1.parse().unwrap(),
                size: 7,
                trust: Trust::Index,
                signed_by: Vec::new(),
                signature: None,
                checksums: Some(format!("{base}SHA256SUMS").parse().unwrap()),
                version: Some("v3.6.15".to_string()),
            }]);
        lock.repositories.clear();
        assert!(lock.is_current(&three_six).unwrap());

        // Another release line, or a lock edited to another release.
        let three_seven = manifest("3.7");
        lock.config_hash = LockFile::compute_config_hash(&three_seven);
        assert!(!lock.is_current(&three_seven).unwrap());
        lock.config_hash = LockFile::compute_config_hash(&three_six);
        lock.files[0].version = Some("v3.6.16".to_string());
        assert!(!lock.is_current(&three_six).unwrap());
    }

    #[test]
    fn a_pinned_file_has_to_have_the_manifests_hash() {
        let (manifest, mut lock) = with_tini();
        lock.repositories.clear();
        assert!(lock.is_current(&manifest).unwrap());
        lock.files[0].sha256 = TEST_HASH_2.parse().unwrap();
        assert!(!lock.is_current(&manifest).unwrap());
    }

    #[test]
    fn rejects_a_file_recorded_twice() {
        let (_, mut lock) = with_tini();
        lock.files.push(lock.files[0].clone());
        let err = LockFile::from_str(&lock.serialize().unwrap()).unwrap_err();
        assert_eq!(err.to_string(), "the lock records file 'tini' twice");
    }

    #[test]
    fn rejects_an_image_with_a_file_the_lock_does_not_record() {
        let (_, mut lock) = with_tini();
        lock.files.clear();
        let err = LockFile::from_str(&lock.serialize().unwrap()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "image 'default' lists file 'tini', which the lock doesn't record"
        );
    }

    #[test]
    fn test_config_hash_determinism() {
        let manifest = make_manifest();

        let hash1 = LockFile::compute_config_hash(&manifest);
        let hash2 = LockFile::compute_config_hash(&manifest);

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn stores_stay_out_of_the_config_hash() {
        let without = make_manifest();
        let mut with = without.clone();
        with.stores = vec!["https://bucket.example.org/debs".parse().unwrap()];
        assert_eq!(
            LockFile::compute_config_hash(&without),
            LockFile::compute_config_hash(&with)
        );
    }

    #[test]
    fn files_are_resolution_inputs() {
        let file = |sha256: &str| {
            format!(
                "[files.tini]\nurl = \"https://example.com/tini\"\n\
                 sha256 = \"{sha256}\"\npath = \"/usr/bin/tini\"\n"
            )
        };
        let hash = |toml: &str| LockFile::compute_config_hash(&Manifest::from_str(toml).unwrap());
        let plain = hash(&make_manifest_toml());
        let one = hash(&format!("{}{}", make_manifest_toml(), file(TEST_HASH_1)));
        let other = hash(&format!("{}{}", make_manifest_toml(), file(TEST_HASH_2)));
        assert_ne!(plain, one);
        assert_ne!(one, other);
    }

    #[test]
    fn test_config_hash_changes() {
        let config1 = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;
        let config2 = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"

[[packages]]
name = "vim"
"#;

        let manifest1 = Manifest::from_str(config1).unwrap();
        let manifest2 = Manifest::from_str(config2).unwrap();

        let hash1 = LockFile::compute_config_hash(&manifest1);
        let hash2 = LockFile::compute_config_hash(&manifest2);

        assert_ne!(hash1, hash2);
    }

    /// The hash covers what the constraint means, not how it was spelled.
    #[test]
    fn test_config_hash_normalizes_versions() {
        let with_version = |version: &str| {
            let config = format!(
                "[repositories.debian]\nurl = \"https://deb.debian.org/debian\"\n\
                 distribution = \"bookworm\"\ncomponents = [\"main\"]\nsigned_by = \"keys/debian.asc\"\n\n\
                 [[packages]]\nname = \"bash\"\nversion = \"{version}\"\n"
            );
            LockFile::compute_config_hash(&Manifest::from_str(&config).unwrap())
        };
        assert_eq!(with_version(">=5.2"), with_version(">= 5.2"));
        assert_eq!(with_version("5.2"), with_version("= 5.2"));
        assert_ne!(with_version(">= 5.2"), with_version(">= 5.3"));
    }

    #[test]
    fn test_matches_config() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);

        assert!(lock.matches_config(&manifest));

        // Change manifest
        let different_config = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "vim"
"#;
        let different_manifest = Manifest::from_str(different_config).unwrap();
        assert!(!lock.matches_config(&different_manifest));
    }

    #[test]
    fn test_get_package() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);

        let bash = lock.get_package("bash");
        assert!(bash.is_some());
        assert_eq!(bash.unwrap().version, "5.2.15-2");

        let nonexistent = lock.get_package("nonexistent");
        assert!(nonexistent.is_none());
    }

    #[test]
    fn is_current_needs_a_signer_the_manifest_still_trusts() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("debian.asc");
        let write_key = |name: &str| {
            let testdata = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/repository/debian/testdata")
                .join(name);
            std::fs::copy(testdata, &key).unwrap();
        };
        let manifest_path = dir.path().join("paranoid.toml");
        let toml = make_manifest_toml().replace("keys/debian.asc", "debian.asc");
        std::fs::write(&manifest_path, toml).unwrap();
        let manifest = Manifest::from_file(&manifest_path).unwrap();

        let mut lock = LockFile::from_resolved(&manifest, make_resolved());
        lock.repositories[0].signed_by =
            vec![serde_json::from_str("\"B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8\"").unwrap()];

        write_key("debian-archive-bookworm-automatic.asc");
        assert!(lock.is_current(&manifest).unwrap());

        // Same path, different key: the config hash can't tell.
        write_key("debian-archive-bookworm-security-automatic.asc");
        assert!(lock.matches_config(&manifest));
        assert!(!lock.is_current(&manifest).unwrap());

        std::fs::remove_file(&key).unwrap();
        assert!(matches!(
            lock.is_current(&manifest),
            Err(LockFileError::Keyring { .. })
        ));
    }

    #[test]
    fn leaves_unmet_out_when_there_is_none() {
        let lock = LockFile::from_resolved(&make_manifest(), make_resolved());
        let serialized = lock.serialize().unwrap();
        assert!(!serialized.contains("unmet"), "{serialized}");
    }

    #[test]
    fn records_unmet_dependencies_sorted() {
        use crate::repository::debian::UnmetDependency;

        let unmet = |by: &str, dep: &str| UnmetDependency {
            required_by: by.to_string(),
            dependency: dep.parse().unwrap(),
        };
        let mut resolved = make_resolved();
        resolved.images[0].unmet = vec![
            unmet("coreutils", "libacl1 (>= 2.2)"),
            unmet("bash", "base-files (>= 2.1.12) | debianutils"),
        ];

        let lock = LockFile::from_resolved(&make_manifest(), resolved);
        let shown: Vec<_> = lock.unmet().map(|(_, u)| u.to_string()).collect();
        assert_eq!(
            shown,
            [
                "base-files (>= 2.1.12) | debianutils (required by bash)",
                "libacl1 (>= 2.2) (required by coreutils)",
            ]
        );

        let parsed = LockFile::from_str(&lock.serialize().unwrap()).unwrap();
        assert_eq!(parsed.images, lock.images);
    }

    #[test]
    fn holds_are_resolution_inputs() {
        let plain = make_manifest();
        let held = Manifest::from_str(&format!(
            "{}\n[holds]\nbash = \"= 5.2.15-2\"\n",
            make_manifest_toml()
        ))
        .unwrap();
        assert_ne!(
            LockFile::compute_config_hash(&plain),
            LockFile::compute_config_hash(&held)
        );
    }

    #[test]
    fn test_content_hash() {
        let manifest = make_manifest();
        let resolved = make_resolved();

        let lock = LockFile::from_resolved(&manifest, resolved);
        let hash = lock.content_hash().unwrap();

        assert!(hash.starts_with("sha256:"));
    }
}
