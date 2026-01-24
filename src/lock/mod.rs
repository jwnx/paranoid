//! Lock file handling.
//!
//! The lock file (`paranoid.lock`) records the exact resolved state of all
//! packages, including transitive dependencies, with their checksums.

mod lockfile;
mod sha256;

pub use lockfile::{LockFile, LockFileError, LockedPackage};
pub use sha256::{InvalidSha256Error, Sha256Hash};
