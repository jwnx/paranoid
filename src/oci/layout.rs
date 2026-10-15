//! Reading images back from an OCI layout directory or tarball.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::digest::Digest;
use super::error::OciError;
use super::spec::{ImageConfig, ImageIndex, ImageManifest};

/// Annotation on an index entry carrying the image reference.
const REF_NAME: &str = "org.opencontainers.image.ref.name";

/// What we can learn about an image from its metadata.
#[derive(Debug, Clone)]
pub struct ImageInfo {
    /// Digest of the image manifest.
    pub manifest_digest: Digest,
    /// The reference recorded in the index, e.g. `node:latest`.
    pub reference: Option<String>,
    pub config: ImageConfig,
    /// Total compressed size of all layers.
    pub layers_size: u64,
}

/// Where an image's files are read from.
enum Source {
    Layout(PathBuf),
    Tarball(PathBuf),
}

impl Source {
    /// Read one file of the layout, e.g. `index.json` or `blobs/sha256/<hex>`.
    ///
    /// For a tarball this scans the archive, which reads only the entries up
    /// to the one we need and never buffers the (large) layer blobs.
    fn read(&self, name: &str) -> Result<Vec<u8>, OciError> {
        let read_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| OciError::ReadImage {
                path: path.clone(),
                source,
            }
        };
        match self {
            Source::Layout(dir) => {
                let path = dir.join(name);
                fs::read(&path).map_err(read_err(&path))
            }
            Source::Tarball(path) => {
                let err = read_err(path);
                let mut archive = tar::Archive::new(File::open(path).map_err(&err)?);
                for entry in archive.entries().map_err(&err)? {
                    let mut entry = entry.map_err(&err)?;
                    if entry.path().map_err(&err)?.as_os_str() == name {
                        let mut data = Vec::new();
                        entry.read_to_end(&mut data).map_err(&err)?;
                        return Ok(data);
                    }
                }
                Err(OciError::InvalidImage(format!("{name} not found in image")))
            }
        }
    }

    fn json<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T, OciError> {
        serde_json::from_slice(&self.read(name)?)
            .map_err(|e| OciError::InvalidImage(format!("cannot parse {name}: {e}")))
    }
}

fn blob(digest: &Digest) -> String {
    format!("blobs/sha256/{}", digest.hash())
}

/// Read the metadata of the first image in a layout directory or tarball.
pub fn read_image(path: &Path) -> Result<ImageInfo, OciError> {
    let source = if path.is_dir() {
        Source::Layout(path.to_path_buf())
    } else {
        Source::Tarball(path.to_path_buf())
    };

    let index: ImageIndex = source.json("index.json")?;
    let entry = index
        .manifests
        .first()
        .ok_or_else(|| OciError::InvalidImage("index contains no manifests".to_string()))?;
    let manifest: ImageManifest = source.json(&blob(&entry.digest))?;
    let config: ImageConfig = source.json(&blob(&manifest.config.digest))?;

    Ok(ImageInfo {
        manifest_digest: entry.digest,
        reference: entry
            .annotations
            .as_ref()
            .and_then(|a| a.get(REF_NAME))
            .cloned(),
        config,
        layers_size: manifest
            .layers
            .iter()
            .map(|l| u64::try_from(l.size).unwrap_or(0))
            .sum(),
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::arch::Architecture;
    use crate::oci::{ImageOutput, OciConfig, write_image};
    use crate::rootfs::Rootfs;

    #[test]
    fn reads_back_what_we_write() {
        let dir = TempDir::new().unwrap();
        let config = OciConfig::new()
            .architecture(Architecture::Armhf)
            .entrypoint(["/bin/sh"])
            .try_image_name("demo")
            .unwrap();

        for output in [
            ImageOutput::Layout(dir.path().join("layout")),
            ImageOutput::Tarball(dir.path().join("image.tar")),
        ] {
            let written = write_image(&config, &Rootfs::new(), &output, &()).unwrap();
            let info = read_image(output.path()).unwrap();

            assert_eq!(info.manifest_digest, written.manifest_digest);
            assert_eq!(info.reference.as_deref(), Some("demo:latest"));
            assert_eq!(info.config.architecture, Architecture::Armhf);
            assert!(info.layers_size > 0);
        }
    }

    #[test]
    fn missing_image_is_an_error() {
        assert!(read_image(Path::new("/nonexistent/image.tar")).is_err());
    }
}
