//! Files fetched from a URL rather than a repository (`[files.<id>]`).
//!
//! `lock` resolves a file from an index to a release, then checks it
//! against the hash the manifest pins, the signatures of the keys it names,
//! or the hash its index lists, and records the file's hash and size. From
//! then on a file is a blob like a package: `build` fetches it from the
//! stores or its URL, checks it against the lock, and places it in the
//! image after the packages.

mod lock;
mod place;
mod resolve;

pub use lock::{FileError, LockError, MAX_FILE_SIZE, Options, lock, lock_with};
pub use place::{PlaceError, place};
pub use resolve::{Expected, Indexes, Release, ResolveError, render_extract};
