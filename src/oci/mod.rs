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
//! use paranoid::arch::Architecture;
//! use paranoid::oci::{write_image, EnvVar, ImageOutput, OciConfig};
//! use paranoid::rootfs::Rootfs;
//! use std::convert::TryFrom;
//!
//! let config = OciConfig::new()
//!     .architecture(Architecture::Amd64)
//!     .entrypoint(["/bin/sh"])
//!     .env([EnvVar::try_from("PATH=/usr/bin:/bin").unwrap()])
//!     .try_image_name("my-image").unwrap()
//!     .try_image_tag("v1.0").unwrap();
//!
//! let rootfs = Rootfs::new(); // normally assembled from packages
//! let result = write_image(&config, &rootfs, &ImageOutput::from_path("image.tar"), &())
//!     .expect("failed to generate image");
//!
//! println!("Generated image: {}", result.manifest_digest);
//! // Load with: podman load -i ./image.tar
//! ```

mod digest;
mod error;
mod layer;
mod layout;
mod service;
mod spec;
pub mod types;

pub use digest::{Digest, DigestError};
pub use error::{OciError, OciImageResult};
pub use layer::{Compression, Layer};
pub use layout::{ImageInfo, read_image};
pub use service::{ImageOutput, OciConfig, OciConfigError, write_image};
pub use spec::{
    ContainerConfig, Descriptor, History, ImageConfig, ImageIndex, ImageManifest, MediaType,
    OciLayout, RootFs,
};
pub use types::{
    AbsolutePath, AbsolutePathError, EnvVar, EnvVarError, ImageName, ImageNameError, ImageTag,
    ImageTagError, ParseSignalError, ParseTimestampError, Signal, Timestamp,
};
