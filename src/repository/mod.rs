pub mod debian;
mod types;

// Re-export debian types
pub use debian::{
    Architecture, DebianPackage, DebianRepository, DebianRepositoryError, DebianVersion,
    DependencySpec, PackageDependency, PackageIndex, PackageIndexSet, ParseDebianPackageError,
    ParseDebianVersionError, ParsePackageIndexError, Priority, ProvidesEntry, ResolveBuilder,
    ResolveError, ResolveResult, ResolvedEntry, SourcedPackage, UnmetDependency,
    VirtualPackageProvider,
};

// Re-export common types
pub use types::Package;
