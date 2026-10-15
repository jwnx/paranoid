mod architecture;
mod dependency;
mod package;
mod package_index;
mod package_index_set;
mod priority;
mod provides;
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
pub use package::{DebianPackage, ParseDebianPackageError};
pub use package_index::{PackageIndex, ParsePackageIndexError, VirtualPackageProvider};
pub use package_index_set::{PackageIndexSet, SourcedPackage, SourcedProvider};
pub use priority::Priority;
pub use provides::ProvidesEntry;
pub use repository::{DebianRepository, DebianRepositoryError};
pub use resolver::{
    Origin, Request, Requirement, Resolution, ResolveError, Resolved, UnmetDependency, resolve,
};
pub use version::{DebianVersion, ParseDebianVersionError};
