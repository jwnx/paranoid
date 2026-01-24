use std::str::FromStr;

use thiserror::Error;

use crate::repository::*;

use super::architecture::Architecture;
use super::dependency::{DependencyList, PackageDependency};
use super::priority::Priority;
use super::provides::{ProvidesEntry, ProvidesList};
use super::version::DebianVersion;

/// Error parsing a Debian package stanza.
#[derive(Debug, Error)]
pub enum ParseDebianPackageError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebianPackage {
    pub name: String,
    pub version: DebianVersion,
    pub architecture: Option<Architecture>,
    pub installed_size: u64,
    pub depends: Vec<PackageDependency>,
    pub pre_depends: Vec<PackageDependency>,
    pub provides: Vec<ProvidesEntry>,
    pub filename: String,
    pub size: u64,
    pub md5sum: String,
    pub sha256: String,
    pub priority: Option<Priority>,
    pub description: Option<String>,
}

impl Package for DebianPackage {
    fn dependencies(&self) -> Vec<&DependencySpec> {
        let mut deps = Vec::new();

        // Pre-Depends are required
        for dep in &self.pre_depends {
            if let Some(resolved) = dep.resolve() {
                deps.push(resolved);
            }
        }

        // Depends are required
        for dep in &self.depends {
            if let Some(resolved) = dep.resolve() {
                deps.push(resolved);
            }
        }

        deps
    }
}

/// Parse an RFC 822-style stanza into key-value pairs.
///
/// Handles:
/// - `Key: Value` format
/// - `Key:` with empty value
/// - Continuation lines (starting with space or tab)
fn parse_stanza(stanza: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut current_key = String::new();
    let mut current_value = String::new();

    for line in stanza.lines() {
        if line.is_empty() {
            continue;
        }

        // Check for continuation line (starts with space or tab)
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation of previous field
            if !current_key.is_empty() {
                current_value.push('\n');
                current_value.push_str(line.trim_start());
            }
            continue;
        }

        // Save previous field if any
        if !current_key.is_empty() {
            fields.push((current_key, current_value));
            current_key = String::new();
            current_value = String::new();
        }

        // Parse "Key: Value" or "Key:"
        if let Some(colon_pos) = line.find(':') {
            current_key = line[..colon_pos].to_string();
            current_value = line[colon_pos + 1..].trim_start().to_string();
        }
    }

    // Don't forget the last field
    if !current_key.is_empty() {
        fields.push((current_key, current_value));
    }

    fields
}

impl FromStr for DebianPackage {
    type Err = ParseDebianPackageError;

    fn from_str(stanza: &str) -> Result<Self, Self::Err> {
        let fields = parse_stanza(stanza);
        let mut pkg = DebianPackage::default();
        let mut version: Option<DebianVersion> = None;

        for (key, value) in fields {
            match key.as_str() {
                "Package" => pkg.name = value,
                "Version" => {
                    if let Ok(v) = DebianVersion::from_str(&value) {
                        version = Some(v);
                    }
                }
                "Architecture" => {
                    pkg.architecture = Architecture::from_str(&value).ok();
                }
                "Installed-Size" => {
                    // Installed-Size is in KB
                    pkg.installed_size = value.parse().unwrap_or(0) * 1024;
                }
                "Depends" => {
                    // Lenient: skip packages with malformed dependencies
                    pkg.depends = DependencyList::from_str(&value)
                        .ok()
                        .map(Into::into)
                        .unwrap_or_default();
                }
                "Pre-Depends" => {
                    pkg.pre_depends = DependencyList::from_str(&value)
                        .ok()
                        .map(Into::into)
                        .unwrap_or_default();
                }
                "Provides" => {
                    pkg.provides = ProvidesList::from_str(&value)
                        .ok()
                        .map(Into::into)
                        .unwrap_or_default();
                }
                "Filename" => pkg.filename = value,
                "Size" => pkg.size = value.parse().unwrap_or(0),
                "MD5sum" => pkg.md5sum = value,
                "SHA256" => pkg.sha256 = value,
                "Priority" => pkg.priority = Priority::from_str(&value).ok(),
                "Description" => {
                    // Only take the first line of the description
                    pkg.description = Some(value.lines().next().unwrap_or("").to_string());
                }
                _ => {
                    // Ignore.
                }
            }
        }

        if pkg.name.is_empty() {
            return Err(ParseDebianPackageError::MissingField("Package"));
        }
        pkg.version = version.ok_or(ParseDebianPackageError::MissingField("Version"))?;
        if pkg.architecture.is_none() {
            return Err(ParseDebianPackageError::MissingField("Architecture"));
        }
        if pkg.filename.is_empty() {
            return Err(ParseDebianPackageError::MissingField("Filename"));
        }

        Ok(pkg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stanza_simple() {
        let stanza = "Package: foo\nVersion: 1.0\nArchitecture: amd64";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], ("Package".to_string(), "foo".to_string()));
        assert_eq!(fields[1], ("Version".to_string(), "1.0".to_string()));
        assert_eq!(fields[2], ("Architecture".to_string(), "amd64".to_string()));
    }

    #[test]
    fn test_parse_stanza_continuation_lines() {
        let stanza = "Package: foo\nDescription: Short desc\n Long description\n continues here";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 2);
        assert_eq!(
            fields[1],
            (
                "Description".to_string(),
                "Short desc\nLong description\ncontinues here".to_string()
            )
        );
    }

    #[test]
    fn test_parse_stanza_empty_value() {
        let stanza = "Package: foo\nDepends:\nVersion: 1.0";
        let fields = parse_stanza(stanza);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], ("Depends".to_string(), "".to_string()));
    }

    #[test]
    fn test_debian_package_from_str() {
        let stanza = r#"Package: libc6
Version: 2.36-9+deb12u4
Architecture: amd64
Installed-Size: 12345
Depends: libgcc-s1
Filename: pool/main/g/glibc/libc6_2.36-9+deb12u4_amd64.deb
Size: 2897636
SHA256: abc123def456
Priority: required"#;

        let pkg = DebianPackage::from_str(stanza).unwrap();
        assert_eq!(pkg.name, "libc6");
        assert_eq!(pkg.version.to_string(), "2.36-9+deb12u4");
        assert_eq!(pkg.architecture, Some(Architecture::Amd64));
        assert_eq!(pkg.installed_size, 12345 * 1024);
        assert_eq!(pkg.depends.len(), 1);
        assert_eq!(
            pkg.filename,
            "pool/main/g/glibc/libc6_2.36-9+deb12u4_amd64.deb"
        );
        assert_eq!(pkg.size, 2897636);
        assert_eq!(pkg.sha256, "abc123def456");
        assert_eq!(pkg.priority, Some(Priority::Required));
    }

    #[test]
    fn test_debian_package_from_str_missing_package() {
        let stanza = "Version: 1.0\nArchitecture: amd64\nFilename: test.deb";
        let result = DebianPackage::from_str(stanza);
        assert!(matches!(
            result,
            Err(ParseDebianPackageError::MissingField("Package"))
        ));
    }

    #[test]
    fn test_debian_package_from_str_missing_version() {
        let stanza = "Package: foo\nArchitecture: amd64\nFilename: test.deb";
        let result = DebianPackage::from_str(stanza);
        assert!(matches!(
            result,
            Err(ParseDebianPackageError::MissingField("Version"))
        ));
    }

    #[test]
    fn test_debian_package_from_str_missing_architecture() {
        let stanza = "Package: foo\nVersion: 1.0\nFilename: test.deb";
        let result = DebianPackage::from_str(stanza);
        assert!(matches!(
            result,
            Err(ParseDebianPackageError::MissingField("Architecture"))
        ));
    }

    #[test]
    fn test_debian_package_from_str_missing_filename() {
        let stanza = "Package: foo\nVersion: 1.0\nArchitecture: amd64";
        let result = DebianPackage::from_str(stanza);
        assert!(matches!(
            result,
            Err(ParseDebianPackageError::MissingField("Filename"))
        ));
    }
}
