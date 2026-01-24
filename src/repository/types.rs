//! Common traits for package types.

use crate::repository::DependencySpec;

/// Trait for package types that can report their dependencies.
pub trait Package {
    /// Get required dependencies (Depends + Pre-Depends).
    fn dependencies(&self) -> Vec<&DependencySpec>;

    /// Get recommended dependencies (Recommends).
    ///
    /// Returns an empty list if not implemented.
    fn recommends(&self) -> Vec<&DependencySpec> {
        Vec::new()
    }
}
