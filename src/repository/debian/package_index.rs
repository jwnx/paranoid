//! Package index for efficient lookups.
//!
//! The `PackageIndex` provides O(1) lookup of packages by name, version,
//! and architecture. It also indexes virtual packages (from `Provides` fields)
//! for dependency resolution.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use thiserror::Error;

use super::architecture::Architecture;
use super::package::{DebianPackage, ParseDebianPackageError};
use super::provides::ProvidesEntry;
use super::version::DebianVersion;

/// Errors that can occur when parsing a Packages index file.
#[derive(Debug, Error)]
#[error("stanza {stanza_index}: {source}")]
pub struct ParsePackageIndexError {
    pub stanza_index: usize,
    #[source]
    pub source: ParseDebianPackageError,
}

/// A virtual package provider entry. The provider is shared with the
/// index's own entry for it rather than copied.
#[derive(Debug, Clone)]
pub struct VirtualPackageProvider {
    pub provider: Arc<DebianPackage>,
    pub provided_version: Option<DebianVersion>,
}

/// Index of packages for efficient lookups.
///
/// Packages are indexed by architecture, name, and version for O(1) access.
/// Virtual packages are also indexed separately for dependency resolution.
///
/// # Example
///
/// ```no_run
/// use std::str::FromStr;
/// use paranoid::repository::debian::{PackageIndex, Architecture, DebianVersion};
/// use paranoid::version::Version;
///
/// // Parse from a Packages file
/// let contents = std::fs::read_to_string("Packages").unwrap();
/// let index = PackageIndex::from_str(&contents).unwrap();
///
/// // Look up a specific package by version
/// let version = DebianVersion::from_str("2.36-9+deb12u4").unwrap();
/// if let Some(pkg) = index.get("libc6", version, Architecture::Amd64) {
///     println!("Found: {}", pkg.filename);
/// }
///
/// // Get the latest version
/// if let Some(pkg) = index.latest_version("libc6", Architecture::Amd64) {
///     println!("Latest: {}", pkg.version);
/// }
///
/// // Look up providers of a virtual package
/// let providers = index.virtual_providers("awk", Architecture::Amd64);
/// ```
#[derive(Debug, Default)]
pub struct PackageIndex {
    /// packages[(arch, name)][version] = Package
    packages: HashMap<(Architecture, String), HashMap<DebianVersion, Arc<DebianPackage>>>,
    /// virtual_packages[(arch, name)] = Vec<VirtualPackageProvider>
    virtual_packages: HashMap<(Architecture, String), Vec<VirtualPackageProvider>>,
}

impl PackageIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a package to the index.
    pub fn add(&mut self, pkg: DebianPackage) {
        // NOTE(juliana): Skipping packages without architecture. Should we?
        let arch = match pkg.architecture {
            Some(a) => a,
            None => return,
        };

        let pkg = Arc::new(pkg);
        for provide in &pkg.provides {
            self.add_virtual_package(arch, provide, &pkg);
        }

        self.packages
            .entry((arch, pkg.name.clone()))
            .or_default()
            .insert(pkg.version.clone(), pkg);
    }

    /// Parse a `Packages` file and add every package in it, so several
    /// components can share one index without copying it.
    pub fn add_stanzas(&mut self, contents: &str) -> Result<(), ParsePackageIndexError> {
        // Stanzas are separated by blank lines, `\n\n` or `\r\n\r\n`.
        let stanzas = contents
            .split("\n\n")
            .flat_map(|s| s.split("\r\n\r\n"))
            .filter(|s| !s.trim().is_empty());
        for (i, stanza) in stanzas.enumerate() {
            let pkg = DebianPackage::from_str(stanza).map_err(|e| ParsePackageIndexError {
                stanza_index: i,
                source: e,
            })?;
            self.add(pkg);
        }
        Ok(())
    }

    /// Add a virtual package entry.
    fn add_virtual_package(
        &mut self,
        arch: Architecture,
        provide: &ProvidesEntry,
        provider: &Arc<DebianPackage>,
    ) {
        let entry = VirtualPackageProvider {
            provider: Arc::clone(provider),
            provided_version: provide.version.clone(),
        };

        self.virtual_packages
            .entry((arch, provide.name.clone()))
            .or_default()
            .push(entry);
    }

    /// Get a specific package by name, version, and architecture.
    #[must_use]
    pub fn get(
        &self,
        name: &str,
        version: DebianVersion,
        arch: Architecture,
    ) -> Option<&DebianPackage> {
        self.packages
            .get(&(arch, name.to_string()))
            .and_then(|by_version| by_version.get(&version))
            .map(Arc::as_ref)
    }

    // Get all available versions of a package for an architecture.
    pub fn versions(&self, name: &str, arch: Architecture) -> Vec<&DebianPackage> {
        self.packages
            .get(&(arch, name.to_string()))
            .map(|by_version| by_version.values().map(Arc::as_ref).collect())
            .unwrap_or_default()
    }

    /// Get packages that provide a virtual package.
    pub fn virtual_providers(
        &self,
        name: &str,
        arch: Architecture,
    ) -> Vec<&VirtualPackageProvider> {
        self.virtual_packages
            .get(&(arch, name.to_string()))
            .map(|providers| providers.iter().collect())
            .unwrap_or_default()
    }

    /// Check if a package name exists (either real or virtual).
    #[must_use]
    pub fn contains(&self, name: &str, arch: Architecture) -> bool {
        let key = (arch, name.to_string());
        self.packages.contains_key(&key) || self.virtual_packages.contains_key(&key)
    }

    /// Get the total number of packages in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packages
            .values()
            .map(|by_version| by_version.len())
            .sum()
    }

    /// Check if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Get the latest version of a package.
    //
    // Uses Debian version comparison to find the highest version.
    #[must_use]
    pub fn latest_version(&self, name: &str, arch: Architecture) -> Option<&DebianPackage> {
        self.versions(name, arch)
            .into_iter()
            .max_by_key(|p| &p.version)
    }

    /// Get all packages in the index.
    pub fn packages(&self) -> Vec<&DebianPackage> {
        self.packages
            .values()
            .flat_map(|by_version| by_version.values().map(Arc::as_ref))
            .collect()
    }
}

impl FromStr for PackageIndex {
    type Err = ParsePackageIndexError;

    fn from_str(contents: &str) -> Result<Self, Self::Err> {
        let mut index = PackageIndex::new();
        index.add_stanzas(contents)?;
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::super::architecture::Architecture;
    use super::super::dependency::PackageDependency;
    use super::super::priority::Priority;
    use super::super::test_utils::{make_package, provides};
    use super::*;

    #[test]
    fn test_add_and_lookup_package() {
        let mut index = PackageIndex::new();
        index.add(make_package("libc6", "2.36-9", "amd64"));

        let pkg = index.get(
            "libc6",
            DebianVersion::from_str("2.36-9").unwrap(),
            Architecture::Amd64,
        );

        assert!(pkg.is_some());
        assert_eq!(pkg.unwrap().name, "libc6");
    }

    #[test]
    fn test_lookup_nonexistent_package() {
        let index = PackageIndex::new();
        assert!(
            index
                .get(
                    "notexist",
                    DebianVersion::from_str("1.0").unwrap(),
                    Architecture::Amd64
                )
                .is_none()
        );
    }

    #[test]
    fn test_virtual_package_simple() {
        let mut index = PackageIndex::new();
        let mut mawk = make_package("mawk", "1.3.4", "amd64");
        mawk.provides = provides(&["awk"]);
        index.add(mawk);

        let providers = index.virtual_providers("awk", Architecture::Amd64);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider.name, "mawk");
        assert!(providers[0].provided_version.is_none());
    }

    #[test]
    fn test_virtual_package_with_version() {
        let mut index = PackageIndex::new();
        let mut libssl3 = make_package("libssl3", "3.0.0", "amd64");
        libssl3.provides = provides(&["libssl (= 3.0.0)"]);
        index.add(libssl3);

        let providers = index.virtual_providers("libssl", Architecture::Amd64);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider.name, "libssl3");
        assert_eq!(
            providers[0].provided_version,
            Some(DebianVersion::from_str("3.0.0").unwrap())
        );
    }

    #[test]
    fn test_multiple_virtual_providers() {
        let mut index = PackageIndex::new();

        let mut mawk = make_package("mawk", "1.3.4", "amd64");
        mawk.provides = provides(&["awk"]);

        let mut gawk = make_package("gawk", "5.1.0", "amd64");
        gawk.provides = provides(&["awk"]);

        let mut original_awk = make_package("original-awk", "2012", "amd64");
        original_awk.provides = provides(&["awk"]);

        index.add(mawk);
        index.add(gawk);
        index.add(original_awk);

        let providers = index.virtual_providers("awk", Architecture::Amd64);
        assert_eq!(providers.len(), 3);

        let names: Vec<_> = providers.iter().map(|p| p.provider.name.as_str()).collect();
        assert!(names.contains(&"mawk"));
        assert!(names.contains(&"gawk"));
        assert!(names.contains(&"original-awk"));
    }

    #[test]
    fn test_contains() {
        let mut index = PackageIndex::new();
        let mut mawk = make_package("mawk", "1.3.4", "amd64");
        mawk.provides = provides(&["awk"]);
        index.add(mawk);

        // Real package exists
        assert!(index.contains("mawk", Architecture::Amd64));
        // Virtual package exists
        assert!(index.contains("awk", Architecture::Amd64));
        // Non-existent package
        assert!(!index.contains("notexist", Architecture::Amd64));
        // Wrong architecture
        assert!(!index.contains("mawk", Architecture::I386));
    }

    #[test]
    fn test_index_len() {
        let mut index = PackageIndex::new();
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());

        index.add(make_package("foo", "1.0", "amd64"));
        index.add(make_package("bar", "2.0", "amd64"));
        index.add(make_package("foo", "1.0", "i386"));

        assert_eq!(index.len(), 3);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_parse_provides_entry() {
        let entry = ProvidesEntry::from_str("awk").unwrap();
        assert_eq!(entry.name, "awk");
        assert_eq!(entry.version, None);

        let entry = ProvidesEntry::from_str("libssl (= 3.0.0)").unwrap();
        assert_eq!(entry.name, "libssl");
        assert_eq!(
            entry.version,
            Some(DebianVersion::from_str("3.0.0").unwrap())
        );

        let entry = ProvidesEntry::from_str(" libfoo ").unwrap();
        assert_eq!(entry.name, "libfoo");
        assert_eq!(entry.version, None);
    }

    #[test]
    fn test_different_architectures() {
        let mut index = PackageIndex::new();
        index.add(make_package("libc6", "2.36-9", "amd64"));
        index.add(make_package("libc6", "2.36-9", "i386"));
        index.add(make_package("libc6", "2.36-9", "arm64"));

        let v = DebianVersion::from_str("2.36-9").unwrap();
        assert!(index.get("libc6", v.clone(), Architecture::Amd64).is_some());
        assert!(index.get("libc6", v.clone(), Architecture::I386).is_some());
        assert!(index.get("libc6", v.clone(), Architecture::Arm64).is_some());
        assert!(index.get("libc6", v.clone(), Architecture::Armhf).is_none());
    }

    // Tests from packages.rs (now using FromStr)

    #[test]
    fn test_from_str_single() {
        let contents = r#"Package: libc6
Version: 2.36-9+deb12u4
Architecture: amd64
Installed-Size: 12345
Depends: libgcc-s1
Filename: pool/main/g/glibc/libc6_2.36-9+deb12u4_amd64.deb
Size: 2897636
SHA256: abc123def456
Priority: required
"#;

        let index = PackageIndex::from_str(contents).unwrap();
        assert_eq!(index.len(), 1);

        let version = DebianVersion::from_str("2.36-9+deb12u4").unwrap();

        let pkg = index.get("libc6", version, Architecture::Amd64).unwrap();
        assert_eq!(pkg.name, "libc6");
        assert_eq!(pkg.version.to_string(), "2.36-9+deb12u4");
        assert_eq!(pkg.architecture, Some(Architecture::Amd64));
        assert_eq!(pkg.installed_size, 12345 * 1024);
        assert_eq!(pkg.depends.len(), 1);
        match &pkg.depends[0] {
            PackageDependency::Required(spec) => assert_eq!(spec.name, "libgcc-s1"),
            _ => panic!("Expected Required dependency"),
        }
        assert_eq!(
            pkg.filename,
            "pool/main/g/glibc/libc6_2.36-9+deb12u4_amd64.deb"
        );
        assert_eq!(pkg.size, 2897636);
        assert_eq!(pkg.sha256, "abc123def456");
        assert_eq!(pkg.priority, Some(Priority::Required));
    }

    #[test]
    fn test_from_str_multiple() {
        let contents = r#"Package: foo
Version: 1.0
Architecture: amd64
Filename: pool/main/f/foo/foo_1.0_amd64.deb
Size: 1000

Package: bar
Version: 2.0
Architecture: all
Filename: pool/main/b/bar/bar_2.0_all.deb
Size: 2000
Depends: foo, baz (>= 1.0)
"#;

        let index = PackageIndex::from_str(contents).unwrap();
        assert_eq!(index.len(), 2);

        assert!(index.contains("foo", Architecture::Amd64));
        assert!(index.contains("bar", Architecture::All));
        assert!(!index.contains("baz", Architecture::Amd64));

        let v1 = DebianVersion::from_str("1.0").unwrap();
        let v2 = DebianVersion::from_str("2.0").unwrap();

        let foo = index.get("foo", v1, Architecture::Amd64).unwrap();
        assert_eq!(foo.architecture, Some(Architecture::Amd64));

        let bar = index.get("bar", v2, Architecture::All).unwrap();
        assert_eq!(bar.architecture, Some(Architecture::All));
        assert_eq!(bar.depends.len(), 2);
    }

    #[test]
    fn test_from_str_multiple_versions() {
        let contents = r#"Package: libc6
Version: 2.35-1
Architecture: amd64
Filename: pool/main/g/glibc/libc6_2.35-1_amd64.deb
Size: 1000

Package: libc6
Version: 2.36-9
Architecture: amd64
Filename: pool/main/g/glibc/libc6_2.36-9_amd64.deb
Size: 2000
"#;

        let index = PackageIndex::from_str(contents).unwrap();
        assert_eq!(index.len(), 2);

        let versions = index.versions("libc6", Architecture::Amd64);
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn test_from_str_incomplete_stanza_error() {
        let contents = r#"Package: incomplete
Version: 1.0

Package: complete
Version: 2.0
Architecture: amd64
Filename: pool/main/c/complete/complete_2.0_amd64.deb
Size: 1000
"#;

        let result = PackageIndex::from_str(contents);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.stanza_index, 0); // First stanza is incomplete
    }

    #[test]
    fn test_from_str_provides() {
        let contents = r#"Package: mawk
Version: 1.3.4
Architecture: amd64
Provides: awk
Filename: pool/main/m/mawk/mawk_1.3.4_amd64.deb
Size: 100
"#;

        let index = PackageIndex::from_str(contents).unwrap();
        let version = DebianVersion::from_str("1.3.4").unwrap();
        let mawk = index.get("mawk", version, Architecture::Amd64).unwrap();
        assert_eq!(mawk.provides.len(), 1);
        assert_eq!(mawk.provides[0].name, "awk");
        assert!(mawk.provides[0].version.is_none());

        // Virtual package should be indexed
        assert!(index.contains("awk", Architecture::Amd64));
        let providers = index.virtual_providers("awk", Architecture::Amd64);
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn test_from_str_real_world() {
        let contents = r#"Package: libc6
Source: glibc
Version: 2.36-9+deb12u4
Installed-Size: 13696
Maintainer: GNU Libc Maintainers <debian-glibc@lists.debian.org>
Architecture: amd64
Replaces: libc6-amd64
Depends: libgcc-s1
Recommends: libnss-nis, libnss-nisplus
Suggests: glibc-doc, debconf | debconf-2.0, libc-l10n, locales
Breaks: busybox (<< 1.30.1-6)
Conflicts: libc6-amd64
Provides: libc6-i686
Section: libs
Priority: required
Multi-Arch: same
Homepage: https://www.gnu.org/software/libc/libc.html
Description: GNU C Library: Shared libraries
 Contains the standard libraries that are used by nearly all programs on
 the system. This package includes shared versions of the standard C library
 and the standard math library, as well as many others.
Filename: pool/main/g/glibc/libc6_2.36-9+deb12u4_amd64.deb
Size: 2897636
MD5sum: d5f4a1bd3f4d3f4a1bd3f4d3f4a1bd3f
SHA256: 1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef
"#;

        let index = PackageIndex::from_str(contents).unwrap();
        let version = DebianVersion::from_str("2.36-9+deb12u4").unwrap();
        assert_eq!(index.len(), 1);

        let pkg = index.get("libc6", version, Architecture::Amd64).unwrap();
        assert_eq!(pkg.name, "libc6");
        assert_eq!(pkg.version.to_string(), "2.36-9+deb12u4");
        assert_eq!(pkg.architecture, Some(Architecture::Amd64));
        assert_eq!(pkg.depends.len(), 1);
        match &pkg.depends[0] {
            PackageDependency::Required(spec) => assert_eq!(spec.name, "libgcc-s1"),
            _ => panic!("Expected Required dependency"),
        }
        assert_eq!(pkg.provides.len(), 1);
        assert_eq!(pkg.provides[0].name, "libc6-i686");
        assert!(pkg.provides[0].version.is_none());
        assert_eq!(pkg.priority, Some(Priority::Required));
        assert_eq!(
            pkg.description,
            Some("GNU C Library: Shared libraries".to_string())
        );
        assert_eq!(
            pkg.sha256,
            "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
        );
    }
}
