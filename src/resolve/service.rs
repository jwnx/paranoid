//! Resolution service implementation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::arch::Architecture;
use crate::config::{ImageSpec, Manifest};
use crate::hash::{ParseSha256Error, Sha256};
use crate::http::{Client, ClientError};
use crate::repository::debian::{
    DebianPackage, DebianRepository, DebianRepositoryError, Fingerprint, Hold, Keyring,
    KeyringError, Origin, PackageIndexSet, Request, ResolveError, UnmetDependency, check_sources,
    resolve,
};
use crate::version::{VersionConstraint, VersionRelation};

/// Errors that can occur during resolution.
#[derive(Debug, Error)]
pub enum ResolveServiceError {
    #[error("client error: {0}")]
    Client(#[from] ClientError),

    #[error("repository '{name}': signed_by {}: {source}", path.display())]
    Keyring {
        name: String,
        path: PathBuf,
        #[source]
        source: Box<KeyringError>,
    },

    #[error("repository '{name}': {source}")]
    Repository {
        name: String,
        #[source]
        source: Box<DebianRepositoryError>,
    },

    #[error("resolve error: {0}")]
    Resolve(#[from] ResolveError),

    #[error("image '{image}': {source}")]
    Image {
        image: String,
        #[source]
        source: ResolveError,
    },

    #[error("the hold on '{name}' matches no package in the resolution")]
    UnusedHold { name: String },

    #[error("package '{package}' has an invalid SHA256 in the repository index")]
    InvalidChecksum {
        package: String,
        #[source]
        source: ParseSha256Error,
    },
}

/// A resolved package with all metadata needed for the lock file.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub name: String,
    pub version: String,
    pub architecture: Architecture,
    pub sha256: Sha256,
    pub filename: String,
    pub size: u64,
    /// The repository this package came from.
    pub repository: String,
}

impl ResolvedPackage {
    /// Create a ResolvedPackage from a DebianPackage and repository name.
    ///
    /// This is where a package's checksum leaves the repository index and
    /// heads for the lock file, so we parse it here.
    pub fn from_debian_package(
        pkg: &DebianPackage,
        repository: impl Into<String>,
    ) -> Result<Self, ResolveServiceError> {
        let sha256 = pkg
            .sha256
            .parse()
            .map_err(|source| ResolveServiceError::InvalidChecksum {
                package: pkg.name.clone(),
                source,
            })?;
        Ok(Self {
            name: pkg.name.clone(),
            version: pkg.version.to_string(),
            architecture: pkg.architecture.unwrap_or(Architecture::All),
            sha256,
            filename: pkg.filename.clone(),
            size: pkg.size,
            repository: repository.into(),
        })
    }
}

/// The signed snapshot of a repository that a resolution used. The lock
/// file records these as its `[[repository]]` tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRepository {
    pub name: String,
    /// The `Date` of its `Release`.
    pub date: DateTime<Utc>,
    /// SHA-256 of its `InRelease` file.
    pub inrelease: Sha256,
    /// Primary fingerprints of the keys whose signatures verified.
    pub signed_by: Vec<Fingerprint>,
}

/// One image's part of a resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    /// The image's id in the manifest.
    pub id: String,
    /// Every package in the image, sorted.
    pub packages: Vec<String>,
    /// The packages the manifest asks for, sorted.
    pub direct: Vec<String>,
    /// Dependency graph: parent name → sorted list of child names.
    pub graph: BTreeMap<String, Vec<String>>,
    /// Dependencies nothing in the repositories satisfies.
    pub unmet: Vec<UnmetDependency>,
}

/// The complete resolution of a manifest's images.
#[derive(Debug)]
pub struct ResolvedManifest {
    /// The repository snapshots, in manifest order.
    pub repositories: Vec<ResolvedRepository>,
    /// Every package of every image, one version per name, sorted by name.
    pub packages: Vec<ResolvedPackage>,
    /// The images, in manifest order.
    pub images: Vec<ResolvedImage>,
}

/// Service for resolving packages from a manifest.
pub struct ResolveService {
    client: Client,
}

impl ResolveService {
    /// Create a new resolve service.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn new() -> Result<Self, ResolveServiceError> {
        Ok(Self {
            client: Client::new()?,
        })
    }

    /// Resolve all packages from a manifest.
    pub async fn resolve(
        &self,
        manifest: &Manifest,
    ) -> Result<ResolvedManifest, ResolveServiceError> {
        // Repository presence, pins and architecture were validated when
        // the manifest was parsed.
        let arch = manifest.architecture;

        let now = Utc::now();

        // Build the index set from all repositories, each from a snapshot
        // signed by one of the keys the manifest names for it.
        let mut index_set = PackageIndexSet::new();
        let mut repositories = Vec::new();
        for (name, config) in &manifest.repositories {
            let path = manifest.resolve_path(&config.signed_by);
            let keyring = Keyring::load(&path).map_err(|source| ResolveServiceError::Keyring {
                name: name.clone(),
                path: config.signed_by.clone(),
                source: Box::new(source),
            })?;
            let repository_error = |source| ResolveServiceError::Repository {
                name: name.clone(),
                source: Box::new(source),
            };

            let repo = DebianRepository::new(&self.client, name, config, &keyring);
            let snapshot = repo.fetch_release(now).await.map_err(repository_error)?;
            let index = repo
                .fetch_index(&snapshot, arch)
                .await
                .map_err(repository_error)?;
            let priority = config
                .priority
                .unwrap_or_else(|| snapshot.release.default_pin_priority());
            index_set.add_index_with_priority(name.clone(), priority, index);
            repositories.push(ResolvedRepository {
                name: name.clone(),
                date: snapshot.release.date,
                inrelease: snapshot.inrelease,
                signed_by: snapshot.signers,
            });
        }

        let (images, chosen) = resolve_images(&index_set, manifest)?;
        let packages = chosen
            .iter()
            .map(|(package, repository)| ResolvedPackage::from_debian_package(package, *repository))
            .collect::<Result<_, _>>()?;

        Ok(ResolvedManifest {
            repositories,
            packages,
            images,
        })
    }
}

/// Every package any image selected, with the repository it came from,
/// sorted by name.
type Selected<'a> = Vec<(&'a DebianPackage, &'a str)>;

/// Each selected package's version and repository, with the image that
/// chose it, by name.
type Chosen<'a, 'm> = BTreeMap<&'a str, (&'a DebianPackage, &'a str, &'m str)>;

/// An image that couldn't be resolved, and what the images before it chose.
struct PassFailure<'a, 'm> {
    image: &'m str,
    source: ResolveError,
    chosen: Chosen<'a, 'm>,
}

/// Resolve every image of `manifest` against `index_set`.
///
/// A package has one version across the set. Images resolve in manifest
/// order, and the first image to select a package fixes its version for
/// the rest, unless a later image can't live with it: then we resolve that
/// image without the earlier choices, adopt the versions it picks for the
/// whole set, and start over. Each retry fixes a package not fixed before,
/// so this ends; it fails when the earlier images can't live with the later
/// one's versions either.
fn resolve_images<'a>(
    index_set: &'a PackageIndexSet,
    manifest: &Manifest,
) -> Result<(Vec<ResolvedImage>, Selected<'a>), ResolveServiceError> {
    let arch = manifest.architecture;
    let mut holds: Vec<Hold> = manifest
        .holds
        .iter()
        .map(|(name, hold)| Hold {
            name: name.clone(),
            version: hold.version.clone(),
            repository: hold.repository.clone(),
            origin: Origin::Hold,
        })
        .collect();

    let (images, chosen) = loop {
        let failure = match resolve_pass(index_set, manifest, &holds) {
            Ok(resolved) => break resolved,
            Err(failure) => failure,
        };
        let alone = resolve(
            index_set,
            &requests(&manifest.images[failure.image]),
            &holds,
            arch,
        );
        let learned: Vec<Hold> = alone
            .iter()
            .flat_map(|alone| &alone.packages)
            .filter(|r| {
                let name = r.package.name.as_str();
                failure
                    .chosen
                    .get(name)
                    .is_some_and(|&(package, repository, _)| {
                        package.version != r.package.version || repository != r.repository
                    })
                    && !is_learned(&holds, name)
            })
            .map(|r| exact_hold(r.package, r.repository, failure.image))
            .collect();
        if learned.is_empty() {
            return Err(if manifest.is_set() {
                ResolveServiceError::Image {
                    image: failure.image.to_string(),
                    source: failure.source,
                }
            } else {
                ResolveServiceError::Resolve(failure.source)
            });
        }
        holds.extend(learned);
    };

    // Each image checked its own sources; the set must agree too.
    check_sources(chosen.values().map(|(package, _, _)| *package))?;

    // A hold on nothing is most likely a misspelled name.
    if let Some(name) = manifest
        .holds
        .keys()
        .find(|name| !chosen.contains_key(name.as_str()))
    {
        return Err(ResolveServiceError::UnusedHold { name: name.clone() });
    }

    let chosen = chosen
        .into_values()
        .map(|(package, repository, _)| (package, repository))
        .collect();
    Ok((images, chosen))
}

/// Resolve the images in manifest order under `holds`, each package keeping
/// the version the first image to select it chose.
fn resolve_pass<'a, 'm>(
    index_set: &'a PackageIndexSet,
    manifest: &'m Manifest,
    holds: &[Hold],
) -> Result<(Vec<ResolvedImage>, Chosen<'a, 'm>), PassFailure<'a, 'm>> {
    let mut chosen: Chosen<'a, 'm> = BTreeMap::new();
    let mut images = Vec::new();
    for (id, image) in &manifest.images {
        let mut image_holds = holds.to_vec();
        image_holds.extend(
            chosen
                .values()
                .filter(|(package, _, _)| !is_learned(holds, &package.name))
                .map(|&(package, repository, by)| exact_hold(package, repository, by)),
        );

        let resolution = match resolve(
            index_set,
            &requests(image),
            &image_holds,
            manifest.architecture,
        ) {
            Ok(resolution) => resolution,
            Err(source) => {
                return Err(PassFailure {
                    image: id,
                    source,
                    chosen,
                });
            }
        };

        for r in &resolution.packages {
            chosen
                .entry(r.package.name.as_str())
                .or_insert((r.package, r.repository, id.as_str()));
        }
        images.push(ResolvedImage {
            id: id.clone(),
            packages: resolution
                .packages
                .iter()
                .map(|r| r.package.name.clone())
                .collect(),
            direct: resolution
                .packages
                .iter()
                .filter(|r| r.direct)
                .map(|r| r.package.name.clone())
                .collect(),
            graph: resolution.graph,
            unmet: resolution.unmet,
        });
    }
    Ok((images, chosen))
}

/// Whether `holds` already fix `name` to a version an image needed.
fn is_learned(holds: &[Hold], name: &str) -> bool {
    holds
        .iter()
        .any(|h| h.name == name && matches!(h.origin, Origin::Image(_)))
}

/// The packages an image asks for, as resolver requests.
fn requests(image: &ImageSpec) -> Vec<Request> {
    image
        .packages
        .iter()
        .map(|spec| Request {
            name: spec.name.clone(),
            version: spec.version.clone(),
            repository: spec.repository.clone(),
        })
        .collect()
}

/// A hold on exactly this package, as image `by` chose it.
fn exact_hold(package: &DebianPackage, repository: &str, by: &str) -> Hold {
    Hold {
        name: package.name.clone(),
        version: Some(VersionConstraint::new(
            VersionRelation::Equal,
            package.version.clone(),
        )),
        repository: Some(repository.to_string()),
        origin: Origin::Image(by.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::repository::debian::PackageIndex;

    /// An index from `Packages`-style stanzas, amd64 unless they say.
    fn index(stanzas: &str) -> PackageIndexSet {
        let text: String = stanzas
            .split("\n\n")
            .map(|stanza| {
                let mut stanza = stanza.trim().to_string();
                if !stanza.contains("Architecture:") {
                    stanza.push_str("\nArchitecture: amd64");
                }
                format!("{stanza}\nFilename: pool/x.deb\n\n")
            })
            .collect();
        let mut set = PackageIndexSet::new();
        set.add_index("debian".into(), PackageIndex::from_str(&text).unwrap());
        set
    }

    fn manifest(rest: &str) -> Manifest {
        Manifest::from_str(&format!(
            "[repositories.debian]\nurl = \"https://deb.debian.org/debian\"\n\
             distribution = \"trixie\"\ncomponents = [\"main\"]\nsigned_by = \"k.asc\"\n\n\
             [defaults]\narchitecture = \"amd64\"\n\n{rest}"
        ))
        .unwrap()
    }

    fn versions(selected: &Selected<'_>) -> Vec<String> {
        selected
            .iter()
            .map(|(p, _)| format!("{}={}", p.name, p.version))
            .collect()
    }

    const POSTGRES: &str = "\
Package: libc6\nVersion: 2.41\n\n\
Package: postgresql-17\nVersion: 17.11-1\nDepends: libc6\n\n\
Package: postgresql-17\nVersion: 17.12-1\nDepends: libc6\n\n\
Package: postgresql-server-dev-17\nSource: postgresql-17\nVersion: 17.11-1\n\n\
Package: postgresql-server-dev-17\nSource: postgresql-17\nVersion: 17.12-1\n\n\
Package: gcc\nVersion: 14.2\nDepends: libc6";

    #[test]
    fn each_image_gets_its_own_closure_with_shared_versions() {
        let set = index(POSTGRES);
        let manifest = manifest(
            "[images.runtime]\npackages = [\"postgresql-17\"]\n\n\
             [images.builder]\npackages = [\"postgresql-server-dev-17\", \"gcc\"]\n",
        );
        let (images, selected) = resolve_images(&set, &manifest).unwrap();
        assert_eq!(images[0].id, "runtime");
        assert_eq!(images[0].packages, ["libc6", "postgresql-17"]);
        // The builder's compiler doesn't leak into the runtime.
        assert_eq!(
            images[1].packages,
            ["gcc", "libc6", "postgresql-server-dev-17"]
        );
        assert_eq!(images[1].direct, ["gcc", "postgresql-server-dev-17"]);
        assert_eq!(
            versions(&selected),
            [
                "gcc=14.2",
                "libc6=2.41",
                "postgresql-17=17.12-1",
                "postgresql-server-dev-17=17.12-1",
            ]
        );
    }

    /// A hold on only the headers would build extensions for a server the
    /// runtime doesn't have. Each image is consistent on its own; only the
    /// set isn't.
    #[test]
    fn the_source_rule_spans_images() {
        let set = index(POSTGRES);
        let manifest = manifest(
            "[holds]\npostgresql-server-dev-17 = \"= 17.11-1\"\n\n\
             [images.runtime]\npackages = [\"postgresql-17\"]\n\n\
             [images.builder]\npackages = [\"postgresql-server-dev-17\"]\n",
        );
        let err = resolve_images(&set, &manifest).unwrap_err();
        assert_eq!(
            err.to_string(),
            "resolve error: 'postgresql-17' and 'postgresql-server-dev-17' were built from \
             different versions of source package 'postgresql-17': 17.12-1 and 17.11-1; hold \
             one of them to the other's version or repository"
        );
    }

    #[test]
    fn image_packages_are_plain_names() {
        let set = index(POSTGRES);
        let versioned = manifest(
            "[images.runtime]\npackages = [\"postgresql-17\"]\n\n\
             [images.old]\npackages = [\"postgresql-17 (<< 17.12)\"]\n",
        );
        // Image packages are names; versions go through holds. A name with
        // a version in it simply isn't a package.
        assert!(resolve_images(&set, &versioned).is_err());

        let grouped = manifest(
            "[groups]\nserver = [\"postgresql-17\"]\n\n\
             [images.runtime]\ngroups = [\"server\"]\n\n\
             [images.builder]\npackages = [\"postgresql-server-dev-17\"]\n",
        );
        let (_, selected) = resolve_images(&set, &grouped).unwrap();
        assert!(versions(&selected).contains(&"postgresql-17=17.12-1".to_string()));
    }

    const LIBPQ: &str = "\
Package: libpq5\nVersion: 17\n\n\
Package: libpq5\nVersion: 18\n\n\
Package: old-client\nVersion: 1\nDepends: libpq5 (<< 18)\n\n\
Package: new-client\nVersion: 1\nDepends: libpq5 (>= 18)";

    /// The first image would take the newest libpq5, but takes the older
    /// one just as well; the second image can only take the older one.
    #[test]
    fn a_later_image_can_move_an_earlier_choice() {
        let set = index(LIBPQ);
        let manifest = manifest(
            "[images.new]\npackages = [\"libpq5\"]\n\n\
             [images.old]\npackages = [\"old-client\"]\n",
        );
        let (images, selected) = resolve_images(&set, &manifest).unwrap();
        assert_eq!(versions(&selected), ["libpq5=17", "old-client=1"]);
        assert_eq!(images[0].packages, ["libpq5"]);

        // The same set in the other order agrees.
        let reversed = self::manifest(
            "[images.old]\npackages = [\"old-client\"]\n\n\
             [images.new]\npackages = [\"libpq5\"]\n",
        );
        let (_, selected) = resolve_images(&set, &reversed).unwrap();
        assert_eq!(versions(&selected), ["libpq5=17", "old-client=1"]);
    }

    #[test]
    fn a_conflict_names_the_image_that_chose_the_version() {
        let set = index(LIBPQ);
        let manifest = manifest(
            "[images.new]\npackages = [\"new-client\"]\n\n\
             [images.old]\npackages = [\"old-client\"]\n",
        );
        assert_eq!(
            resolve_images(&set, &manifest).unwrap_err().to_string(),
            "image 'new': no version of 'libpq5' satisfies every requirement: \
             (= 17) from image 'old', (>= 18) from new-client"
        );
    }

    #[test]
    fn holds_apply_across_images_and_must_be_used() {
        let set = index(POSTGRES);
        let held = manifest(
            "[holds]\npostgresql-17 = \"= 17.11-1\"\npostgresql-server-dev-17 = \"= 17.11-1\"\n\n\
             [images.runtime]\npackages = [\"postgresql-17\"]\n\n\
             [images.builder]\npackages = [\"postgresql-server-dev-17\"]\n",
        );
        let (_, selected) = resolve_images(&set, &held).unwrap();
        assert_eq!(
            versions(&selected),
            [
                "libc6=2.41",
                "postgresql-17=17.11-1",
                "postgresql-server-dev-17=17.11-1",
            ]
        );

        let typo = manifest(
            "[holds]\npostgresql-18 = \"= 18.1-1\"\n\n[images.runtime]\npackages = [\"postgresql-17\"]\n",
        );
        assert_eq!(
            resolve_images(&set, &typo).unwrap_err().to_string(),
            "the hold on 'postgresql-18' matches no package in the resolution"
        );
    }

    #[test]
    fn test_resolve_service_creation() {
        let _service = ResolveService::new().expect("Should create service");
    }
}
