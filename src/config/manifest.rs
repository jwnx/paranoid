use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, NaiveDateTime, SubsecRound, Utc};
use indexmap::IndexMap;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::arch::Architecture;
use crate::repository::DebianVersion;
use crate::version::VersionConstraint;

/// A manifest: repositories, holds, and the images built from them.
///
/// A manifest describes either one image (`[image]` and `[[packages]]`) or
/// a set of them (`[defaults]`, `[groups]` and `[images.<id>]`). Either way
/// it's a set: a single image is the set of one, with the id
/// [`DEFAULT_IMAGE`]. Every image of a set resolves against the same
/// repositories at the same time, into one lock.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// The paranoid versions this manifest builds with. Image digests
    /// depend on the binary, so a manifest can insist on one.
    pub paranoid: Option<VersionReq>,
    /// Repositories in declaration order. When several carry the same
    /// package at the same version, the earlier one wins.
    pub repositories: IndexMap<String, RepositoryConfig>,
    /// Version or repository requirements on packages wherever they end
    /// up, in any image, by package name.
    pub holds: IndexMap<String, HoldSpec>,
    /// The architecture of every image.
    pub architecture: Architecture,
    /// The images by id, in declaration order.
    pub images: IndexMap<String, ImageSpec>,
    /// Stores to fetch packages from by their SHA-256, before trying their
    /// repositories: `<store>/sha256/<2 hex>/<sha256>.deb`, the layout of
    /// the local cache. Like `mirrors`, they're left out of the config
    /// hash.
    pub stores: Vec<Url>,
    /// Written as a set (`[images.*]`) rather than a single `[image]`.
    set: bool,
    /// The directory paths in the manifest are relative to: the manifest's
    /// own directory, or the current directory when parsed from a string.
    dir: PathBuf,
}

/// The version of this paranoid.
pub fn running_version() -> Version {
    env!("CARGO_PKG_VERSION")
        .parse()
        .expect("Cargo.toml has a valid version")
}

/// The id of the image in a single-image manifest.
pub const DEFAULT_IMAGE: &str = "default";

/// One image of a manifest.
#[derive(Debug, Clone, Default)]
pub struct ImageSpec {
    pub name: Option<String>,
    pub tag: Option<String>,
    pub description: Option<String>,
    pub authors: Option<String>,
    pub source: Option<String>,
    /// Leave every package unpacked but not configured, as `dpkg --unpack`
    /// would, with its maintainer scripts in place, for `dpkg --configure`
    /// to run later in the image (say, in a Dockerfile). paranoid itself
    /// never runs them.
    pub unconfigured: bool,
    /// What the image asks for, groups expanded.
    pub packages: Vec<PackageSpec>,
    pub entrypoint: Option<EntrypointConfig>,
    /// Where `build` writes the image, as written: relative to the
    /// manifest, with `{image}` standing for the image's id. See
    /// [`Manifest::output`].
    pub output: Option<String>,
}

/// A manifest as written, in either form.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    paranoid: Option<String>,
    #[serde(default)]
    repositories: IndexMap<String, RepositoryConfig>,
    #[serde(default)]
    holds: IndexMap<String, HoldSpec>,
    download: Option<RawDownload>,

    // One image.
    image: Option<RawImageConfig>,
    #[serde(default)]
    packages: Vec<PackageSpec>,
    entrypoint: Option<EntrypointConfig>,

    // A set of images.
    defaults: Option<RawDefaults>,
    #[serde(default)]
    groups: IndexMap<String, Vec<String>>,
    #[serde(default)]
    images: IndexMap<String, RawImage>,
}

/// `[download]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDownload {
    #[serde(default, deserialize_with = "deserialize_http_urls")]
    stores: Vec<Url>,
}

/// `[image]` of a single-image manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImageConfig {
    name: Option<String>,
    tag: Option<String>,
    #[serde(deserialize_with = "deserialize_from_str")]
    architecture: Architecture,
    description: Option<String>,
    authors: Option<String>,
    source: Option<String>,
    #[serde(default)]
    unconfigured: bool,
    output: Option<String>,
}

/// `[defaults]`: what every image of a set starts from.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    #[serde(deserialize_with = "deserialize_from_str")]
    architecture: Architecture,
    tag: Option<String>,
    description: Option<String>,
    authors: Option<String>,
    source: Option<String>,
    unconfigured: Option<bool>,
    entrypoint: Option<EntrypointConfig>,
    output: Option<String>,
}

/// `[images.<id>]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImage {
    name: Option<String>,
    tag: Option<String>,
    description: Option<String>,
    authors: Option<String>,
    source: Option<String>,
    unconfigured: Option<bool>,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(default)]
    packages: Vec<String>,
    entrypoint: Option<EntrypointConfig>,
    output: Option<String>,
}

impl RawManifest {
    fn into_manifest(self) -> Result<Manifest, ManifestError> {
        let invalid = |field, message: String| ManifestError::Validation { field, message };
        let paranoid = self
            .paranoid
            .map(|req| {
                req.parse::<VersionReq>().map_err(|e| {
                    invalid(
                        "paranoid",
                        format!("'{req}' is not a version requirement: {e}"),
                    )
                })
            })
            .transpose()?;
        let stores = self.download.unwrap_or_default().stores;
        let single = self.image.is_some() || !self.packages.is_empty() || self.entrypoint.is_some();
        let set = self.defaults.is_some() || !self.groups.is_empty() || !self.images.is_empty();
        if single && set {
            return Err(invalid(
                "images",
                "describe one image with [image] and [[packages]], or a set with [defaults] \
                 and [images.*], not both"
                    .to_string(),
            ));
        }

        if !set {
            let (architecture, image) = match self.image {
                Some(i) => (
                    i.architecture,
                    ImageSpec {
                        name: i.name,
                        tag: i.tag,
                        description: i.description,
                        authors: i.authors,
                        source: i.source,
                        unconfigured: i.unconfigured,
                        packages: self.packages,
                        entrypoint: self.entrypoint,
                        output: i.output,
                    },
                ),
                None => (
                    Architecture::Amd64,
                    ImageSpec {
                        packages: self.packages,
                        entrypoint: self.entrypoint,
                        ..ImageSpec::default()
                    },
                ),
            };
            return Ok(Manifest {
                paranoid,
                repositories: self.repositories,
                holds: self.holds,
                architecture,
                images: IndexMap::from([(DEFAULT_IMAGE.to_string(), image)]),
                stores,
                set: false,
                dir: PathBuf::new(),
            });
        }

        let defaults = self.defaults.ok_or_else(|| {
            invalid(
                "defaults",
                "a set of images needs [defaults] with an architecture".to_string(),
            )
        })?;
        let mut images = IndexMap::new();
        for (id, raw) in self.images {
            if !is_image_id(&id) {
                return Err(invalid(
                    "images",
                    format!("image id '{id}' must be lower-case letters, digits, '.', '-' or '_'"),
                ));
            }
            let mut names: Vec<String> = Vec::new();
            for group in &raw.groups {
                let members = self.groups.get(group).ok_or_else(|| {
                    invalid(
                        "images",
                        format!("image '{id}' uses unknown group '{group}'"),
                    )
                })?;
                names.extend(members.iter().cloned());
            }
            names.extend(raw.packages);
            let mut seen = std::collections::HashSet::new();
            names.retain(|name| seen.insert(name.clone()));

            let image = ImageSpec {
                name: raw.name,
                tag: raw.tag.or_else(|| defaults.tag.clone()),
                description: raw.description.or_else(|| defaults.description.clone()),
                authors: raw.authors.or_else(|| defaults.authors.clone()),
                source: raw.source.or_else(|| defaults.source.clone()),
                unconfigured: raw.unconfigured.or(defaults.unconfigured).unwrap_or(false),
                packages: names
                    .into_iter()
                    .map(|name| PackageSpec {
                        name,
                        version: None,
                        repository: None,
                    })
                    .collect(),
                entrypoint: raw.entrypoint.or_else(|| defaults.entrypoint.clone()),
                output: raw.output.or_else(|| defaults.output.clone()),
            };
            images.insert(id, image);
        }
        if images.is_empty() {
            return Err(invalid(
                "images",
                "a set needs at least one [images.<id>]".to_string(),
            ));
        }

        Ok(Manifest {
            paranoid,
            repositories: self.repositories,
            holds: self.holds,
            architecture: defaults.architecture,
            images,
            stores,
            set: true,
            dir: PathBuf::new(),
        })
    }
}

/// Image ids end up in paths (`-o '{image}.tar'`), so keep them tame.
fn is_image_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '.' | '-' | '_'))
        && !id.starts_with('.')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    #[serde(deserialize_with = "deserialize_http_url")]
    pub url: Url,
    pub distribution: String,
    pub components: Vec<String>,
    /// OpenPGP keys allowed to sign the repository's `InRelease`, relative
    /// to the manifest. Like apt's `Signed-By`, but required.
    pub signed_by: PathBuf,
    /// A point in time on a snapshot service such as snapshot.debian.org:
    /// the repository is then `<url>/<snapshot>`, and its Release must have
    /// been valid at that time rather than now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotTime>,
    /// apt's pin priority: when several repositories carry a package, the
    /// highest priority wins, then the highest version, and a negative one
    /// is never used. Defaults to what apt would use: 500, or 1 or 100 for
    /// a repository whose Release says `NotAutomatic` (like backports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// More base URLs serving the same pool, tried in order when the
    /// repository doesn't have a package or is down: PGDG moves superseded
    /// versions to apt-archive, for one. Only `.deb` downloads use them,
    /// and those are checked against the lock, so they don't affect
    /// resolution and are left out of the config hash.
    #[serde(default, skip_serializing, deserialize_with = "deserialize_http_urls")]
    pub mirrors: Vec<Url>,
}

impl RepositoryConfig {
    /// The repository's root URL, without a trailing slash: `url`, or
    /// `url/snapshot` for a snapshot.
    pub fn base_url(&self) -> String {
        let url = self.url.as_str().trim_end_matches('/');
        match &self.snapshot {
            Some(snapshot) => format!("{url}/{snapshot}"),
            None => url.to_string(),
        }
    }
}

/// A snapshot timestamp as snapshot.debian.org and snapshot.ubuntu.com
/// write it: `20261005T000000Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotTime(DateTime<Utc>);

const SNAPSHOT_FORMAT: &str = "%Y%m%dT%H%M%SZ";

impl SnapshotTime {
    /// The current time, to the second.
    pub fn now() -> Self {
        Self(Utc::now().trunc_subsecs(0))
    }

    pub fn time(&self) -> DateTime<Utc> {
        self.0
    }
}

impl FromStr for SnapshotTime {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        NaiveDateTime::parse_from_str(s, SNAPSHOT_FORMAT)
            .map(|t| Self(t.and_utc()))
            .map_err(|_| format!("invalid snapshot '{s}': expected a time like 20261005T000000Z"))
    }
}

impl std::fmt::Display for SnapshotTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.format(SNAPSHOT_FORMAT))
    }
}

impl Serialize for SnapshotTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SnapshotTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_from_str(deserializer)
    }
}

/// Deserialize any `FromStr` type from a string, so aliases and validation
/// live in one place.
fn deserialize_from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: FromStr,
    T::Err: std::fmt::Display,
{
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}

/// Custom deserializer that validates URLs are http or https.
fn deserialize_http_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    let url = Url::parse(&s).map_err(serde::de::Error::custom)?;
    check_base_url(&url).map_err(serde::de::Error::custom)?;
    Ok(url)
}

/// Check a URL that paths get appended to: a repository's, a mirror's or a
/// store's. A query or fragment would end up in front of the path, and
/// credentials in it would end up in every error message.
pub fn check_base_url(url: &Url) -> Result<(), String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!(
            "URL must use http or https scheme, got '{}'",
            url.scheme()
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "'{url}' has a query or fragment, but paths are appended to it"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "'{}' has credentials in it, which would show up in logs",
            url.host_str().unwrap_or_default()
        ));
    }
    Ok(())
}

fn deserialize_http_urls<'de, D>(deserializer: D) -> Result<Vec<Url>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct HttpUrl(#[serde(deserialize_with = "deserialize_http_url")] Url);

    Ok(Vec::<HttpUrl>::deserialize(deserializer)?
        .into_iter()
        .map(|u| u.0)
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSpec {
    pub name: String,
    /// `">= 1.0"`, or a bare version meaning `"= 1.0"`.
    pub version: Option<VersionConstraint<DebianVersion>>,
    /// Optional repository name to pin this package to.
    /// If not specified, the package will be searched in all repositories.
    pub repository: Option<String>,
}

/// A hold: `"= 2.58.0-1"`, or `{ version = "...", repository = "..." }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HoldSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionConstraint<DebianVersion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl<'de> Deserialize<'de> for HoldSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Table {
            version: Option<VersionConstraint<DebianVersion>>,
            repository: Option<String>,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Version(VersionConstraint<DebianVersion>),
            Table(Table),
        }

        match Raw::deserialize(deserializer).map_err(|_| {
            serde::de::Error::custom(
                "expected a version like \"= 2.58.0-1\", or a table with `version` and/or `repository`",
            )
        })? {
            Raw::Version(version) => Ok(Self {
                version: Some(version),
                repository: None,
            }),
            Raw::Table(Table { version, repository }) => Ok(Self { version, repository }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrypointConfig {
    pub cmd: Vec<String>,
    /// Environment variables, kept in declaration order so the image config
    /// is stable across builds.
    pub env: Option<IndexMap<String, String>>,
    pub workdir: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Yaml,
    Json,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    ParseToml(#[from] toml::de::Error),

    #[error("{0}")]
    EditToml(#[from] toml_edit::TomlError),

    #[error("YAML parse error: {0}")]
    ParseYaml(#[from] serde_yaml::Error),

    #[error("JSON parse error: {0}")]
    ParseJson(#[from] serde_json::Error),

    #[error("unknown config format: '{extension}'. Supported: .toml, .yaml, .yml, .json")]
    UnknownFormat { extension: String },

    #[error(
        "no config file found in '{dir}'. Expected paranoid.toml, paranoid.yaml, paranoid.yml, or paranoid.json"
    )]
    NotFound { dir: PathBuf },

    #[error("multiple config files found: {files:?}. Please keep only one.")]
    MultipleFound { files: Vec<PathBuf> },

    #[error("this manifest requires paranoid {required}, but this is paranoid {running}")]
    Version {
        required: VersionReq,
        running: Version,
    },

    #[error("validation error in '{field}': {message}")]
    Validation {
        field: &'static str,
        message: String,
    },
}

impl FromStr for Manifest {
    type Err = ManifestError;

    /// Parses a manifest from a string, assuming TOML format.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_toml(s)
    }
}

/// All supported config file names.
const CONFIG_FILE_NAMES: &[&str] = &[
    "paranoid.toml",
    "paranoid.yaml",
    "paranoid.yml",
    "paranoid.json",
];

impl Manifest {
    /// Load a manifest from a file, or find one in a directory.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        Self::from_file(&Self::locate(path)?)
    }

    /// The manifest file at `path`: `path` itself, or the one manifest in
    /// it if it's a directory.
    pub fn locate(path: &Path) -> Result<PathBuf, ManifestError> {
        if path.is_dir() {
            Self::find_in(path)
        } else {
            Ok(path.to_path_buf())
        }
    }

    /// Where the lock file for the manifest at `path` lives by default:
    /// `paranoid.lock` in the manifest's directory.
    pub fn default_lock_path(path: &Path) -> PathBuf {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(Path::new("."))
        };
        dir.join("paranoid.lock")
    }

    /// Finds and loads a manifest from a directory.
    ///
    /// Looks for `paranoid.toml`, `paranoid.yaml`, `paranoid.yml`, or `paranoid.json`.
    /// Returns an error if no config file is found or if multiple are present.
    pub fn from_dir(dir: &Path) -> Result<Self, ManifestError> {
        Self::from_file(&Self::find_in(dir)?)
    }

    /// The one manifest file in `dir`.
    fn find_in(dir: &Path) -> Result<PathBuf, ManifestError> {
        let mut found: Vec<PathBuf> = CONFIG_FILE_NAMES
            .iter()
            .map(|name| dir.join(name))
            .filter(|path| path.exists())
            .collect();

        match found.len() {
            0 => Err(ManifestError::NotFound {
                dir: dir.to_path_buf(),
            }),
            1 => Ok(found.remove(0)),
            _ => Err(ManifestError::MultipleFound { files: found }),
        }
    }

    /// Loads a manifest from a file, detecting format from extension.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_file_content(path, &content)
    }

    /// Parses `content` as the manifest at `path`: in the format its
    /// extension says, with paths relative to its directory.
    pub fn from_file_content(path: &Path, content: &str) -> Result<Self, ManifestError> {
        let format = Self::detect_format(path)?;
        let mut manifest = Self::from_str_with_format(content, format)?;
        manifest.dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        Ok(manifest)
    }

    /// Resolve a path written in the manifest against its directory.
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        self.dir.join(path)
    }

    /// Detects the config format from a file's extension.
    pub fn detect_format(path: &Path) -> Result<ConfigFormat, ManifestError> {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match extension {
            "toml" => Ok(ConfigFormat::Toml),
            "yaml" | "yml" => Ok(ConfigFormat::Yaml),
            "json" => Ok(ConfigFormat::Json),
            other => Err(ManifestError::UnknownFormat {
                extension: other.to_string(),
            }),
        }
    }

    /// Parses a manifest from a string using the specified format.
    pub fn from_str_with_format(
        content: &str,
        format: ConfigFormat,
    ) -> Result<Self, ManifestError> {
        let manifest = match format {
            ConfigFormat::Toml => Self::from_toml(content)?,
            ConfigFormat::Yaml => Self::from_yaml(content)?,
            ConfigFormat::Json => Self::from_json(content)?,
        };
        Ok(manifest)
    }

    /// Parses a manifest from a TOML string.
    pub fn from_toml(content: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(content)?;
        Self::from_raw(raw)
    }

    /// Parses a manifest from a YAML string.
    pub fn from_yaml(content: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_yaml::from_str(content)?;
        Self::from_raw(raw)
    }

    /// Parses a manifest from a JSON string.
    pub fn from_json(content: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_json::from_str(content)?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawManifest) -> Result<Self, ManifestError> {
        let manifest = raw.into_manifest()?;
        manifest.validate()?;
        manifest.check_version(&running_version())?;
        Ok(manifest)
    }

    /// Fail unless `running` meets the manifest's `paranoid` requirement.
    pub fn check_version(&self, running: &Version) -> Result<(), ManifestError> {
        match &self.paranoid {
            Some(required) if !required.matches(running) => Err(ManifestError::Version {
                required: required.clone(),
                running: running.clone(),
            }),
            _ => Ok(()),
        }
    }

    /// Where `build` writes image `id`, if the manifest says: its `output`
    /// with `{image}` replaced, relative to the manifest.
    pub fn output(&self, id: &str) -> Option<PathBuf> {
        let template = self.images.get(id)?.output.as_ref()?;
        Some(self.resolve_path(Path::new(&template.replace("{image}", id))))
    }

    /// Whether the manifest describes a set of images (`[images.*]`).
    pub fn is_set(&self) -> bool {
        self.set
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.repositories.is_empty() {
            return Err(ManifestError::Validation {
                field: "repositories",
                message: "at least one repository must be defined".to_string(),
            });
        }

        if !self.architecture.is_concrete() {
            return Err(ManifestError::Validation {
                field: if self.set {
                    "defaults.architecture"
                } else {
                    "image.architecture"
                },
                message: format!("'{}' is not a concrete CPU architecture", self.architecture),
            });
        }

        for (name, repo) in &self.repositories {
            // URL validation is handled by serde deserializer (deserialize_http_url)

            if repo.distribution.is_empty() {
                return Err(ManifestError::Validation {
                    field: "repositories",
                    message: format!("repository '{}' has an empty distribution", name),
                });
            }

            if repo.signed_by.as_os_str().is_empty() {
                return Err(ManifestError::Validation {
                    field: "repositories",
                    message: format!("repository '{}' has an empty signed_by", name),
                });
            }

            if repo.components.is_empty() {
                return Err(ManifestError::Validation {
                    field: "repositories",
                    message: format!("repository '{}' must have at least one component", name),
                });
            }
        }

        for (id, image) in &self.images {
            if image.packages.is_empty() {
                return Err(ManifestError::Validation {
                    field: "packages",
                    message: if self.set {
                        format!("image '{id}' asks for no packages")
                    } else {
                        "at least one package must be specified".to_string()
                    },
                });
            }
        }

        for pkg in self.images.values().flat_map(|i| &i.packages) {
            if pkg.name.is_empty() {
                return Err(ManifestError::Validation {
                    field: "packages",
                    message: "package name cannot be empty".to_string(),
                });
            }
            if let Some(repo) = &pkg.repository
                && !self.repositories.contains_key(repo)
            {
                return Err(ManifestError::Validation {
                    field: "packages",
                    message: format!(
                        "package '{}' is pinned to unknown repository '{}'",
                        pkg.name, repo
                    ),
                });
            }
        }

        let mut outputs: HashMap<PathBuf, &str> = HashMap::new();
        for id in self.images.keys() {
            let Some(output) = self.output(id) else {
                continue;
            };
            if let Some(other) = outputs.insert(output.clone(), id) {
                return Err(ManifestError::Validation {
                    field: "output",
                    message: format!(
                        "images '{other}' and '{id}' would both be written to '{}'; \
                         put {{image}} in output",
                        output.display()
                    ),
                });
            }
        }

        for (name, hold) in &self.holds {
            if hold.version.is_none() && hold.repository.is_none() {
                return Err(ManifestError::Validation {
                    field: "holds",
                    message: format!("hold on '{name}' sets neither a version nor a repository"),
                });
            }
            if let Some(repo) = &hold.repository
                && !self.repositories.contains_key(repo)
            {
                return Err(ManifestError::Validation {
                    field: "holds",
                    message: format!("'{name}' is held to unknown repository '{repo}'"),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(extra: &str) -> Result<RepositoryConfig, ManifestError> {
        let toml = format!(
            "[repositories.debian]\n\
             url = \"https://snapshot.debian.org/archive/debian/\"\n\
             distribution = \"trixie\"\n\
             components = [\"main\"]\n\
             signed_by = \"keys/debian.asc\"\n{extra}\n\
             [[packages]]\nname = \"bash\"\n"
        );
        Manifest::from_str(&toml).map(|m| m.repositories["debian"].clone())
    }

    #[test]
    fn parses_holds() {
        let manifest = Manifest::from_str(
            r#"
[repositories.pgdg]
url = "https://apt.postgresql.org/pub/repos/apt"
distribution = "trixie-pgdg"
components = ["main"]
signed_by = "keys/pgdg.asc"

[holds]
pgbackrest = "= 2.58.0-1.pgdg13+1"
libpq5 = { repository = "pgdg" }
postgresql-17 = { version = ">= 17.11", repository = "pgdg" }

[[packages]]
name = "pgbackrest"
"#,
        )
        .unwrap();
        let holds: Vec<_> = manifest
            .holds
            .iter()
            .map(|(name, h)| {
                let version = h.version.as_ref().map(ToString::to_string);
                (name.as_str(), version, h.repository.as_deref())
            })
            .collect();
        assert_eq!(
            holds,
            [
                ("pgbackrest", Some("= 2.58.0-1.pgdg13+1".to_string()), None),
                ("libpq5", None, Some("pgdg")),
                ("postgresql-17", Some(">= 17.11".to_string()), Some("pgdg")),
            ]
        );
    }

    #[test]
    fn rejects_bad_holds() {
        let with = |holds: &str| {
            Manifest::from_str(&format!(
                "[repositories.pgdg]\nurl = \"https://example.org\"\ndistribution = \"x\"\n\
                 components = [\"main\"]\nsigned_by = \"k.asc\"\n\n[holds]\n{holds}\n\n\
                 [[packages]]\nname = \"bash\"\n"
            ))
            .unwrap_err()
            .to_string()
        };
        assert!(with("x = {}").contains("neither a version nor a repository"));
        assert!(with("x = { repository = \"nope\" }").contains("unknown repository 'nope'"));
        assert!(with("x = \">> \"").contains("expected a version"));
        assert!(with("x = { versoin = \"1\" }").contains("expected a version"));
    }

    const SET_REPOS: &str = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "trixie"
components = ["main"]
signed_by = "keys/debian.asc"
"#;

    fn set(rest: &str) -> Result<Manifest, ManifestError> {
        Manifest::from_str(&format!("{SET_REPOS}\n{rest}"))
    }

    #[test]
    fn parses_a_set_of_images() {
        let manifest = set(r#"
[defaults]
architecture = "arm64"
unconfigured = true
tag = "v1"
entrypoint = { cmd = ["/bin/sh"] }

[holds]
pgbackrest = "= 2.58.0-1"

[groups]
essential = ["bash", "coreutils"]
runtime = ["bash", "pgbackrest"]

[images.pg17]
name = "mpg-pg17-base"
groups = ["essential", "runtime"]
packages = ["postgresql-17", "coreutils"]

[images.pg17-builder]
name = "mpg-pg17-builder-base"
groups = ["essential"]
packages = ["postgresql-server-dev-17"]
unconfigured = false
entrypoint = { cmd = ["make"] }
"#)
        .unwrap();
        assert!(manifest.is_set());
        assert_eq!(manifest.architecture, Architecture::Arm64);
        let ids: Vec<_> = manifest.images.keys().collect();
        assert_eq!(ids, ["pg17", "pg17-builder"]);

        let names = |id: &str| -> Vec<String> {
            manifest.images[id]
                .packages
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        // Groups first, in order, then packages; each name once.
        assert_eq!(
            names("pg17"),
            ["bash", "coreutils", "pgbackrest", "postgresql-17"]
        );
        assert_eq!(
            names("pg17-builder"),
            ["bash", "coreutils", "postgresql-server-dev-17"]
        );

        let pg17 = &manifest.images["pg17"];
        assert_eq!(pg17.name.as_deref(), Some("mpg-pg17-base"));
        assert_eq!(pg17.tag.as_deref(), Some("v1"));
        assert!(pg17.unconfigured);
        assert_eq!(pg17.entrypoint.as_ref().unwrap().cmd, ["/bin/sh"]);
        let builder = &manifest.images["pg17-builder"];
        assert!(!builder.unconfigured);
        assert_eq!(builder.entrypoint.as_ref().unwrap().cmd, ["make"]);
        assert_eq!(manifest.holds.len(), 1);
    }

    #[test]
    fn rejects_bad_sets() {
        let message = |rest: &str| set(rest).unwrap_err().to_string();
        let one = "[images.a]\npackages = [\"bash\"]\n";
        assert!(message(one).contains("needs [defaults] with an architecture"));
        let defaults = "[defaults]\narchitecture = \"amd64\"\n";
        assert!(
            message(&format!("{defaults}[images.a]\ngroups = [\"nope\"]\n"))
                .contains("image 'a' uses unknown group 'nope'")
        );
        assert!(
            message(&format!("{defaults}[images.a]\n")).contains("image 'a' asks for no packages")
        );
        assert!(
            message(&format!("{defaults}[images.A]\npackages = [\"bash\"]\n"))
                .contains("image id 'A'")
        );
        assert!(message(defaults).contains("at least one [images.<id>]"));
        assert!(
            message(&format!("{defaults}{one}\n[[packages]]\nname = \"bash\"\n"))
                .contains("not both")
        );
        // Architecture is set-wide.
        assert!(
            message(&format!(
                "{defaults}[images.a]\narchitecture = \"arm64\"\npackages = [\"bash\"]\n"
            ))
            .contains("unknown field `architecture`")
        );
    }

    #[test]
    fn outputs_come_from_defaults_or_the_image() {
        let manifest = set(
            "[defaults]\narchitecture = \"amd64\"\noutput = \"oci/{image}\"\n\n\
             [images.a]\npackages = [\"bash\"]\n\n\
             [images.b]\npackages = [\"bash\"]\noutput = \"b.tar\"\n",
        )
        .unwrap();
        assert_eq!(manifest.output("a"), Some(PathBuf::from("oci/a")));
        assert_eq!(manifest.output("b"), Some(PathBuf::from("b.tar")));
        assert_eq!(manifest.output("c"), None);

        let single = Manifest::from_str(&format!(
            "{SET_REPOS}\n[image]\narchitecture = \"amd64\"\noutput = \"out\"\n\n\
             [[packages]]\nname = \"bash\"\n"
        ))
        .unwrap();
        assert_eq!(single.output(DEFAULT_IMAGE), Some(PathBuf::from("out")));
    }

    #[test]
    fn two_images_cant_share_an_output() {
        let err = set(
            "[defaults]\narchitecture = \"amd64\"\noutput = \"image.tar\"\n\n\
             [images.a]\npackages = [\"bash\"]\n\n[images.b]\npackages = [\"bash\"]\n",
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "validation error in 'output': images 'a' and 'b' would both be written to \
             'image.tar'; put {image} in output"
        );
    }

    #[test]
    fn checks_the_required_paranoid_version() {
        let with = |req: &str| {
            Manifest::from_str(&format!(
                "paranoid = \"{req}\"\n{SET_REPOS}\n[[packages]]\nname = \"bash\"\n"
            ))
        };
        let running = running_version();
        let compatible = format!("~{}.{}", running.major, running.minor);
        assert!(with(&compatible).is_ok());

        let manifest = with(&compatible).unwrap();
        let err = manifest
            .check_version(&Version::new(running.major, running.minor + 1, 0))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "this manifest requires paranoid {compatible}, but this is paranoid {}.{}.0",
                running.major,
                running.minor + 1
            )
        );
        assert!(with("=0.0.1").is_err());
        assert!(with("not a version").is_err());
    }

    #[test]
    fn a_single_image_is_the_set_of_one() {
        let manifest =
            Manifest::from_str(&format!("{SET_REPOS}\n[[packages]]\nname = \"bash\"\n")).unwrap();
        assert!(!manifest.is_set());
        assert_eq!(manifest.images.keys().collect::<Vec<_>>(), [DEFAULT_IMAGE]);
        assert_eq!(manifest.architecture, Architecture::Amd64);
    }

    #[test]
    fn a_snapshot_is_appended_to_the_url() {
        let repo = repository("snapshot = \"20261005T000000Z\"").unwrap();
        assert_eq!(
            repo.base_url(),
            "https://snapshot.debian.org/archive/debian/20261005T000000Z"
        );
        assert_eq!(
            repo.snapshot.unwrap().time(),
            "2026-10-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let plain = repository("").unwrap();
        assert_eq!(
            plain.base_url(),
            "https://snapshot.debian.org/archive/debian"
        );
    }

    #[test]
    fn parses_stores() {
        let with = |download: &str| {
            Manifest::from_str(&format!(
                "{SET_REPOS}\n[download]\n{download}\n\n[[packages]]\nname = \"bash\"\n"
            ))
        };
        let manifest =
            with("stores = [\"https://bucket.example.org/debs/\", \"http://cache.lan\"]").unwrap();
        let stores: Vec<&str> = manifest.stores.iter().map(Url::as_str).collect();
        assert_eq!(
            stores,
            ["https://bucket.example.org/debs/", "http://cache.lan/"]
        );

        let err = with("stores = [\"s3://bucket\"]").unwrap_err();
        assert!(err.to_string().contains("http or https"), "{err}");
        let err = with("stores = [\"https://b.example.org/debs?token=x\"]").unwrap_err();
        assert!(err.to_string().contains("has a query or fragment"), "{err}");
        let err = with("stores = [\"https://b.example.org/debs#x\"]").unwrap_err();
        assert!(err.to_string().contains("has a query or fragment"), "{err}");
        let err = with("stores = [\"https://user:secret@b.example.org/debs\"]").unwrap_err();
        assert!(err.to_string().contains("has credentials"), "{err}");
        let err = with("store = []").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn mirrors_stay_out_of_the_config_hash() {
        let repo =
            repository("mirrors = [\"https://apt-archive.postgresql.org/pub/repos/apt\"]").unwrap();
        assert_eq!(repo.mirrors.len(), 1);
        let json = serde_json::to_string(&repo).unwrap();
        assert!(!json.contains("mirrors"), "{json}");

        let err = repository("mirrors = [\"ftp://example.org\"]").unwrap_err();
        assert!(err.to_string().contains("http or https"), "{err}");
    }

    #[test]
    fn rejects_malformed_snapshots() {
        for bad in [
            "2026-10-05",
            "20261005",
            "20261005T000000",
            "20261305T000000Z",
        ] {
            let err = repository(&format!("snapshot = \"{bad}\"")).unwrap_err();
            assert!(err.to_string().contains("invalid snapshot"), "{bad}: {err}");
        }
    }

    #[test]
    fn parse_valid_config() {
        let config = r#"
[image]
name = "my-container"
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main", "contrib"]
signed_by = "keys/debian.asc"

[[packages]]
name = "nginx"

[[packages]]
name = "curl"
version = "7.88.1-10"

[entrypoint]
cmd = ["/usr/sbin/nginx", "-g", "daemon off;"]
workdir = "/var/www"

[entrypoint.env]
PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin"
"#;

        let manifest = Manifest::from_str(config).unwrap();

        assert!(!manifest.is_set());
        let image = manifest.images[DEFAULT_IMAGE].clone();
        assert_eq!(image.name, Some("my-container".to_string()));
        assert_eq!(manifest.architecture, Architecture::Amd64);
        assert!(image.tag.is_none());
        assert!(image.description.is_none());
        assert!(image.authors.is_none());
        assert!(image.source.is_none());

        assert_eq!(manifest.repositories.len(), 1);
        let debian = &manifest.repositories["debian"];
        assert_eq!(debian.url.as_str(), "https://deb.debian.org/debian");
        assert_eq!(debian.distribution, "bookworm");
        assert_eq!(debian.components, vec!["main", "contrib"]);

        assert_eq!(manifest.images[DEFAULT_IMAGE].packages.len(), 2);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "nginx");
        assert!(manifest.images[DEFAULT_IMAGE].packages[0].version.is_none());
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[1].name, "curl");
        assert_eq!(
            manifest.images[DEFAULT_IMAGE].packages[1]
                .version
                .as_ref()
                .map(ToString::to_string),
            Some("= 7.88.1-10".to_string())
        );

        assert!(manifest.images[DEFAULT_IMAGE].entrypoint.is_some());
        let entrypoint = manifest.images[DEFAULT_IMAGE].entrypoint.clone().unwrap();
        assert_eq!(entrypoint.cmd, vec!["/usr/sbin/nginx", "-g", "daemon off;"]);
        assert_eq!(entrypoint.workdir, Some("/var/www".to_string()));
        assert!(entrypoint.env.is_some());
    }

    #[test]
    fn parse_minimal_config() {
        let config = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;

        let manifest = Manifest::from_str(config).unwrap();

        assert!(manifest.images[DEFAULT_IMAGE].name.is_none());
        assert!(manifest.images[DEFAULT_IMAGE].entrypoint.is_none());
        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages.len(), 1);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "bash");
    }

    #[test]
    fn error_on_missing_repositories() {
        let config = r#"
[[packages]]
name = "bash"
"#;

        let result = Manifest::from_str(config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            ManifestError::Validation {
                field: "repositories",
                ..
            }
        ));
    }

    #[test]
    fn error_on_missing_packages() {
        let config = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"
"#;

        let result = Manifest::from_str(config);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            ManifestError::Validation {
                field: "packages",
                ..
            }
        ));
    }

    #[test]
    fn error_on_malformed_toml() {
        let config = r#"
[repositories.debian
url = "https://deb.debian.org/debian"
"#;

        let result = Manifest::from_str(config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ManifestError::ParseToml(_)));
    }

    #[test]
    fn error_on_invalid_repository_url() {
        // ftp:// is rejected during deserialization (only http/https allowed)
        let config = r#"
[repositories.debian]
url = "ftp://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;

        let result = Manifest::from_str(config);
        assert!(result.is_err());
        // Now caught during parsing, not validation
        assert!(matches!(result.unwrap_err(), ManifestError::ParseToml(_)));
    }

    #[test]
    fn handle_optional_fields() {
        let config = r#"
[image]
architecture = "arm64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "vim"
"#;

        let manifest = Manifest::from_str(config).unwrap();

        let image = manifest.images[DEFAULT_IMAGE].clone();
        assert!(image.name.is_none());
        assert_eq!(manifest.architecture, Architecture::Arm64);
    }

    #[test]
    fn parse_image_metadata_fields() {
        let config = r#"
[image]
name = "myapp/node"
tag = "1.0"
architecture = "amd64"
description = "Node.js runtime image"
authors = "ops-team <ops@example.com>"
source = "https://github.com/example/node-image"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "nodejs"
"#;

        let manifest = Manifest::from_str(config).unwrap();
        let image = manifest.images[DEFAULT_IMAGE].clone();
        assert_eq!(image.name, Some("myapp/node".to_string()));
        assert_eq!(image.tag, Some("1.0".to_string()));
        assert_eq!(image.description, Some("Node.js runtime image".to_string()));
        assert_eq!(
            image.authors,
            Some("ops-team <ops@example.com>".to_string())
        );
        assert_eq!(
            image.source,
            Some("https://github.com/example/node-image".to_string())
        );
    }

    #[test]
    fn parse_yaml_config() {
        let config = r#"
repositories:
  debian:
    url: https://deb.debian.org/debian
    distribution: bookworm
    components:
      - main
      - contrib
    signed_by: keys/debian.asc

packages:
  - name: nginx
  - name: curl
    version: "7.88.1-10"
"#;

        let manifest = Manifest::from_yaml(config).unwrap();

        assert_eq!(manifest.repositories.len(), 1);
        let debian = &manifest.repositories["debian"];
        assert_eq!(debian.url.as_str(), "https://deb.debian.org/debian");
        assert_eq!(debian.components, vec!["main", "contrib"]);

        assert_eq!(manifest.images[DEFAULT_IMAGE].packages.len(), 2);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "nginx");
        assert_eq!(
            manifest.images[DEFAULT_IMAGE].packages[1]
                .version
                .as_ref()
                .map(ToString::to_string),
            Some("= 7.88.1-10".to_string())
        );
    }

    #[test]
    fn parse_json_config() {
        let config = r#"{
  "repositories": {
    "debian": {
      "url": "https://deb.debian.org/debian",
      "distribution": "bookworm",
      "components": ["main"],
      "signed_by": "keys/debian.asc"
    }
  },
  "packages": [
    { "name": "bash" }
  ]
}"#;

        let manifest = Manifest::from_json(config).unwrap();

        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages.len(), 1);
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "bash");
    }

    #[test]
    fn error_on_malformed_yaml() {
        let config = r#"
repositories:
  debian:
    url: https://deb.debian.org/debian
    distribution: [invalid yaml
"#;

        let result = Manifest::from_yaml(config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ManifestError::ParseYaml(_)));
    }

    #[test]
    fn error_on_malformed_json() {
        let config = r#"{ "packages": [ { "name": "bash" }"#;

        let result = Manifest::from_json(config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ManifestError::ParseJson(_)));
    }

    #[test]
    fn detect_format_from_extension() {
        assert_eq!(
            Manifest::detect_format(Path::new("config.toml")).unwrap(),
            ConfigFormat::Toml
        );
        assert_eq!(
            Manifest::detect_format(Path::new("config.yaml")).unwrap(),
            ConfigFormat::Yaml
        );
        assert_eq!(
            Manifest::detect_format(Path::new("config.yml")).unwrap(),
            ConfigFormat::Yaml
        );
        assert_eq!(
            Manifest::detect_format(Path::new("config.json")).unwrap(),
            ConfigFormat::Json
        );
    }

    #[test]
    fn error_on_unknown_extension() {
        let result = Manifest::detect_format(Path::new("config.xml"));
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ManifestError::UnknownFormat { extension } if extension == "xml"
        ));
    }

    #[test]
    fn from_dir_loads_single_toml() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("paranoid.toml");
        std::fs::write(
            &config_path,
            r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#,
        )
        .unwrap();

        let manifest = Manifest::from_dir(dir.path()).unwrap();
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "bash");
    }

    #[test]
    fn from_dir_loads_single_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("paranoid.yaml");
        std::fs::write(
            &config_path,
            r#"
repositories:
  debian:
    url: https://deb.debian.org/debian
    distribution: bookworm
    components: [main]
    signed_by: keys/debian.asc
packages:
  - name: vim
"#,
        )
        .unwrap();

        let manifest = Manifest::from_dir(dir.path()).unwrap();
        assert_eq!(manifest.images[DEFAULT_IMAGE].packages[0].name, "vim");
    }

    #[test]
    fn from_dir_error_on_no_config() {
        let dir = tempfile::tempdir().unwrap();

        let result = Manifest::from_dir(dir.path());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ManifestError::NotFound { .. }
        ));
    }

    #[test]
    fn from_dir_error_on_multiple_configs() {
        let dir = tempfile::tempdir().unwrap();

        // Create both toml and yaml files
        std::fs::write(
            dir.path().join("paranoid.toml"),
            r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"
[[packages]]
name = "bash"
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("paranoid.yaml"),
            r#"
repositories:
  debian:
    url: https://deb.debian.org/debian
    distribution: bookworm
    components: [main]
    signed_by: keys/debian.asc
packages:
  - name: bash
"#,
        )
        .unwrap();

        let result = Manifest::from_dir(dir.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            ManifestError::MultipleFound { files } => {
                assert_eq!(files.len(), 2);
            }
            other => panic!("Expected MultipleFound, got {:?}", other),
        }
    }

    const REPO_AND_PACKAGE: &str = r#"
[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;

    #[test]
    fn error_on_meta_architecture() {
        let config = format!("[image]\narchitecture = \"all\"\n{REPO_AND_PACKAGE}");
        let err = Manifest::from_str(&config).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::Validation {
                    field: "image.architecture",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn architecture_accepts_aliases() {
        let config = format!("[image]\narchitecture = \"x86_64\"\n{REPO_AND_PACKAGE}");
        let manifest = Manifest::from_str(&config).unwrap();
        assert_eq!(manifest.architecture, Architecture::Amd64);
    }

    #[test]
    fn error_on_unknown_field() {
        let config = format!("{REPO_AND_PACKAGE}recommends = true\n");
        assert!(Manifest::from_str(&config).is_err());
    }

    #[test]
    fn error_on_pin_to_unknown_repository() {
        let config = format!("{REPO_AND_PACKAGE}repository = \"nope\"\n");
        let err = Manifest::from_str(&config).unwrap_err();
        assert!(
            matches!(
                err,
                ManifestError::Validation {
                    field: "packages",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn repositories_keep_declaration_order() {
        let config = r#"
[repositories.zeta]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]
signed_by = "keys/debian.asc"

[repositories.alpha]
url = "https://deb.debian.org/debian-security"
distribution = "bookworm-security"
components = ["main"]
signed_by = "keys/debian.asc"

[[packages]]
name = "bash"
"#;
        let manifest = Manifest::from_str(config).unwrap();
        let names: Vec<_> = manifest.repositories.keys().map(String::as_str).collect();
        assert_eq!(names, ["zeta", "alpha"]);
    }

    /// A bad version constraint is a manifest error, not a resolve error.
    #[test]
    fn rejects_invalid_version_constraints() {
        for version in ["> 1.0", ">= ", "1.0-", "abc:1"] {
            let config = format!(
                "[repositories.debian]\nurl = \"https://deb.debian.org/debian\"\n\
                 distribution = \"bookworm\"\ncomponents = [\"main\"]\nsigned_by = \"keys/debian.asc\"\n\n\
                 [[packages]]\nname = \"bash\"\nversion = \"{version}\"\n"
            );
            assert!(
                matches!(
                    Manifest::from_str(&config),
                    Err(ManifestError::ParseToml(_))
                ),
                "accepted {version:?}"
            );
        }
    }
}
