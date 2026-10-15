use std::path::{Path, PathBuf};
use std::str::FromStr;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::arch::Architecture;
use crate::repository::DebianVersion;
use crate::version::VersionConstraint;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub image: Option<ImageConfig>,
    /// Repositories in declaration order. When several carry the same
    /// package at the same version, the earlier one wins.
    #[serde(default)]
    pub repositories: IndexMap<String, RepositoryConfig>,
    #[serde(default)]
    pub packages: Vec<PackageSpec>,
    pub entrypoint: Option<EntrypointConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageConfig {
    pub name: Option<String>,
    pub tag: Option<String>,
    #[serde(deserialize_with = "deserialize_from_str")]
    pub architecture: Architecture,
    pub description: Option<String>,
    pub authors: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    #[serde(deserialize_with = "deserialize_http_url")]
    pub url: Url,
    pub distribution: String,
    pub components: Vec<String>,
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
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(serde::de::Error::custom(format!(
            "URL must use http or https scheme, got '{}'",
            url.scheme()
        )));
    }
    Ok(url)
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
        if path.is_dir() {
            Self::from_dir(path)
        } else {
            Self::from_file(path)
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
        let found: Vec<PathBuf> = CONFIG_FILE_NAMES
            .iter()
            .map(|name| dir.join(name))
            .filter(|path| path.exists())
            .collect();

        match found.len() {
            0 => Err(ManifestError::NotFound {
                dir: dir.to_path_buf(),
            }),
            1 => Self::from_file(&found[0]),
            _ => Err(ManifestError::MultipleFound { files: found }),
        }
    }

    /// Loads a manifest from a file, detecting format from extension.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let format = Self::detect_format(path)?;
        let content = std::fs::read_to_string(path)?;
        Self::from_str_with_format(&content, format)
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
        let manifest: Manifest = toml::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses a manifest from a YAML string.
    pub fn from_yaml(content: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_yaml::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses a manifest from a JSON string.
    pub fn from_json(content: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_json::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.repositories.is_empty() {
            return Err(ManifestError::Validation {
                field: "repositories",
                message: "at least one repository must be defined".to_string(),
            });
        }

        if let Some(image) = &self.image
            && !image.architecture.is_concrete()
        {
            return Err(ManifestError::Validation {
                field: "image.architecture",
                message: format!(
                    "'{}' is not a concrete CPU architecture",
                    image.architecture
                ),
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

            if repo.components.is_empty() {
                return Err(ManifestError::Validation {
                    field: "repositories",
                    message: format!("repository '{}' must have at least one component", name),
                });
            }
        }

        if self.packages.is_empty() {
            return Err(ManifestError::Validation {
                field: "packages",
                message: "at least one package must be specified".to_string(),
            });
        }

        for pkg in &self.packages {
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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert!(manifest.image.is_some());
        let image = manifest.image.unwrap();
        assert_eq!(image.name, Some("my-container".to_string()));
        assert_eq!(image.architecture, Architecture::Amd64);
        assert!(image.tag.is_none());
        assert!(image.description.is_none());
        assert!(image.authors.is_none());
        assert!(image.source.is_none());

        assert_eq!(manifest.repositories.len(), 1);
        let debian = &manifest.repositories["debian"];
        assert_eq!(debian.url.as_str(), "https://deb.debian.org/debian");
        assert_eq!(debian.distribution, "bookworm");
        assert_eq!(debian.components, vec!["main", "contrib"]);

        assert_eq!(manifest.packages.len(), 2);
        assert_eq!(manifest.packages[0].name, "nginx");
        assert!(manifest.packages[0].version.is_none());
        assert_eq!(manifest.packages[1].name, "curl");
        assert_eq!(
            manifest.packages[1]
                .version
                .as_ref()
                .map(ToString::to_string),
            Some("= 7.88.1-10".to_string())
        );

        assert!(manifest.entrypoint.is_some());
        let entrypoint = manifest.entrypoint.unwrap();
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

[[packages]]
name = "bash"
"#;

        let manifest = Manifest::from_str(config).unwrap();

        assert!(manifest.image.is_none());
        assert!(manifest.entrypoint.is_none());
        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.packages.len(), 1);
        assert_eq!(manifest.packages[0].name, "bash");
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

[[packages]]
name = "vim"
"#;

        let manifest = Manifest::from_str(config).unwrap();

        let image = manifest.image.unwrap();
        assert!(image.name.is_none());
        assert_eq!(image.architecture, Architecture::Arm64);
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

[[packages]]
name = "nodejs"
"#;

        let manifest = Manifest::from_str(config).unwrap();
        let image = manifest.image.unwrap();
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

        assert_eq!(manifest.packages.len(), 2);
        assert_eq!(manifest.packages[0].name, "nginx");
        assert_eq!(
            manifest.packages[1]
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
      "components": ["main"]
    }
  },
  "packages": [
    { "name": "bash" }
  ]
}"#;

        let manifest = Manifest::from_json(config).unwrap();

        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.packages.len(), 1);
        assert_eq!(manifest.packages[0].name, "bash");
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

[[packages]]
name = "bash"
"#,
        )
        .unwrap();

        let manifest = Manifest::from_dir(dir.path()).unwrap();
        assert_eq!(manifest.packages[0].name, "bash");
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
packages:
  - name: vim
"#,
        )
        .unwrap();

        let manifest = Manifest::from_dir(dir.path()).unwrap();
        assert_eq!(manifest.packages[0].name, "vim");
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
        assert_eq!(manifest.image.unwrap().architecture, Architecture::Amd64);
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

[repositories.alpha]
url = "https://deb.debian.org/debian-security"
distribution = "bookworm-security"
components = ["main"]

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
                 distribution = \"bookworm\"\ncomponents = [\"main\"]\n\n\
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
