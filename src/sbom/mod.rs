//! Software bills of materials for an image, from its lock: SPDX 2.3 and
//! CycloneDX 1.6, both as JSON.
//!
//! The lock already says everything an image is built from, so an SBOM
//! needs no build and no scan. It names each Debian package with a package
//! URL that vulnerability scanners match on (`pkg:deb/debian/libssl3@...
//! ?arch=arm64&distro=debian-12&upstream=openssl`), each `[files]` entry with
//! its URL and hash, and the image itself by the digest the lock records
//! for it, when it records one that still holds.
//!
//! The same lock gives the same SBOM: its time is the newest snapshot's,
//! and its id is derived from what it describes.

mod cyclonedx;
mod spdx;

use std::fmt::Write;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::arch::Architecture;
use crate::build::{self, BuildError, IMAGE_FORMAT};
use crate::config::Manifest;
use crate::lock::{DepEdge, LockFile, LockedFile, LockedPackage};
use crate::oci::{Digest, OciConfig, OciConfigError, Timestamp};
use crate::resolve::ResolvedRepository;

/// An SBOM format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// SPDX 2.3, as JSON.
    Spdx,
    /// CycloneDX 1.6, as JSON.
    CycloneDx,
}

#[derive(Debug, Error)]
pub enum SbomError {
    #[error("no image '{0}' in the lock")]
    UnknownImage(String),

    #[error(transparent)]
    Inputs(#[from] BuildError),

    #[error("the image's name or tag is invalid")]
    Name(#[from] OciConfigError),

    #[error("repository '{0}' is in the lock but not the manifest")]
    UnknownRepository(String),
}

/// What an SBOM says about one image of a lock.
#[derive(Debug, Clone)]
pub struct Sbom<'a> {
    image: Image,
    created: DateTime<Utc>,
    id: String,
    distro: Distro,
    packages: Vec<Package<'a>>,
    files: Vec<File<'a>>,
    repositories: &'a [ResolvedRepository],
    direct: &'a [String],
    deps: &'a [DepEdge],
}

/// The image an SBOM describes.
#[derive(Debug, Clone)]
struct Image {
    /// Its id in the manifest.
    id: String,
    /// `name:tag`, or the id when it has no name.
    name: String,
    /// The digest the lock records for it, if that still holds.
    digest: Option<Digest>,
    /// Its package URL, which needs the digest.
    purl: Option<String>,
}

#[derive(Debug, Clone)]
struct Package<'a> {
    locked: &'a LockedPackage,
    purl: String,
    /// Where the `.deb` is, in its repository's snapshot.
    download: String,
}

#[derive(Debug, Clone)]
struct File<'a> {
    locked: &'a LockedFile,
    purl: String,
}

impl<'a> Sbom<'a> {
    /// The SBOM of `image`, one of the images of `manifest` and `lock`.
    pub fn new(manifest: &Manifest, lock: &'a LockFile, image: &str) -> Result<Self, SbomError> {
        let unknown = || SbomError::UnknownImage(image.to_string());
        let locked = lock.image(image).ok_or_else(unknown)?;
        let spec = manifest.images.get(image).ok_or_else(unknown)?;
        let inputs = build::inputs(manifest, lock, image)?;

        // A recorded digest names this image only while it holds.
        let digest = locked
            .expected
            .as_ref()
            .filter(|e| e.format == Some(IMAGE_FORMAT) && e.inputs == inputs)
            .map(|e| e.digest);
        let config = OciConfig::from_image(spec, manifest.architecture, Timestamp::epoch())?;
        let image_purl = digest.map(|digest| {
            let repository = config.repository();
            let name = match &repository {
                Some(repository) => repository.rsplit('/').next().unwrap_or(repository),
                None => image,
            };
            let mut qualifiers = vec![("arch", manifest.architecture.to_string())];
            if let Some(repository) = &repository {
                qualifiers.push(("repository_url", repository.clone()));
                qualifiers.push(("tag", config.tag().to_string()));
            }
            purl("oci", None, name, &digest.to_string(), &qualifiers)
        });
        let image = Image {
            id: image.to_string(),
            name: if spec.name.is_some() {
                config.reference()
            } else {
                image.to_string()
            },
            digest,
            purl: image_purl,
        };

        let distro = Distro::of(manifest);
        let packages = lock
            .image_packages(locked)
            .into_iter()
            .map(|locked| {
                let repository = manifest
                    .repositories
                    .get(&locked.repository)
                    .ok_or_else(|| SbomError::UnknownRepository(locked.repository.clone()))?;
                Ok(Package {
                    locked,
                    purl: distro.purl(locked, manifest.architecture),
                    download: format!("{}/{}", repository.base_url(), locked.filename),
                })
            })
            .collect::<Result<_, SbomError>>()?;
        let files = lock
            .image_files(locked)
            .into_iter()
            .map(|locked| {
                let mut qualifiers = vec![("checksum", format!("sha256:{}", locked.sha256))];
                qualifiers.push(("download_url", locked.url.to_string()));
                let version = locked.version.as_deref().unwrap_or("");
                File {
                    locked,
                    purl: purl("generic", None, &locked.name, version, &qualifiers),
                }
            })
            .collect();

        let created = lock
            .repositories
            .iter()
            .map(|r| r.date)
            .max()
            .unwrap_or_default();
        // The same image of the same lock, described by the same paranoid,
        // gets the same id.
        let id = Digest::from_bytes(
            format!(
                "{}\n{inputs}\n{}\n{}",
                image.id,
                digest.map(|d| d.to_string()).unwrap_or_default(),
                env!("CARGO_PKG_VERSION"),
            )
            .as_bytes(),
        );

        Ok(Self {
            image,
            created,
            id: uuid(&id),
            distro,
            packages,
            files,
            repositories: &lock.repositories,
            direct: &locked.direct,
            deps: &locked.deps,
        })
    }

    /// The SBOM in `format`, as pretty-printed JSON.
    #[must_use]
    pub fn to_json(&self, format: Format) -> String {
        let value = match format {
            Format::Spdx => spdx::document(self),
            Format::CycloneDx => cyclonedx::document(self),
        };
        serde_json::to_string_pretty(&value).expect("an SBOM always serializes")
    }
}

/// The distribution an image's packages belong to, for their package URLs:
/// the first repository whose distribution paranoid knows, `bookworm` for
/// `bookworm-security` too. Scanners take an image's packages to be its
/// distribution's, whatever repository they came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Distro {
    /// `debian` or `ubuntu`.
    namespace: &'static str,
    /// `debian-12`, `ubuntu-24.04`, or none when the codename is unknown.
    qualifier: Option<&'static str>,
}

/// Codenames, their distribution and the version scanners know them by.
const CODENAMES: &[(&str, &str, &str)] = &[
    ("buster", "debian", "debian-10"),
    ("bullseye", "debian", "debian-11"),
    ("bookworm", "debian", "debian-12"),
    ("trixie", "debian", "debian-13"),
    ("forky", "debian", "debian-14"),
    ("focal", "ubuntu", "ubuntu-20.04"),
    ("jammy", "ubuntu", "ubuntu-22.04"),
    ("noble", "ubuntu", "ubuntu-24.04"),
    ("plucky", "ubuntu", "ubuntu-25.04"),
    ("questing", "ubuntu", "ubuntu-25.10"),
];

impl Distro {
    /// The operating system as scanners name it, `debian` and `12`, when
    /// the codename is known. Scanners take it from the SBOM to know which
    /// advisories apply: Grype finds nothing in a CycloneDX SBOM without.
    fn os(self) -> Option<(&'static str, &'static str)> {
        self.qualifier?.split_once('-')
    }

    fn of(manifest: &Manifest) -> Self {
        let codename = |distribution: &str| {
            let base = distribution
                .split(['-', '/'])
                .next()
                .unwrap_or(distribution);
            CODENAMES.iter().find(|(name, ..)| *name == base)
        };
        let known = manifest
            .repositories
            .values()
            .find_map(|r| codename(&r.distribution));
        match known {
            Some(&(_, namespace, qualifier)) => Self {
                namespace,
                qualifier: Some(qualifier),
            },
            None => Self {
                namespace: match manifest.repositories.values().next() {
                    Some(r) if r.url.as_str().contains("ubuntu") => "ubuntu",
                    _ => "debian",
                },
                qualifier: None,
            },
        }
    }

    fn purl(self, package: &LockedPackage, architecture: Architecture) -> String {
        // `all` packages are the image's architecture's as far as a scanner
        // is concerned, but say what the lock says.
        let arch = match package.architecture {
            Architecture::All => "all".to_string(),
            _ => architecture.to_string(),
        };
        let mut qualifiers = vec![("arch", arch)];
        if let Some(distro) = self.qualifier {
            qualifiers.push(("distro", distro.to_string()));
        }
        let (source, version) = package.source();
        if source != package.name || version != package.version {
            let upstream = if version == package.version {
                source.to_string()
            } else {
                format!("{source}@{version}")
            };
            qualifiers.push(("upstream", upstream));
        }
        purl(
            "deb",
            Some(self.namespace),
            &package.name,
            &package.version,
            &qualifiers,
        )
    }
}

/// A package URL: `pkg:type/namespace/name@version?qualifiers`, with
/// qualifiers in the order given, which the callers keep sorted by key.
fn purl(
    kind: &str,
    namespace: Option<&str>,
    name: &str,
    version: &str,
    qualifiers: &[(&str, String)],
) -> String {
    let mut purl = format!("pkg:{kind}/");
    if let Some(namespace) = namespace {
        let _ = write!(purl, "{}/", encode(namespace, ""));
    }
    purl.push_str(&encode(name, ""));
    if !version.is_empty() {
        let _ = write!(purl, "@{}", encode(version, ""));
    }
    for (i, (key, value)) in qualifiers.iter().enumerate() {
        let sep = if i == 0 { '?' } else { '&' };
        let _ = write!(purl, "{sep}{key}={}", encode(value, "/"));
    }
    purl
}

/// Percent-encode everything but unreserved characters and `keep`.
fn encode(s: &str, keep: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || "-._~".contains(c) || keep.contains(c) {
            out.push(c);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// A name-based UUID (version 5's layout) from a digest.
fn uuid(digest: &Digest) -> String {
    let hex = digest.to_string();
    let hex = hex.trim_start_matches("sha256:");
    let mut bytes: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("a digest is hex"))
        .collect();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests;
