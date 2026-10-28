//! Files fetched from a URL rather than a repository (`[files.<id>]`).
//!
//! `lock` checks each file against the hash the manifest pins, or the
//! signatures of the keys it names, and records the file's hash and size.
//! From then on a file is a blob like a package: `build` fetches it from
//! the stores or its URL, checks it against the lock, and places it in the
//! image after the packages.

mod lock;
mod place;

pub use lock::{FileError, LockError, MAX_FILE_SIZE, lock};
pub use place::{PlaceError, place};
