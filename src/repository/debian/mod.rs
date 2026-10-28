mod architecture;
mod deb822;
mod dependency;
mod keyring;
mod package;
mod package_index;
mod package_index_set;
mod priority;
mod provides;
mod release;
mod repository;
mod resolver;
#[cfg(test)]
pub(crate) mod test_utils;
mod version;

// Re-export public types
pub use architecture::Architecture;
pub use dependency::{
    DependencySpec, PackageDependency, ParseDependencySpecError, ParseRelationsError,
    parse_relations,
};
pub use keyring::{Fingerprint, KeyTime, Keyring, KeyringError, Verified, VerifyError};
pub use package::{DebianPackage, ParseDebianPackageError};
pub use package_index::{PackageIndex, ParsePackageIndexError, VirtualPackageProvider};
pub use package_index_set::{
    DEFAULT_PIN_PRIORITY, PackageIndexSet, SourcedPackage, SourcedProvider,
};
pub use priority::Priority;
pub use provides::ProvidesEntry;
pub use release::{IndexFile, ParseReleaseError, Release, ReleaseError};
pub use repository::{DebianRepository, DebianRepositoryError, Snapshot};
pub use resolver::{
    Hold, Origin, Request, Requirement, Resolution, ResolveError, Resolved, UnmetDependency,
    check_sources, resolve,
};
pub use version::{DebianVersion, ParseDebianVersionError};
