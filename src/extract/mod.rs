//! Package extraction support.
//!
//! This module provides:
//! - Archive trait for different archive formats
//! - Implementations: tar (with compression), ar, deb
//! - Parallel extraction with bounded concurrency
//! - Progress reporting via trait
//! - Path traversal protection

mod ar;
mod archive;
mod deb;
mod error;
mod path;
mod progress;
mod service;
mod tar;

pub use ar::{ArArchive, ArMember};
pub use archive::{Archive, ArchiveEntry, ExtractOptions};
pub use deb::DebArchive;
pub use error::{
    EntryType, ExtractedFile, ExtractedPackage, ExtractionError, ExtractionFailure,
    ExtractionResult,
};
pub use progress::{ExtractionProgress, NoProgress, StderrProgress};
pub use service::{ExtractionConfig, ExtractionMode, ExtractionService};
pub use tar::{Compression, TarArchive};
