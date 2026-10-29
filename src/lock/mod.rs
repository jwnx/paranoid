//! Lock file handling.
//!
//! The lock file (`paranoid.lock`) records the exact resolved state of all
//! packages, including transitive dependencies, with their checksums.

mod diff;
mod lockfile;
mod summary;

pub use diff::{Change, ChangeKind, FileChange, LockDiff};
pub use lockfile::{
    ExpectedDigest, LockFile, LockFileError, LockedFile, LockedImage, LockedPackage, LockedUnmet,
    Trust, Unchecked,
};
pub use summary::LockSummary;
