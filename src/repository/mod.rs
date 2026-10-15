pub mod debian;

// Re-export debian types
pub use debian::{
    Architecture, DebianPackage, DebianRepository, DebianRepositoryError, DebianVersion,
    DependencySpec, PackageDependency, PackageIndex, PackageIndexSet, ParseDebianPackageError,
    ParseDebianVersionError, ParsePackageIndexError, Priority, ProvidesEntry, Request, Resolution,
    ResolveError, Resolved, SourcedPackage, UnmetDependency, VirtualPackageProvider,
};
