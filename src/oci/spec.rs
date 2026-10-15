//! OCI image specification types (v1).
//!
//! These types represent the JSON structures used in OCI container images,
//! implementing OCI Image Spec v1.x with Docker Schema v2 compatibility.
//!
//! See: <https://github.com/opencontainers/image-spec>

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer};

use crate::arch::Architecture;

use super::digest::Digest;
use super::types::{AbsolutePath, EnvVar, Signal, Timestamp};

/// OCI Image Spec v1 media types.
///
/// These identify the type of content in OCI image components.
/// Ref.: https://github.com/opencontainers/image-spec/blob/main/media-types.md
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// application/vnd.oci.image.manifest.v1+json
    ImageManifest,
    /// application/vnd.oci.image.index.v1+json
    ImageIndex,
    /// application/vnd.oci.image.config.v1+json
    ImageConfig,
    /// application/vnd.oci.image.layer.v1.tar
    LayerTar,
    /// application/vnd.oci.image.layer.v1.tar+gzip
    LayerTarGzip,
    /// application/vnd.oci.image.layer.v1.tar+zstd
    LayerTarZstd,
}

impl MediaType {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ImageManifest => "application/vnd.oci.image.manifest.v1+json",
            Self::ImageIndex => "application/vnd.oci.image.index.v1+json",
            Self::ImageConfig => "application/vnd.oci.image.config.v1+json",
            Self::LayerTar => "application/vnd.oci.image.layer.v1.tar",
            Self::LayerTarGzip => "application/vnd.oci.image.layer.v1.tar+gzip",
            Self::LayerTarZstd => "application/vnd.oci.image.layer.v1.tar+zstd",
        }
    }
}

impl Serialize for MediaType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "application/vnd.oci.image.manifest.v1+json" => Ok(Self::ImageManifest),
            "application/vnd.oci.image.index.v1+json" => Ok(Self::ImageIndex),
            "application/vnd.oci.image.config.v1+json" => Ok(Self::ImageConfig),
            "application/vnd.oci.image.layer.v1.tar" => Ok(Self::LayerTar),
            "application/vnd.oci.image.layer.v1.tar+gzip" => Ok(Self::LayerTarGzip),
            "application/vnd.oci.image.layer.v1.tar+zstd" => Ok(Self::LayerTarZstd),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &[
                    "application/vnd.oci.image.manifest.v1+json",
                    "application/vnd.oci.image.index.v1+json",
                    "application/vnd.oci.image.config.v1+json",
                    "application/vnd.oci.image.layer.v1.tar",
                    "application/vnd.oci.image.layer.v1.tar+gzip",
                    "application/vnd.oci.image.layer.v1.tar+zstd",
                ],
            )),
        }
    }
}

/// Schema version for OCI manifests (always 2 for Docker compatibility).
/// Ref.: https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions
const SCHEMA_VERSION: i32 = 2;

/// OCI layout version (always "1.0.0" for OCI Image Spec v1).
/// Ref.: https://github.com/opencontainers/image-spec/blob/main/image-layout.md#oci-layout-file
const OCI_LAYOUT_VERSION: &str = "1.0.0";

/// RootFs type (always "layers" in OCI Image Spec v1).
const ROOTFS_TYPE: &str = "layers";

/// Serialize a `u64` size as an OCI `i64`.
///
/// # Panics
///
/// Panics if the size exceeds `i64::MAX` (~8 exabytes).
fn size_to_oci(size: u64) -> i64 {
    i64::try_from(size).expect("size exceeds i64::MAX")
}

/// Deserialize an OCI `i64` size, rejecting negative values.
fn deserialize_oci_size<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = i64::deserialize(deserializer)?;
    if v < 0 {
        return Err(serde::de::Error::custom(format!(
            "descriptor size must be non-negative, got {}",
            v
        )));
    }
    Ok(v)
}

/// Content descriptor pointing to a blob.
///
/// Descriptors are used throughout OCI to reference content by digest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Descriptor {
    /// Media type of the referenced content.
    pub media_type: MediaType,
    /// Digest of the content (e.g., "sha256:abc123...").
    pub digest: Digest,
    /// Size of the content in bytes (non-negative).
    pub size: i64,
    /// Optional annotations.
    pub annotations: Option<BTreeMap<String, String>>,
}

impl Descriptor {
    /// Create a new descriptor.
    pub fn new(media_type: MediaType, digest: Digest, size: u64) -> Self {
        Self {
            media_type,
            digest,
            size: size_to_oci(size),
            annotations: None,
        }
    }

    /// Add an annotation.
    pub fn with_annotation(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.annotations
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }
}

impl Serialize for Descriptor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let len = 3 + self.annotations.as_ref().map_or(0, |_| 1);
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("mediaType", &self.media_type)?;
        map.serialize_entry("digest", &self.digest)?;
        map.serialize_entry("size", &self.size)?;
        if let Some(ref ann) = self.annotations {
            map.serialize_entry("annotations", ann)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Descriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct DescriptorHelper {
            media_type: MediaType,
            digest: Digest,
            #[serde(deserialize_with = "deserialize_oci_size")]
            size: i64,
            annotations: Option<BTreeMap<String, String>>,
        }

        let h = DescriptorHelper::deserialize(deserializer)?;
        Ok(Descriptor {
            media_type: h.media_type,
            digest: h.digest,
            size: h.size,
            annotations: h.annotations,
        })
    }
}

/// OCI image manifest.
///
/// The manifest describes the image's config and layers.
/// Schema version is always 2 (Docker compatibility) and media type
/// is always `application/vnd.oci.image.manifest.v1+json`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageManifest {
    /// Reference to the image configuration.
    pub config: Descriptor,
    /// List of layer descriptors.
    pub layers: Vec<Descriptor>,
    /// Optional annotations.
    pub annotations: Option<BTreeMap<String, String>>,
}

impl ImageManifest {
    pub fn new(config: Descriptor, layers: Vec<Descriptor>) -> Self {
        Self {
            config,
            layers,
            annotations: None,
        }
    }
}

impl Serialize for ImageManifest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let len = 4 + self.annotations.as_ref().map_or(0, |_| 1);
        let mut map = serializer.serialize_map(Some(len))?;

        map.serialize_entry("schemaVersion", &SCHEMA_VERSION)?;
        map.serialize_entry("mediaType", MediaType::ImageManifest.as_str())?;
        map.serialize_entry("config", &self.config)?;
        map.serialize_entry("layers", &self.layers)?;

        if let Some(ref annotations) = self.annotations {
            map.serialize_entry("annotations", annotations)?;
        }

        map.end()
    }
}

impl<'de> Deserialize<'de> for ImageManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ImageManifestHelper {
            #[allow(dead_code)]
            schema_version: i32,
            #[allow(dead_code)]
            media_type: Option<String>,
            config: Descriptor,
            layers: Vec<Descriptor>,
            annotations: Option<BTreeMap<String, String>>,
        }

        let helper = ImageManifestHelper::deserialize(deserializer)?;
        Ok(ImageManifest {
            config: helper.config,
            layers: helper.layers,
            annotations: helper.annotations,
        })
    }
}

/// OCI image index (manifest list).
///
/// Used for multi-platform images. Schema version is always 2 (Docker
/// compatibility) and media type is always `application/vnd.oci.image.index.v1+json`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageIndex {
    /// List of manifest descriptors.
    pub manifests: Vec<Descriptor>,
    /// Optional annotations.
    pub annotations: Option<BTreeMap<String, String>>,
}

impl ImageIndex {
    pub fn new(manifests: Vec<Descriptor>) -> Self {
        Self {
            manifests,
            annotations: None,
        }
    }
}

impl Serialize for ImageIndex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let len = 3 + self.annotations.as_ref().map_or(0, |_| 1);
        let mut map = serializer.serialize_map(Some(len))?;

        map.serialize_entry("schemaVersion", &SCHEMA_VERSION)?;
        map.serialize_entry("mediaType", MediaType::ImageIndex.as_str())?;
        map.serialize_entry("manifests", &self.manifests)?;

        if let Some(ref annotations) = self.annotations {
            map.serialize_entry("annotations", annotations)?;
        }

        map.end()
    }
}

impl<'de> Deserialize<'de> for ImageIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ImageIndexHelper {
            #[allow(dead_code)]
            schema_version: i32,
            #[allow(dead_code)]
            media_type: Option<String>,
            manifests: Vec<Descriptor>,
            annotations: Option<BTreeMap<String, String>>,
        }

        let helper = ImageIndexHelper::deserialize(deserializer)?;
        Ok(ImageIndex {
            manifests: helper.manifests,
            annotations: helper.annotations,
        })
    }
}

/// Container runtime configuration.
///
/// All fields use domain types that enforce OCI spec invariants at
/// construction time:
///
/// - [`EnvVar`] — each entry is a validated `KEY=VALUE` pair.
/// - [`AbsolutePath`] — `WorkingDir` must be an absolute path.
/// - [`Signal`] — `StopSignal` must be a known POSIX signal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub struct ContainerConfig {
    /// User to run as (e.g., "1000:1000" or "nobody").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// Environment variables, each in `KEY=VALUE` format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVar>>,

    /// Entrypoint command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,

    /// Default command arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,

    /// Working directory (must be an absolute path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<AbsolutePath>,

    /// Labels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,

    /// Stop signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<Signal>,
}

/// Rootfs information for image config.
///
/// The type is always "layers" in OCI Image Spec v1.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RootFs {
    /// DiffIDs of the layers (uncompressed digests).
    pub diff_ids: Vec<Digest>,
}

impl RootFs {
    /// Create a new rootfs with the given diff_ids.
    pub fn new(diff_ids: Vec<Digest>) -> Self {
        Self { diff_ids }
    }
}

impl Serialize for RootFs {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", ROOTFS_TYPE)?;
        map.serialize_entry("diff_ids", &self.diff_ids)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for RootFs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RootFsHelper {
            #[serde(rename = "type")]
            _fs_type: String,
            diff_ids: Vec<Digest>,
        }

        let helper = RootFsHelper::deserialize(deserializer)?;
        Ok(RootFs {
            diff_ids: helper.diff_ids,
        })
    }
}

/// History entry for an image layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct History {
    /// Creation timestamp (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<Timestamp>,
    /// Command that created this layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Author of the layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Comment about this layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// If true, this is an empty layer (metadata only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_layer: Option<bool>,
}

/// Target operating system (always "linux" for this tool).
const OS: &str = "linux";

/// OCI image configuration.
///
/// This describes the image's runtime configuration and layer information.
/// The operating system is always "linux".
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageConfig {
    /// Creation timestamp (RFC 3339).
    pub created: Option<Timestamp>,
    /// Author of the image.
    pub author: Option<String>,
    /// Target CPU architecture.
    pub architecture: Architecture,
    /// Container runtime configuration.
    pub config: Option<ContainerConfig>,
    /// Rootfs information.
    pub rootfs: RootFs,
    /// Layer history.
    pub history: Option<Vec<History>>,
}

impl ImageConfig {
    /// Create a new image configuration.
    pub fn new(architecture: Architecture, diff_ids: Vec<Digest>) -> Self {
        Self {
            created: None,
            author: None,
            architecture,
            config: None,
            rootfs: RootFs::new(diff_ids),
            history: None,
        }
    }
}

impl Serialize for ImageConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let platform = self.architecture.oci_platform().ok_or_else(|| {
            serde::ser::Error::custom(format!(
                "architecture '{}' has no OCI equivalent",
                self.architecture
            ))
        })?;

        let mut len = 3; // architecture, os, rootfs
        if platform.variant.is_some() {
            len += 1;
        }
        if self.created.is_some() {
            len += 1;
        }
        if self.author.is_some() {
            len += 1;
        }
        if self.config.is_some() {
            len += 1;
        }
        if self.history.is_some() {
            len += 1;
        }

        let mut map = serializer.serialize_map(Some(len))?;

        if let Some(ref created) = self.created {
            map.serialize_entry("created", created)?;
        }
        if let Some(ref author) = self.author {
            map.serialize_entry("author", author)?;
        }

        map.serialize_entry("architecture", platform.architecture)?;
        if let Some(variant) = platform.variant {
            map.serialize_entry("variant", variant)?;
        }
        map.serialize_entry("os", OS)?;

        if let Some(ref config) = self.config {
            map.serialize_entry("config", config)?;
        }

        map.serialize_entry("rootfs", &self.rootfs)?;

        if let Some(ref history) = self.history {
            map.serialize_entry("history", history)?;
        }

        map.end()
    }
}

impl<'de> Deserialize<'de> for ImageConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ImageConfigHelper {
            created: Option<Timestamp>,
            author: Option<String>,
            architecture: String,
            variant: Option<String>,
            #[allow(dead_code)]
            os: String,
            config: Option<ContainerConfig>,
            rootfs: RootFs,
            history: Option<Vec<History>>,
        }

        let helper = ImageConfigHelper::deserialize(deserializer)?;

        let architecture = Architecture::from_oci(&helper.architecture, helper.variant.as_deref())
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown OCI architecture: {:?} (variant {:?})",
                    helper.architecture, helper.variant
                ))
            })?;

        Ok(ImageConfig {
            created: helper.created,
            author: helper.author,
            architecture,
            config: helper.config,
            rootfs: helper.rootfs,
            history: helper.history,
        })
    }
}

/// OCI layout file content.
///
/// The layout version is always "1.0.0" in OCI Image Spec v1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OciLayout;

impl Serialize for OciLayout {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("imageLayoutVersion", OCI_LAYOUT_VERSION)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for OciLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OciLayoutHelper {
            #[allow(dead_code)]
            image_layout_version: String,
        }

        let _helper = OciLayoutHelper::deserialize(deserializer)?;
        Ok(OciLayout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_digest() -> Digest {
        Digest::from_bytes(b"test data")
    }

    #[test]
    fn test_descriptor_serialization() {
        let desc = Descriptor::new(MediaType::ImageConfig, valid_digest(), 1234);
        let json = serde_json::to_string(&desc).unwrap();
        assert!(json.contains("mediaType"));
        assert!(json.contains("digest"));
        assert!(json.contains("size"));
        assert!(!json.contains("annotations"));
    }

    #[test]
    fn test_descriptor_with_annotation() {
        let desc = Descriptor::new(MediaType::ImageManifest, valid_digest(), 100)
            .with_annotation("org.opencontainers.image.ref.name", "latest");
        let json = serde_json::to_string_pretty(&desc).unwrap();
        assert!(json.contains("annotations"));
        assert!(json.contains("org.opencontainers.image.ref.name"));
    }

    #[test]
    fn test_descriptor_rejects_negative_size_on_deserialize() {
        let json = r#"{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9","size":-1}"#;
        let result: Result<Descriptor, _> = serde_json::from_str(json);
        assert!(result.is_err(), "negative size should be rejected");
    }

    #[test]
    fn test_image_manifest_serialization() {
        let config = Descriptor::new(MediaType::ImageConfig, valid_digest(), 500);
        let layer = Descriptor::new(MediaType::LayerTarGzip, valid_digest(), 10000);
        let manifest = ImageManifest::new(config, vec![layer]);

        let json = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(json.contains("\"schemaVersion\": 2"));
        assert!(json.contains(MediaType::ImageManifest.as_str()));
    }

    #[test]
    fn test_image_config_serialization() {
        let env = vec![EnvVar::try_from("PATH=/usr/bin:/bin").unwrap()];
        let workdir = AbsolutePath::try_from("/app").unwrap();

        let mut config = ImageConfig::new(Architecture::Amd64, vec![valid_digest()]);
        config.config = Some(ContainerConfig {
            entrypoint: Some(vec!["/bin/sh".to_string()]),
            env: Some(env),
            working_dir: Some(workdir),
            ..Default::default()
        });

        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("\"architecture\": \"amd64\""));
        assert!(json.contains("\"os\": \"linux\""));
        assert!(json.contains("Entrypoint"));
        assert!(json.contains("/bin/sh"));
        assert!(json.contains("PATH=/usr/bin:/bin"));
        assert!(json.contains("/app"));
    }

    #[test]
    fn test_oci_layout_serialization() {
        let layout = OciLayout;
        let json = serde_json::to_string(&layout).unwrap();
        assert_eq!(json, r#"{"imageLayoutVersion":"1.0.0"}"#);
    }

    #[test]
    fn test_rootfs_serialization() {
        let rootfs = RootFs::new(vec![valid_digest(), valid_digest()]);
        let json = serde_json::to_string(&rootfs).unwrap();
        assert!(json.contains(r#""type":"layers""#));
        assert!(json.contains("diff_ids"));
    }

    #[test]
    fn test_history_default() {
        let history = History::default();
        assert!(history.created.is_none());
        assert!(history.created_by.is_none());
        assert!(history.author.is_none());
        assert!(history.comment.is_none());
        assert!(history.empty_layer.is_none());
    }

    #[test]
    fn test_history_with_timestamp() {
        let ts: Timestamp = "2015-10-31T22:22:56Z".parse().unwrap();
        let h = History {
            created: Some(ts),
            created_by: Some("paranoid".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("2015-10-31"));
        assert!(json.contains("paranoid"));
    }

    #[test]
    fn test_container_config_default() {
        let config = ContainerConfig::default();
        assert!(config.user.is_none());
        assert!(config.env.is_none());
        assert!(config.entrypoint.is_none());
    }

    #[test]
    fn test_container_config_env_serde_roundtrip() {
        let env = vec![
            EnvVar::try_from("PATH=/usr/bin").unwrap(),
            EnvVar::try_from("HOME=/root").unwrap(),
        ];
        let config = ContainerConfig {
            env: Some(env.clone()),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let config2: ContainerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config2.env, Some(env));
    }

    #[test]
    fn test_container_config_rejects_bad_env_on_deserialize() {
        // An env var without '=' should be rejected during deserialization.
        let json = r#"{"Env":["NOTANENVVAR"]}"#;
        let result: Result<ContainerConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "env var without '=' should be rejected");
    }

    #[test]
    fn test_container_config_rejects_relative_workdir_on_deserialize() {
        let json = r#"{"WorkingDir":"relative/path"}"#;
        let result: Result<ContainerConfig, _> = serde_json::from_str(json);
        assert!(result.is_err(), "relative workdir should be rejected");
    }

    #[test]
    fn test_container_config_stop_signal_serde() {
        let config = ContainerConfig {
            stop_signal: Some(Signal::Sigterm),
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("SIGTERM"));
        let config2: ContainerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config2.stop_signal, Some(Signal::Sigterm));
    }

    #[test]
    fn test_media_type_serialization() {
        let json = serde_json::to_string(&MediaType::ImageManifest).unwrap();
        assert_eq!(json, r#""application/vnd.oci.image.manifest.v1+json""#);
    }

    #[test]
    fn test_media_type_deserialization() {
        let media_type: MediaType =
            serde_json::from_str(r#""application/vnd.oci.image.layer.v1.tar+gzip""#).unwrap();
        assert_eq!(media_type, MediaType::LayerTarGzip);
    }
}
