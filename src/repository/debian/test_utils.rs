//! Shared test utilities for Debian repository tests.

use std::str::FromStr;

use super::architecture::Architecture;
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

/// Parse provides entries from string slices.
pub fn provides(entries: &[&str]) -> Vec<ProvidesEntry> {
    entries
        .iter()
        .map(|s| ProvidesEntry::from_str(s).unwrap())
        .collect()
}
