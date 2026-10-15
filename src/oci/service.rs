//! OCI image generation service.
//!
//! Provides a high-level API for generating OCI container images.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::arch::Architecture;
use crate::config::Manifest;

use super::blob::BlobStore;
use super::digest::Digest;
use super::error::{OciError, OciImageResult};
use super::layer::{Compression, Layer};
use super::progress::OciProgress;
use super::spec::{
    ContainerConfig, Descriptor, History, ImageConfig, ImageIndex, ImageManifest, MediaType,
    OciLayout,
};
use super::types::{
    AbsolutePath, AbsolutePathError, EnvVar, EnvVarError, ImageName, ImageNameError, ImageTag,
    ImageTagError, Timestamp,
};

/// Errors that can occur when building an [`OciConfig`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum OciConfigError {
    #[error("invalid environment variable: {0}")]
    EnvVar(#[from] EnvVarError),
    #[error("invalid working directory: {0}")]
    WorkDir(#[from] AbsolutePathError),
    #[error("invalid image tag: {0}")]
    ImageTag(#[from] ImageTagError),
    #[error("invalid image name: {0}")]
    ImageName(#[from] ImageNameError),
}

/// Configuration for OCI image generation.
///
/// Use the builder methods for ergonomic construction. Methods that accept
/// domain types (e.g. [`EnvVar`], [`AbsolutePath`]) validate their input and
/// return `Result`. Methods that accept unambiguously safe values (e.g.
/// [`Architecture`], [`Compression`]) are infallible.
///
/// # Example
///
/// ```
/// use paranoid::oci::{OciConfig, EnvVar};
/// use paranoid::arch::Architecture;
/// use std::convert::TryFrom;
///
/// let config = OciConfig::new()
///     .architecture(Architecture::Amd64)
///     .entrypoint(["/bin/sh"])
///     .env([EnvVar::try_from("PATH=/usr/bin:/bin").unwrap()]);
/// ```
#[derive(Debug, Clone)]
pub struct OciConfig {
    /// Target CPU architecture.
    architecture: Architecture,
    /// Layer compression format.
    compression: Compression,
    /// Image creation timestamp (RFC 3339).
    created: Option<Timestamp>,
    /// Entrypoint command.
    entrypoint: Option<Vec<String>>,
    /// Default command arguments.
    cmd: Option<Vec<String>>,
    /// Environment variables (validated KEY=VALUE pairs).
    env: Option<Vec<EnvVar>>,
    /// Working directory (validated absolute path).
    workdir: Option<AbsolutePath>,
    /// Image labels.
    labels: Option<Vec<(String, String)>>,
    /// User to run as.
    user: Option<String>,
    /// Image name for tagging.
    image_name: Option<ImageName>,
    /// Image tag.
    image_tag: ImageTag,
}

impl OciConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target architecture.
    pub fn architecture(mut self, arch: Architecture) -> Self {
        self.architecture = arch;
        self
    }

    /// Set the layer compression format.
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// Set the image creation timestamp.
    pub fn created(mut self, created: Timestamp) -> Self {
        self.created = Some(created);
        self
    }

    /// Set the entrypoint command.
    ///
    /// Accepts any iterator of string-like items:
    /// ```
    /// # use paranoid::oci::OciConfig;
    /// OciConfig::new().entrypoint(["/bin/sh", "-c"]);
    /// OciConfig::new().entrypoint(vec!["/bin/sh".to_string()]);
    /// ```
    pub fn entrypoint(mut self, entrypoint: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.entrypoint = Some(entrypoint.into_iter().map(Into::into).collect());
        self
    }

    /// Set the default command arguments.
    pub fn cmd(mut self, cmd: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.cmd = Some(cmd.into_iter().map(Into::into).collect());
        self
    }

    /// Set environment variables from an iterator of validated [`EnvVar`]s.
    ///
    /// # Example
    ///
    /// ```
    /// # use paranoid::oci::{OciConfig, EnvVar};
    /// # use std::convert::TryFrom;
    /// let config = OciConfig::new()
    ///     .env([EnvVar::try_from("PATH=/usr/bin:/bin").unwrap()]);
    /// ```
    pub fn env(mut self, env: impl IntoIterator<Item = EnvVar>) -> Self {
        self.env = Some(env.into_iter().collect());
        self
    }

    /// Set environment variables from raw `KEY=VALUE` strings, validating each one.
    ///
    /// Returns an error if any string is not a valid env var.
    ///
    /// # Example
    ///
    /// ```
    /// # use paranoid::oci::OciConfig;
    /// let config = OciConfig::new()
    ///     .try_env(["PATH=/usr/bin:/bin", "HOME=/root"])
    ///     .unwrap();
    /// ```
    pub fn try_env<S>(mut self, env: impl IntoIterator<Item = S>) -> Result<Self, OciConfigError>
    where
        S: TryInto<EnvVar, Error = EnvVarError>,
    {
        let vars: Result<Vec<EnvVar>, _> = env.into_iter().map(TryInto::try_into).collect();
        self.env = Some(vars?);
        Ok(self)
    }

    /// Set the working directory (must be an absolute path).
    ///
    /// # Errors
    ///
    /// Returns [`OciConfigError::WorkDir`] if the path is not absolute.
    pub fn workdir(mut self, workdir: AbsolutePath) -> Self {
        self.workdir = Some(workdir);
        self
    }

    /// Set the working directory from a string, validating that it is absolute.
    ///
    /// # Errors
    ///
    /// Returns [`OciConfigError::WorkDir`] if the path is not absolute.
    pub fn try_workdir(
        mut self,
        workdir: impl TryInto<AbsolutePath, Error = AbsolutePathError>,
    ) -> Result<Self, OciConfigError> {
        self.workdir = Some(workdir.try_into()?);
        Ok(self)
    }

    /// Add a label.
    pub fn label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels
            .get_or_insert_with(Vec::new)
            .push((key.into(), value.into()));
        self
    }

    /// Set the user to run as.
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set the image name (for tagging in index.json).
    ///
    /// # Errors
    ///
    /// Returns [`OciConfigError::ImageName`] if the name contains `:` or `@`.
    pub fn try_image_name(
        mut self,
        name: impl TryInto<ImageName, Error = ImageNameError>,
    ) -> Result<Self, OciConfigError> {
        self.image_name = Some(name.try_into()?);
        Ok(self)
    }

    /// Set the image name from a validated [`ImageName`].
    pub fn image_name(mut self, name: ImageName) -> Self {
        self.image_name = Some(name);
        self
    }

    /// Set the image tag.
    ///
    /// # Errors
    ///
    /// Returns [`OciConfigError::ImageTag`] if the tag is empty, too long, or
    /// contains invalid characters.
    pub fn try_image_tag(
        mut self,
        tag: impl TryInto<ImageTag, Error = ImageTagError>,
    ) -> Result<Self, OciConfigError> {
        self.image_tag = tag.try_into()?;
        Ok(self)
    }

    /// Set the image tag from a validated [`ImageTag`].
    pub fn image_tag(mut self, tag: ImageTag) -> Self {
        self.image_tag = tag;
        self
    }

    /// Build the image configuration a manifest describes.
    ///
    /// `created` is stamped into both the config and the
    /// `org.opencontainers.image.created` label. Callers pick it (e.g. from
    /// `SOURCE_DATE_EPOCH`) so identical inputs produce identical images.
    pub fn from_manifest(manifest: &Manifest, created: Timestamp) -> Result<Self, OciConfigError> {
        const LABEL_PREFIX: &str = "org.opencontainers.image.";

        let mut config = Self::new()
            .created(created.clone())
            .label(format!("{LABEL_PREFIX}created"), created.to_string());

        if let Some(image) = &manifest.image {
            config = config.architecture(image.architecture);
            if let Some(name) = &image.name {
                config = config.try_image_name(name.as_str())?;
            }
            if let Some(tag) = &image.tag {
                config = config.try_image_tag(tag.as_str())?;
            }

            let labels = [
                ("title", &image.name),
                ("description", &image.description),
                ("authors", &image.authors),
                ("source", &image.source),
            ];
            for (key, value) in labels {
                if let Some(value) = value {
                    config = config.label(format!("{LABEL_PREFIX}{key}"), value);
                }
            }
        }

        if let Some(entrypoint) = &manifest.entrypoint {
            config = config.entrypoint(entrypoint.cmd.iter().cloned());
            if let Some(env) = &entrypoint.env {
                config = config.env(
                    env.iter()
                        .map(|(k, v)| EnvVar::new(k, v))
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            if let Some(workdir) = &entrypoint.workdir {
                config = config.try_workdir(workdir.as_str())?;
            }
        }

        Ok(config)
    }

    /// Get the image name, if set.
    pub fn get_image_name(&self) -> Option<&ImageName> {
        self.image_name.as_ref()
    }

    /// Get the image tag.
    pub fn get_image_tag(&self) -> &ImageTag {
        &self.image_tag
    }
}

impl Default for OciConfig {
    fn default() -> Self {
        Self {
            architecture: Architecture::Amd64,
            compression: Compression::Gzip,
            created: None,
            entrypoint: None,
            cmd: None,
            env: None,
            workdir: None,
            labels: None,
            user: None,
            image_name: None,
            image_tag: ImageTag::default(), // "latest"
        }
    }
}

pub struct OciImageService {
    config: OciConfig,
}

impl OciImageService {
    pub fn new() -> Self {
        Self::with_config(OciConfig::default())
    }

    pub fn with_config(config: OciConfig) -> Self {
        Self { config }
    }

    /// Generate an OCI image directory layout.
    ///
    /// Creates the standard OCI image layout:
    /// ```text
    /// output_dir/
    /// ├── blobs/sha256/
    /// │   ├── {config-hash}
    /// │   ├── {manifest-hash}
    /// │   └── {layer-hash}
    /// ├── oci-layout
    /// └── index.json
    /// ```
    pub fn generate<P: OciProgress>(
        &self,
        source_dir: &Path,
        output_dir: &Path,
        progress: &P,
    ) -> Result<OciImageResult, OciError> {
        progress.on_start();

        fs::create_dir_all(output_dir).map_err(|e| OciError::CreateOutputDir {
            path: output_dir.to_path_buf(),
            source: e,
        })?;

        let store = BlobStore::new(output_dir)?;

        progress.on_layer_start();

        let layer = Layer::from_directory(source_dir, self.config.compression)?;

        progress.on_layer_complete(layer.size());
        progress.on_config_start();

        let image_config = self.build_image_config(layer.diff_id());
        let config_json = serde_json::to_vec_pretty(&image_config)?;
        let (config_digest, config_size) = store.write(&config_json)?;

        progress.on_config_complete();
        progress.on_manifest_start();

        let manifest = self.build_manifest(&config_digest, config_size, &layer);
        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        let (manifest_digest, manifest_size) = store.write(&manifest_json)?;

        progress.on_manifest_complete();
        progress.on_writing_blobs();

        store.write_with_digest(layer.data(), layer.digest())?;

        // Write oci-layout
        let layout = OciLayout;
        let layout_json = serde_json::to_vec(&layout)?;
        let layout_path = output_dir.join("oci-layout");
        write_file(&layout_path, &layout_json)?;

        // Write index.json
        let index = self.build_index(&manifest_digest, manifest_size);
        let index_json = serde_json::to_vec_pretty(&index)?;
        let index_path = output_dir.join("index.json");
        write_file(&index_path, &index_json)?;

        let total_size = layer.size() + config_size + manifest_size;

        progress.on_complete(manifest_digest.as_str(), total_size);

        Ok(OciImageResult {
            path: output_dir.to_path_buf(),
            manifest_digest,
            config_digest,
            layer_digest: layer.digest().clone(),
            total_size,
        })
    }

    /// Generate an OCI image as a tarball.
    ///
    /// Creates a tar archive containing the OCI layout that can be loaded
    /// with `podman load -i` or `docker load -i`.
    pub fn generate_tarball<P: OciProgress>(
        &self,
        source_dir: &Path,
        output_path: &Path,
        progress: &P,
    ) -> Result<OciImageResult, OciError> {
        progress.on_start();

        let temp_dir = tempfile::tempdir().map_err(|source| OciError::CreateTempDir { source })?;
        let oci_dir = temp_dir.path();

        let inner_progress = SkipStartProgress(progress);
        let result = self.generate(source_dir, oci_dir, &inner_progress)?;

        progress.on_creating_tarball();

        let tar_file = File::create(output_path).map_err(|e| OciError::WriteFile {
            path: output_path.to_path_buf(),
            source: e,
        })?;

        let mut tar_builder = tar::Builder::new(tar_file);
        add_dir_to_tar(&mut tar_builder, oci_dir, "")?;
        tar_builder
            .finish()
            .map_err(|e| OciError::LayerCreation(format!("failed to finish tarball: {}", e)))?;

        let total_size = fs::metadata(output_path)
            .map(|m| m.len())
            .unwrap_or(result.total_size);

        progress.on_complete(result.manifest_digest.as_str(), total_size);

        Ok(OciImageResult {
            path: output_path.to_path_buf(),
            manifest_digest: result.manifest_digest,
            config_digest: result.config_digest,
            layer_digest: result.layer_digest,
            total_size,
        })
    }

    fn build_image_config(&self, diff_id: &Digest) -> ImageConfig {
        let mut config = ImageConfig::new(self.config.architecture, vec![diff_id.clone()]);

        config.created.clone_from(&self.config.created);

        if self.config.entrypoint.is_some()
            || self.config.cmd.is_some()
            || self.config.env.is_some()
            || self.config.workdir.is_some()
            || self.config.labels.is_some()
            || self.config.user.is_some()
        {
            let mut container_config = ContainerConfig::default();

            container_config
                .entrypoint
                .clone_from(&self.config.entrypoint);

            container_config.cmd.clone_from(&self.config.cmd);
            container_config.env.clone_from(&self.config.env);
            container_config
                .working_dir
                .clone_from(&self.config.workdir);
            container_config.user.clone_from(&self.config.user);

            if let Some(ref labels) = self.config.labels {
                let label_map = labels.iter().cloned().collect();
                container_config.labels = Some(label_map);
            }

            config.config = Some(container_config);
        }

        let history = History {
            created: self.config.created.clone(),
            created_by: Some("paranoid".to_string()),
            ..Default::default()
        };

        config.history = Some(vec![history]);

        config
    }

    fn build_manifest(
        &self,
        config_digest: &Digest,
        config_size: u64,
        layer: &Layer,
    ) -> ImageManifest {
        let config_desc =
            Descriptor::new(MediaType::ImageConfig, config_digest.clone(), config_size);
        let layer_desc = Descriptor::new(layer.media_type(), layer.digest().clone(), layer.size());
        ImageManifest::new(config_desc, vec![layer_desc])
    }

    fn build_index(&self, manifest_digest: &Digest, manifest_size: u64) -> ImageIndex {
        let mut manifest_desc = Descriptor::new(
            MediaType::ImageManifest,
            manifest_digest.clone(),
            manifest_size,
        );

        let ref_name = match &self.config.image_name {
            Some(name) => format!("{}:{}", name.as_str(), self.config.image_tag.as_str()),
            None => self.config.image_tag.as_str().to_string(),
        };

        manifest_desc =
            manifest_desc.with_annotation("org.opencontainers.image.ref.name", ref_name);

        ImageIndex::new(vec![manifest_desc])
    }

    pub fn config(&self) -> &OciConfig {
        &self.config
    }
}

impl Default for OciImageService {
    fn default() -> Self {
        Self::new()
    }
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), OciError> {
    fs::write(path, data).map_err(|e| OciError::WriteFile {
        path: path.to_path_buf(),
        source: e,
    })
}

fn add_dir_to_tar<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), OciError> {
    for entry in fs::read_dir(dir).map_err(|e| OciError::ReadSourceDir {
        path: dir.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| OciError::ReadSourceDir {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let path = entry.path();
        let name = entry.file_name();
        let tar_path = if prefix.is_empty() {
            name.to_string_lossy().into_owned()
        } else {
            format!("{}/{}", prefix, name.to_string_lossy())
        };

        let metadata = fs::metadata(&path).map_err(|e| OciError::ReadSourceDir {
            path: path.clone(),
            source: e,
        })?;

        if metadata.is_dir() {
            builder
                .append_dir(&tar_path, &path)
                .map_err(|e| OciError::LayerCreation(format!("failed to add directory: {}", e)))?;
            add_dir_to_tar(builder, &path, &tar_path)?;
        } else {
            let mut file = File::open(&path).map_err(|e| OciError::ReadSourceDir {
                path: path.clone(),
                source: e,
            })?;
            builder
                .append_file(&tar_path, &mut file)
                .map_err(|e| OciError::LayerCreation(format!("failed to add file: {}", e)))?;
        }
    }

    Ok(())
}

struct SkipStartProgress<'a, P: OciProgress>(&'a P);

impl<P: OciProgress> OciProgress for SkipStartProgress<'_, P> {
    fn on_start(&self) {}

    fn on_layer_start(&self) {
        self.0.on_layer_start();
    }

    fn on_layer_complete(&self, size: u64) {
        self.0.on_layer_complete(size);
    }

    fn on_config_start(&self) {
        self.0.on_config_start();
    }

    fn on_config_complete(&self) {
        self.0.on_config_complete();
    }

    fn on_manifest_start(&self) {
        self.0.on_manifest_start();
    }

    fn on_manifest_complete(&self) {
        self.0.on_manifest_complete();
    }

    fn on_writing_blobs(&self) {
        self.0.on_writing_blobs();
    }

    fn on_creating_tarball(&self) {
        self.0.on_creating_tarball();
    }

    fn on_complete(&self, _digest: &str, _total_size: u64) {}

    fn on_error(&self, error: &str) {
        self.0.on_error(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    use crate::oci::progress::NoProgress;

    fn create_test_rootfs() -> TempDir {
        let dir = TempDir::new().unwrap();

        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();

        let mut script = File::create(dir.path().join("bin/hello")).unwrap();
        script.write_all(b"#!/bin/sh\necho hello\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(dir.path().join("bin/hello"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(dir.path().join("bin/hello"), perms).unwrap();
        }

        File::create(dir.path().join("etc/hostname"))
            .unwrap()
            .write_all(b"test-container\n")
            .unwrap();

        dir
    }

    #[test]
    fn test_oci_config_builder() {
        let config = OciConfig::new()
            .architecture(Architecture::Arm64)
            .entrypoint(["/bin/sh"])
            .cmd(["-c", "echo hello"])
            .try_env(["PATH=/bin"])
            .unwrap()
            .try_workdir("/app")
            .unwrap()
            .user("1000:1000")
            .label("version", "1.0")
            .try_image_name("test-image")
            .unwrap()
            .try_image_tag("v1")
            .unwrap();

        assert_eq!(
            config.architecture.oci_platform().unwrap().architecture,
            "arm64"
        );
        assert_eq!(config.image_tag.as_str(), "v1");
        assert_eq!(config.entrypoint, Some(vec!["/bin/sh".to_string()]));
        assert_eq!(
            config.cmd,
            Some(vec!["-c".to_string(), "echo hello".to_string()])
        );
        assert_eq!(
            config.env,
            Some(vec![EnvVar::try_from("PATH=/bin").unwrap()])
        );
        assert_eq!(
            config.workdir,
            Some(AbsolutePath::try_from("/app").unwrap())
        );
    }

    #[test]
    fn test_oci_config_builder_with_env_var_type() {
        // Test using EnvVar directly (infallible path)
        let vars = vec![
            EnvVar::try_from("PATH=/bin").unwrap(),
            EnvVar::try_from("HOME=/root").unwrap(),
        ];
        let config = OciConfig::new().env(vars.clone());
        assert_eq!(config.env, Some(vars));
    }

    #[test]
    fn test_oci_config_try_env_rejects_bad_var() {
        let result = OciConfig::new().try_env(["NOTANENVVAR"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_oci_config_try_workdir_rejects_relative() {
        let result = OciConfig::new().try_workdir("relative/path");
        assert!(result.is_err());
    }

    #[test]
    fn test_oci_config_try_image_name_rejects_colon() {
        let result = OciConfig::new().try_image_name("name:tag");
        assert!(result.is_err());
    }

    #[test]
    fn test_oci_config_try_image_tag_rejects_empty() {
        let result = OciConfig::new().try_image_tag("");
        assert!(result.is_err());
    }

    #[test]
    fn test_oci_config_default_tag_is_latest() {
        let config = OciConfig::new();
        assert_eq!(config.image_tag.as_str(), "latest");
    }

    #[test]
    fn test_oci_config_builder_with_vec() {
        let config = OciConfig::new()
            .entrypoint(vec!["/bin/sh".to_string(), "-c".to_string()])
            .env(vec![EnvVar::try_from("PATH=/bin").unwrap()]);

        assert_eq!(
            config.entrypoint,
            Some(vec!["/bin/sh".to_string(), "-c".to_string()])
        );
    }

    #[test]
    fn test_generate_oci_directory() {
        let rootfs = create_test_rootfs();
        let output = TempDir::new().unwrap();

        let config = OciConfig::new()
            .architecture(Architecture::Amd64)
            .try_image_name("test")
            .unwrap();

        let service = OciImageService::with_config(config);
        let result = service
            .generate(rootfs.path(), output.path(), &NoProgress)
            .unwrap();

        assert!(output.path().join("oci-layout").exists());
        assert!(output.path().join("index.json").exists());
        assert!(output.path().join("blobs/sha256").exists());

        assert!(result.manifest_digest.as_str().starts_with("sha256:"));
        assert!(result.config_digest.as_str().starts_with("sha256:"));
        assert!(result.layer_digest.as_str().starts_with("sha256:"));
        assert!(result.total_size > 0);

        let layout_json = fs::read(output.path().join("oci-layout")).unwrap();
        let _layout: OciLayout = serde_json::from_slice(&layout_json).unwrap();
        assert!(String::from_utf8_lossy(&layout_json).contains("1.0.0"));

        let index_json = fs::read(output.path().join("index.json")).unwrap();
        let index: ImageIndex = serde_json::from_slice(&index_json).unwrap();
        assert!(String::from_utf8_lossy(&index_json).contains("\"schemaVersion\": 2"));
        assert_eq!(index.manifests.len(), 1);
    }

    #[test]
    fn test_generate_tarball() {
        let rootfs = create_test_rootfs();
        let output = TempDir::new().unwrap();
        let tarball_path = output.path().join("image.tar");

        let config = OciConfig::new()
            .architecture(Architecture::Amd64)
            .try_image_name("test")
            .unwrap();

        let service = OciImageService::with_config(config);
        let result = service
            .generate_tarball(rootfs.path(), &tarball_path, &NoProgress)
            .unwrap();

        assert!(tarball_path.exists());
        assert!(result.path == tarball_path);
        assert!(result.total_size > 0);

        let tar_file = File::open(&tarball_path).unwrap();
        let mut archive = tar::Archive::new(tar_file);
        let entries: Vec<_> = archive.entries().unwrap().collect();

        let paths: Vec<String> = entries
            .into_iter()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(paths.iter().any(|p| p == "oci-layout"));
        assert!(paths.iter().any(|p| p == "index.json"));
        assert!(paths.iter().any(|p| p.starts_with("blobs")));
    }

    #[test]
    fn test_generate_missing_source_dir() {
        let output = TempDir::new().unwrap();
        let service = OciImageService::new();

        let result = service.generate(
            Path::new("/nonexistent/source/dir"),
            output.path(),
            &NoProgress,
        );

        assert!(matches!(result, Err(OciError::SourceNotFound { .. })));
    }

    #[test]
    fn test_skip_start_progress_suppresses_on_start_and_on_complete() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct CountingProgress {
            start_count: AtomicU32,
            complete_count: AtomicU32,
        }

        impl OciProgress for CountingProgress {
            fn on_start(&self) {
                self.start_count.fetch_add(1, Ordering::Relaxed);
            }
            fn on_complete(&self, _digest: &str, _size: u64) {
                self.complete_count.fetch_add(1, Ordering::Relaxed);
            }
            fn on_layer_start(&self) {}
            fn on_layer_complete(&self, _: u64) {}
            fn on_config_start(&self) {}
            fn on_config_complete(&self) {}
            fn on_manifest_start(&self) {}
            fn on_manifest_complete(&self) {}
            fn on_writing_blobs(&self) {}
            fn on_creating_tarball(&self) {}
            fn on_error(&self, _: &str) {}
        }

        let inner = CountingProgress {
            start_count: AtomicU32::new(0),
            complete_count: AtomicU32::new(0),
        };
        let wrapped = SkipStartProgress(&inner);

        wrapped.on_start();
        wrapped.on_complete("sha256:abc", 0);

        // SkipStartProgress suppresses both on_start and on_complete on the wrapper.
        assert_eq!(inner.start_count.load(Ordering::Relaxed), 0);
        assert_eq!(inner.complete_count.load(Ordering::Relaxed), 0);

        // But direct delegation for other hooks works.
        wrapped.on_layer_start(); // should not panic
    }

    #[test]
    fn test_reproducibility() {
        let rootfs = create_test_rootfs();
        let output1 = TempDir::new().unwrap();
        let output2 = TempDir::new().unwrap();

        let config = OciConfig::new()
            .architecture(Architecture::Amd64)
            .created(Timestamp::epoch());

        let service = OciImageService::with_config(config);

        let result1 = service
            .generate(rootfs.path(), output1.path(), &NoProgress)
            .unwrap();
        let result2 = service
            .generate(rootfs.path(), output2.path(), &NoProgress)
            .unwrap();

        assert_eq!(result1.manifest_digest, result2.manifest_digest);
        assert_eq!(result1.config_digest, result2.config_digest);
        assert_eq!(result1.layer_digest, result2.layer_digest);
    }
}
