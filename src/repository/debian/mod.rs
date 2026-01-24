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
pub use architecture::{Architecture, ArchitectureRestriction, System};
pub use dependency::{DependencyList, DependencySpec, PackageDependency};
pub use package::{DebianPackage, ParseDebianPackageError};
pub use package_index::{PackageIndex, ParsePackageIndexError, VirtualPackageProvider};
pub use package_index_set::{PackageIndexSet, SourcedPackage};
pub use priority::Priority;
pub use provides::{ProvidesEntry, ProvidesList};
pub use repository::{DebianRepository, DebianRepositoryError};
pub use resolver::{ResolveBuilder, ResolveError, ResolveResult, ResolvedEntry, UnmetDependency};
pub use version::{DebianVersion, ParseDebianVersionError};
