//! Lock file handling.
//!
//! The lock file (`paranoid.lock`) records the exact resolved state of all
//! packages, including transitive dependencies, with their checksums.

mod diff;
mod lockfile;

pub use diff::{Change, ChangeKind, LockDiff};
pub use lockfile::{LockFile, LockFileError, LockedImage, LockedPackage, LockedUnmet};
