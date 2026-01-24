pub mod debian;
mod types;

// Re-export debian types
pub use debian::{
    Architecture, ArchitectureRestriction, DebianPackage, DebianRepository, DebianRepositoryError,
    DebianVersion, DependencyList, DependencySpec, PackageDependency, PackageIndex,
    PackageIndexSet, ParseDebianPackageError, ParseDebianVersionError, ParsePackageIndexError,
    Priority, ProvidesEntry, ProvidesList, ResolveBuilder, ResolveError, ResolveResult,
    ResolvedEntry, SourcedPackage, System, UnmetDependency, VirtualPackageProvider,
};

// Re-export common types
pub use types::Package;
