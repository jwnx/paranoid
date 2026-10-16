//! OCI image configuration and generation.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::arch::Architecture;
use crate::config::ImageSpec;
use crate::report::{Event, Phase, Reporter};
use crate::rootfs::Rootfs;

use super::digest::Digest;
use super::error::{OciError, OciImageResult};
use super::layer::{Compression, Layer};
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
    pub fn from_image(
        image: &ImageSpec,
        architecture: Architecture,
        created: Timestamp,
    ) -> Result<Self, OciConfigError> {
        const LABEL_PREFIX: &str = "org.opencontainers.image.";

        let mut config = Self::new()
            .created(created.clone())
            .label(format!("{LABEL_PREFIX}created"), created.to_string())
            .architecture(architecture);

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

        if let Some(entrypoint) = &image.entrypoint {
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

/// Where to write an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageOutput {
    /// An OCI image layout directory.
    Layout(PathBuf),
    /// A tarball of the layout, loadable with `podman load` / `docker load`.
    Tarball(PathBuf),
}

impl ImageOutput {
    /// A path ending in `.tar` means a tarball; anything else a layout.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if path.extension().is_some_and(|e| e == "tar") {
            Self::Tarball(path)
        } else {
            Self::Layout(path)
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Layout(path) | Self::Tarball(path) => path,
        }
    }
}

/// Steps reported for [`Phase::Image`].
const IMAGE_STEPS: u64 = 3;

/// Build a single-layer image from `rootfs` and write it to `output`.
///
/// The image is assembled as a sorted list of files (blobs, `oci-layout`,
/// `index.json`) and then written out, so a layout directory and a tarball
/// always hold byte-identical content. The layer is streamed to a temporary
/// file next to the output; everything else is small and kept in memory.
pub fn write_image(
    config: &OciConfig,
    rootfs: &Rootfs,
    output: &ImageOutput,
    reporter: &dyn Reporter,
) -> Result<OciImageResult, OciError> {
    let step = |item| {
        reporter.report(Event::Advance {
            phase: Phase::Image,
            units: 1,
            item,
        })
    };
    reporter.report(Event::Begin {
        phase: Phase::Image,
        total: IMAGE_STEPS,
    });

    // Everything is staged next to the output and moved into place at the
    // end, so a failed build leaves the previous image as it was.
    let parent = output.parent_dir()?;
    let staging = match output {
        ImageOutput::Layout(_) => Some(
            tempfile::Builder::new()
                .prefix(".paranoid-")
                .tempdir_in(&parent)
                .map_err(|source| OciError::CreateOutputDir {
                    path: parent.clone(),
                    source,
                })?,
        ),
        ImageOutput::Tarball(_) => None,
    };
    #[cfg(unix)]
    if let Some(staging) = &staging {
        // tempfile makes it private; the layout is an ordinary directory.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o755)).map_err(
            |source| OciError::CreateOutputDir {
                path: staging.path().to_path_buf(),
                source,
            },
        )?;
    }
    let scratch = staging.as_ref().map_or(parent.as_path(), |s| s.path());
    let layer = Layer::from_rootfs(rootfs, config.compression, scratch)?;
    step("layer");

    let image_config = serde_json::to_vec_pretty(&config.image_config(layer.diff_id()))?;
    let config_digest = Digest::from_bytes(&image_config);
    let manifest = serde_json::to_vec_pretty(&ImageManifest::new(
        Descriptor::new(
            MediaType::ImageConfig,
            config_digest,
            image_config.len() as u64,
        ),
        vec![Descriptor::new(
            layer.media_type(),
            *layer.digest(),
            layer.size(),
        )],
    ))?;
    let manifest_digest = Digest::from_bytes(&manifest);
    let index = serde_json::to_vec_pretty(&config.image_index(&manifest_digest, manifest.len()))?;
    step("manifest");

    let total_size = layer.size() + image_config.len() as u64 + manifest.len() as u64;
    let layer_digest = *layer.digest();
    let blob = |digest: &Digest| format!("blobs/sha256/{}", digest.hash());
    let mut files: Vec<(String, Content)> = vec![
        (blob(&layer_digest), Content::Layer(layer)),
        (blob(&config_digest), Content::Bytes(image_config)),
        (blob(&manifest_digest), Content::Bytes(manifest)),
        (
            "oci-layout".to_string(),
            Content::Bytes(serde_json::to_vec(&OciLayout)?),
        ),
        ("index.json".to_string(), Content::Bytes(index)),
    ];
    files.sort_by(|a, b| a.0.cmp(&b.0));

    match output {
        ImageOutput::Layout(dir) => {
            let staging = staging.expect("layouts are staged");
            write_layout(staging.path(), files)?;
            replace_layout(staging, dir)?;
        }
        ImageOutput::Tarball(path) => write_tarball(path, &files)?,
    }
    step("write");

    reporter.report(Event::End {
        phase: Phase::Image,
        summary: total_size,
    });

    Ok(OciImageResult {
        path: output.path().to_path_buf(),
        reference: config.reference(),
        manifest_digest,
        config_digest,
        layer_digest,
        total_size,
    })
}

/// The contents of one file of an image.
enum Content {
    Bytes(Vec<u8>),
    Layer(Layer),
}

impl ImageOutput {
    /// The directory the output goes in, created if need be. We stage
    /// there, so moving the result into place is a rename.
    fn parent_dir(&self) -> Result<PathBuf, OciError> {
        let dir = match self.path().parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        fs::create_dir_all(dir).map_err(|source| OciError::CreateOutputDir {
            path: dir.to_path_buf(),
            source,
        })?;
        Ok(dir.to_path_buf())
    }
}

impl OciConfig {
    /// The image config document for a single layer.
    fn image_config(&self, diff_id: &Digest) -> ImageConfig {
        let mut image = ImageConfig::new(self.architecture, vec![*diff_id]);
        image.created.clone_from(&self.created);

        let container = ContainerConfig {
            entrypoint: self.entrypoint.clone(),
            cmd: self.cmd.clone(),
            env: self.env.clone(),
            working_dir: self.workdir.clone(),
            user: self.user.clone(),
            labels: self
                .labels
                .as_ref()
                .map(|labels| labels.iter().cloned().collect()),
            ..ContainerConfig::default()
        };
        if container != ContainerConfig::default() {
            image.config = Some(container);
        }

        image.history = Some(vec![History {
            created: self.created.clone(),
            created_by: Some("paranoid".to_string()),
            ..History::default()
        }]);
        image
    }

    /// The index pointing at the manifest, annotated with the image reference.
    fn image_index(&self, manifest_digest: &Digest, manifest_size: usize) -> ImageIndex {
        let manifest = Descriptor::new(
            MediaType::ImageManifest,
            *manifest_digest,
            manifest_size as u64,
        )
        .with_annotation("org.opencontainers.image.ref.name", self.reference());
        ImageIndex::new(vec![manifest])
    }

    /// `name:tag`, or just the tag when the image has no name.
    pub fn reference(&self) -> String {
        match &self.image_name {
            Some(name) => format!("{}:{}", name.as_str(), self.image_tag.as_str()),
            None => self.image_tag.as_str().to_string(),
        }
    }
}

fn write_layout(dir: &Path, files: Vec<(String, Content)>) -> Result<(), OciError> {
    for (name, content) in files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| OciError::CreateOutputDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        match content {
            Content::Bytes(data) => fs::write(&path, data),
            Content::Layer(layer) => layer.persist(&path),
        }
        .map_err(|source| OciError::WriteFile { path, source })?;
    }
    Ok(())
}

/// Swap a staged layout in for `dest`. Whatever was at `dest` goes,
/// stale blobs included, but only if it's an OCI layout or an empty
/// directory: a mistyped `-o` mustn't delete someone's work.
fn replace_layout(staged: tempfile::TempDir, dest: &Path) -> Result<(), OciError> {
    let write_err = |source| OciError::WriteFile {
        path: dest.to_path_buf(),
        source,
    };
    let old = match fs::symlink_metadata(dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(write_err(e)),
        Ok(meta) => {
            let replaceable = meta.is_dir()
                && (dest.join("oci-layout").is_file()
                    || fs::read_dir(dest).map_err(write_err)?.next().is_none());
            if !replaceable {
                return Err(OciError::NotALayout(dest.to_path_buf()));
            }
            // Move the old layout aside, into a directory that removes it
            // when dropped.
            let parent = staged.path().parent().unwrap_or(Path::new("."));
            let trash = tempfile::Builder::new()
                .prefix(".paranoid-old-")
                .tempdir_in(parent)
                .map_err(write_err)?;
            let aside = trash.path().join("layout");
            fs::rename(dest, &aside).map_err(write_err)?;
            Some((trash, aside))
        }
    };
    if let Err(e) = fs::rename(staged.path(), dest) {
        if let Some((_, aside)) = &old {
            let _ = fs::rename(aside, dest);
        }
        return Err(write_err(e));
    }
    // The staged directory is now `dest`; don't let its guard remove it.
    let _ = staged.keep();
    drop(old);
    Ok(())
}

/// Write the layout as a tarball with fixed metadata, through a temp file
/// in the same directory so a failed write never leaves a partial image.
fn write_tarball(path: &Path, files: &[(String, Content)]) -> Result<(), OciError> {
    let write_err = |source| OciError::WriteFile {
        path: path.to_path_buf(),
        source,
    };
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };

    let temp = tempfile::NamedTempFile::new_in(dir).map_err(write_err)?;
    let mut builder = tar::Builder::new(BufWriter::new(temp));

    let header = |kind, mode, size| {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(kind);
        h.set_mode(mode);
        h.set_size(size);
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        h
    };
    for dir in ["blobs/", "blobs/sha256/"] {
        builder
            .append_data(
                &mut header(tar::EntryType::Directory, 0o755, 0),
                dir,
                std::io::empty(),
            )
            .map_err(write_err)?;
    }
    for (name, content) in files {
        match content {
            Content::Bytes(data) => {
                let mut h = header(tar::EntryType::Regular, 0o644, data.len() as u64);
                builder.append_data(&mut h, name, data.as_slice())
            }
            Content::Layer(layer) => {
                let mut h = header(tar::EntryType::Regular, 0o644, layer.size());
                layer
                    .reader()
                    .and_then(|r| builder.append_data(&mut h, name, r))
            }
        }
        .map_err(write_err)?;
    }

    let temp = builder
        .into_inner()
        .and_then(|w| w.into_inner().map_err(|e| e.into_error()))
        .map_err(write_err)?;
    temp.persist(path).map_err(|e| write_err(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::report::test_utils::Recorder;

    fn test_rootfs() -> Rootfs {
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0o755);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(21);
        builder
            .append_data(&mut h, "./bin/hello", &b"#!/bin/sh\necho hello\n"[..])
            .unwrap();
        let mut fs = Rootfs::new();
        fs.apply_tar(builder.into_inner().unwrap().as_slice())
            .unwrap();
        fs
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
    fn test_image_output_from_path() {
        assert!(matches!(
            ImageOutput::from_path("image.tar"),
            ImageOutput::Tarball(_)
        ));
        assert!(matches!(
            ImageOutput::from_path("out/image"),
            ImageOutput::Layout(_)
        ));
    }

    #[test]
    fn test_write_layout() {
        let output = TempDir::new().unwrap();
        let config = OciConfig::new().try_image_name("test").unwrap();
        let recorder = Recorder::default();

        let result = write_image(
            &config,
            &test_rootfs(),
            &ImageOutput::Layout(output.path().to_path_buf()),
            &recorder,
        )
        .unwrap();

        assert_eq!(result.reference, "test:latest");
        for digest in [
            &result.manifest_digest,
            &result.config_digest,
            &result.layer_digest,
        ] {
            assert!(
                output
                    .path()
                    .join("blobs/sha256")
                    .join(digest.hash())
                    .exists()
            );
        }
        let layout = fs::read(output.path().join("oci-layout")).unwrap();
        let _: OciLayout = serde_json::from_slice(&layout).unwrap();
        let index: ImageIndex =
            serde_json::from_slice(&fs::read(output.path().join("index.json")).unwrap()).unwrap();
        assert_eq!(index.manifests.len(), 1);
        assert_eq!(index.manifests[0].digest, result.manifest_digest);

        let events = recorder.0.into_inner().unwrap();
        assert_eq!(events.first().unwrap(), "begin Image 3");
        assert!(events.last().unwrap().starts_with("end Image"));
    }

    #[test]
    fn a_layout_replaces_the_previous_one() {
        let output = TempDir::new().unwrap();
        let dir = output.path().join("layout");
        let layout = ImageOutput::Layout(dir.clone());
        let _ = write_image(&OciConfig::new(), &test_rootfs(), &layout, &()).unwrap();
        let stale = dir.join("blobs/sha256/stale");
        fs::write(&stale, "from an earlier lock").unwrap();

        let _ = write_image(
            &OciConfig::new().try_image_tag("v2").unwrap(),
            &test_rootfs(),
            &layout,
            &(),
        )
        .unwrap();
        assert!(!stale.exists());
        assert!(dir.join("oci-layout").is_file());
        // Nothing staged is left behind.
        let left: Vec<_> = fs::read_dir(output.path()).unwrap().collect();
        assert_eq!(left.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn a_layout_wont_replace_other_directories() {
        let output = TempDir::new().unwrap();
        let precious = output.path().join("src");
        fs::create_dir(&precious).unwrap();
        fs::write(precious.join("main.rs"), "fn main() {}").unwrap();

        let err = write_image(
            &OciConfig::new(),
            &test_rootfs(),
            &ImageOutput::Layout(precious.clone()),
            &(),
        )
        .unwrap_err();
        assert!(matches!(err, OciError::NotALayout(_)), "{err}");
        assert!(precious.join("main.rs").exists());
        let left: Vec<_> = fs::read_dir(output.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "staging left behind");

        // An empty directory is fine.
        let empty = output.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let _ = write_image(
            &OciConfig::new(),
            &test_rootfs(),
            &ImageOutput::Layout(empty.clone()),
            &(),
        )
        .unwrap();
        assert!(empty.join("index.json").is_file());
    }

    #[test]
    fn test_tarball_is_byte_identical_across_builds() {
        let output = TempDir::new().unwrap();
        let config = OciConfig::new().created(Timestamp::epoch());
        let paths = [output.path().join("a.tar"), output.path().join("b.tar")];

        let results: Vec<_> = paths
            .iter()
            .map(|p| {
                write_image(
                    &config,
                    &test_rootfs(),
                    &ImageOutput::Tarball(p.clone()),
                    &(),
                )
                .unwrap()
            })
            .collect();

        assert_eq!(results[0].manifest_digest, results[1].manifest_digest);
        assert_eq!(fs::read(&paths[0]).unwrap(), fs::read(&paths[1]).unwrap());

        let mut archive = tar::Archive::new(fs::File::open(&paths[0]).unwrap());
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(&names[..2], ["blobs/", "blobs/sha256/"]);
        assert!(names.contains(&"index.json".to_string()));
        assert!(names.contains(&"oci-layout".to_string()));
    }

    #[test]
    fn test_layout_and_tarball_have_the_same_digest() {
        let output = TempDir::new().unwrap();
        let config = OciConfig::new();
        let layout = write_image(
            &config,
            &test_rootfs(),
            &ImageOutput::Layout(output.path().join("layout")),
            &(),
        )
        .unwrap();
        let tarball = write_image(
            &config,
            &test_rootfs(),
            &ImageOutput::Tarball(output.path().join("image.tar")),
            &(),
        )
        .unwrap();
        assert_eq!(layout.manifest_digest, tarball.manifest_digest);
    }
}
