//! Dependency resolution service.
//!
//! Combines config, HTTP client, and resolver to produce a complete
//! resolution of packages from a manifest.

mod service;

pub use service::{ResolveService, ResolveServiceError, ResolvedManifest, ResolvedPackage};
