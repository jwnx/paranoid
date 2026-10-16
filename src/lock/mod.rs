//! Lock file handling.
//!
//! The lock file (`paranoid.lock`) records the exact resolved state of all
//! packages, including transitive dependencies, with their checksums.

mod lockfile;

pub use lockfile::{
    Change, ChangeKind, LockDiff, LockFile, LockFileError, LockedImage, LockedPackage, LockedUnmet,
};
