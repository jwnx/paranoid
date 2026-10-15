//! Shared test utilities for Debian repository tests.

use std::str::FromStr;

use super::architecture::Architecture;
use super::dependency::PackageDependency;
use super::package::DebianPackage;
use super::provides::ProvidesEntry;
use super::version::DebianVersion;

/// Create a minimal test package.
pub fn make_package(name: &str, version: &str, arch: &str) -> DebianPackage {
    DebianPackage {
        name: name.to_string(),
        version: DebianVersion::from_str(version).unwrap(),
        architecture: Architecture::from_str(arch).ok(),
        filename: format!("pool/main/{}/{}_{}_{}.deb", &name[..1], name, version, arch),
        ..Default::default()
    }
}

/// Create a test package with dependencies.
///
/// Each dep string can be:
/// - "pkgname" - simple dependency
/// - "pkgname (>= 1.0)" - with version constraint
/// - "pkg1 | pkg2" - alternatives
pub fn make_package_with_deps(
    name: &str,
    version: &str,
    arch: &str,
    depends: Vec<&str>,
) -> DebianPackage {
    let mut pkg = make_package(name, version, arch);
    pkg.depends = depends
        .iter()
        .map(|s| PackageDependency::from_str(s).unwrap_or_else(|e| panic!("{s:?}: {e}")))
        .collect();
    pkg
}

/// Parse provides entries from string slices.
pub fn provides(entries: &[&str]) -> Vec<ProvidesEntry> {
    entries
        .iter()
        .map(|s| ProvidesEntry::from_str(s).unwrap())
        .collect()
}
