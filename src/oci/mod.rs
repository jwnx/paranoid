//! OCI image generation support.
//!
//! This module provides:
//! - Deterministic container image creation
//! - Single-layer images with gzip/zstd compression
//! - Standard OCI image layout compatible with podman/docker
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use paranoid::arch::Architecture;
//! use paranoid::oci::{OciConfig, OciImageService, NoProgress, EnvVar, Timestamp};
//! use std::convert::TryFrom;
//!
//! let config = OciConfig::new()
//!     .architecture(Architecture::Amd64)
//!     .entrypoint(["/bin/sh"])
//!     .env([EnvVar::try_from("PATH=/usr/bin:/bin").unwrap()])
//!     .try_image_name("my-image").unwrap()
//!     .try_image_tag("v1.0").unwrap();
//!
//! let service = OciImageService::with_config(config);
//! let result = service.generate_tarball(
//!     Path::new("./rootfs"),
//!     Path::new("./image.tar"),
//!     &NoProgress,
//! ).expect("failed to generate image");
//!
//! println!("Generated image: {}", result.manifest_digest);
//! // Load with: podman load -i ./image.tar
//! ```

mod blob;
mod digest;
mod error;
mod layer;
mod progress;
mod service;
mod spec;
pub mod types;

pub use digest::{Digest, DigestError};
pub use error::{OciError, OciImageResult};
pub use layer::{Compression, Layer};
pub use progress::{NoProgress, OciProgress, StderrProgress};
pub use service::{OciConfig, OciConfigError, OciImageService};
pub use spec::{
    ContainerConfig, Descriptor, History, ImageConfig, ImageIndex, ImageManifest, MediaType,
    OciLayout, RootFs,
};
pub use types::{
    AbsolutePath, AbsolutePathError, EnvVar, EnvVarError, ImageName, ImageNameError, ImageTag,
    ImageTagError, ParseSignalError, ParseTimestampError, Signal, Timestamp,
};
