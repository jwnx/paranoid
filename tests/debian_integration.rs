//! Integration tests for Debian package parsing and resolution.
//!
//! These tests use real Debian package data to verify the parser and resolver
//! work correctly with production data.

use std::str::FromStr;

use paranoid::repository::debian::{Architecture, PackageIndexSet, PackageIndex, Priority};

/// Load the test fixture containing real Debian packages.
fn load_fixture() -> PackageIndex {
    let contents = include_str!("fixtures/sample_packages.txt");
    PackageIndex::from_str(contents).expect("Failed to parse fixture")
}

/// Helper to create an PackageIndexSet from a PackageIndex.
fn make_index_set(pkg_index: PackageIndex) -> PackageIndexSet {
    let mut set = PackageIndexSet::new();
    set.add_index("test".to_string(), pkg_index);
    set
}

#[test]
fn test_parse_real_packages_file() {
    let index = load_fixture();

    // We should have parsed all 12 packages
    assert_eq!(index.len(), 12, "Expected 12 packages in fixture");

    // Verify some known packages exist
    assert!(
        index.contains("libc6", Architecture::Amd64),
        "libc6 should exist"
    );
    assert!(
        index.contains("bash", Architecture::Amd64),
        "bash should exist"
    );
    assert!(
        index.contains("coreutils", Architecture::Amd64),
        "coreutils should exist"
    );
}

#[test]
fn test_libc6_package_details() {
    let index = load_fixture();

    let libc6 = index
        .latest_version("libc6", Architecture::Amd64)
        .expect("libc6 should exist");

    assert_eq!(libc6.name, "libc6");
    assert!(
        libc6.version.to_string().starts_with("2."),
        "libc6 version should start with 2."
    );
    assert_eq!(libc6.architecture, Some(Architecture::Amd64));

    // libc6 should have dependencies (libgcc-s1)
    assert!(!libc6.depends.is_empty(), "libc6 should have dependencies");

    // Verify it depends on libgcc-s1
    let depends_on_libgcc = libc6.depends.iter().any(|dep| {
        use paranoid::repository::debian::PackageDependency;
        match dep {
            PackageDependency::Required(spec) => spec.name == "libgcc-s1",
            PackageDependency::Alternate(specs) => specs.iter().any(|s| s.name == "libgcc-s1"),
        }
    });
    assert!(depends_on_libgcc, "libc6 should depend on libgcc-s1");
}

#[test]
fn test_bash_package_details() {
    let index = load_fixture();

    let bash = index
        .latest_version("bash", Architecture::Amd64)
        .expect("bash should exist");

    assert_eq!(bash.name, "bash");
    assert_eq!(bash.architecture, Some(Architecture::Amd64));
    assert_eq!(bash.priority, Some(Priority::Required));

    // bash should have pre-dependencies on libc6
    let pre_depends_on_libc6 = bash.pre_depends.iter().any(|dep| {
        use paranoid::repository::debian::PackageDependency;
        match dep {
            PackageDependency::Required(spec) => spec.name == "libc6",
            PackageDependency::Alternate(specs) => specs.iter().any(|s| s.name == "libc6"),
        }
    });
    assert!(pre_depends_on_libc6, "bash should pre-depend on libc6");

    // bash should also depend on base-files and debianutils
    let depends_on_base_files = bash.depends.iter().any(|dep| {
        use paranoid::repository::debian::PackageDependency;
        match dep {
            PackageDependency::Required(spec) => spec.name == "base-files",
            PackageDependency::Alternate(specs) => specs.iter().any(|s| s.name == "base-files"),
        }
    });
    assert!(depends_on_base_files, "bash should depend on base-files");
}

#[test]
fn test_virtual_package_awk() {
    let index = load_fixture();

    // Both mawk and gawk provide "awk"
    let mawk = index
        .latest_version("mawk", Architecture::Amd64)
        .expect("mawk should exist");
    let gawk = index
        .latest_version("gawk", Architecture::Amd64)
        .expect("gawk should exist");

    assert!(
        mawk.provides.iter().any(|p| p.name == "awk"),
        "mawk should provide awk"
    );
    assert!(
        gawk.provides.iter().any(|p| p.name == "awk"),
        "gawk should provide awk"
    );

    // Virtual package lookup should work
    let awk_providers = index.virtual_providers("awk", Architecture::Amd64);
    assert_eq!(awk_providers.len(), 2, "Should have 2 awk providers");

    let provider_names: Vec<_> = awk_providers
        .iter()
        .map(|p| p.provider.name.as_str())
        .collect();
    assert!(provider_names.contains(&"mawk"));
    assert!(provider_names.contains(&"gawk"));
}

#[test]
fn test_ca_certificates_arch_all() {
    let index = load_fixture();

    let ca_certs = index
        .latest_version("ca-certificates", Architecture::All)
        .expect("ca-certificates should exist");

    assert_eq!(ca_certs.name, "ca-certificates");
    assert_eq!(
        ca_certs.architecture,
        Some(Architecture::All),
        "ca-certificates should be arch: all"
    );
}

#[test]
fn test_resolver_with_real_packages() {
    let pkg_index = load_fixture();
    let index = make_index_set(pkg_index);

    // Resolve bash - should find it
    let bash = index.resolve("bash", None, Architecture::Amd64);
    assert!(bash.is_some(), "Should resolve bash");
    assert_eq!(bash.unwrap().package.name, "bash");

    // Resolve virtual package "awk"
    // Should prefer mawk (priority: required) over gawk (priority: optional)
    let awk = index.resolve("awk", None, Architecture::Amd64);
    assert!(awk.is_some(), "Should resolve virtual package awk");
    assert_eq!(
        awk.unwrap().package.name, "mawk",
        "awk should resolve to mawk (priority: required) over gawk (priority: optional)"
    );
}

#[test]
fn test_resolver_fallback_to_arch_all() {
    let pkg_index = load_fixture();
    let index = make_index_set(pkg_index);

    // ca-certificates is only available as arch: all
    // Requesting it for amd64 should fall back to "all"
    let ca_certs = index.resolve("ca-certificates", None, Architecture::Amd64);
    assert!(
        ca_certs.is_some(),
        "Should resolve ca-certificates via arch:all fallback"
    );
    assert_eq!(
        ca_certs.unwrap().package.architecture,
        Some(Architecture::All)
    );
}

#[test]
fn test_resolver_nonexistent_package() {
    let pkg_index = load_fixture();
    let index = make_index_set(pkg_index);

    let result = index
        .resolve_all("this-package-does-not-exist", None, Architecture::Amd64)
        .execute();
    assert!(result.is_err(), "Should fail for nonexistent package");
}

#[test]
fn test_parse_malformed_packages_file() {
    // Missing required fields
    let malformed = r#"Package: broken
Version: 1.0
"#;

    let result = PackageIndex::from_str(malformed);
    assert!(
        result.is_err(),
        "Should fail to parse package missing Architecture"
    );

    // Verify the error contains useful info
    let err = result.unwrap_err();
    assert_eq!(err.stanza_index, 0, "Error should reference first stanza");
}

#[test]
fn test_parse_empty_packages_file() {
    let empty = "";
    let result = PackageIndex::from_str(empty);
    assert!(result.is_ok(), "Empty file should parse successfully");
    assert_eq!(result.unwrap().len(), 0, "Empty file should have 0 packages");
}

#[test]
fn test_parse_packages_with_only_whitespace() {
    let whitespace = "   \n\n   \n\t\n   ";
    let result = PackageIndex::from_str(whitespace);
    assert!(result.is_ok(), "Whitespace-only file should parse successfully");
    assert_eq!(result.unwrap().len(), 0);
}

#[test]
fn test_version_lookup() {
    let index = load_fixture();

    // Get all versions of a package
    let libc6_versions = index.versions("libc6", Architecture::Amd64);
    assert!(
        !libc6_versions.is_empty(),
        "Should have at least one libc6 version"
    );

    // Latest version should be accessible
    let latest = index.latest_version("libc6", Architecture::Amd64);
    assert!(latest.is_some(), "Should have a latest version");
}

/// Test that we can parse a larger portion of the real Packages file.
/// This test is ignored by default as it requires network access.
#[test]
#[ignore]
fn test_parse_full_packages_file() {
    use std::process::Command;

    // Download and decompress the full Packages file
    let output = Command::new("sh")
        .arg("-c")
        .arg("curl -s http://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.gz | gunzip")
        .output()
        .expect("Failed to download Packages file");

    assert!(output.status.success(), "Download should succeed");

    let contents = String::from_utf8(output.stdout).expect("Invalid UTF-8");

    // Parse the full file
    let index = PackageIndex::from_str(&contents).expect("Should parse full Packages file");

    // Should have thousands of packages
    assert!(
        index.len() > 50000,
        "Should have >50k packages, got {}",
        index.len()
    );

    // Verify common packages exist
    assert!(index.contains("linux-image-amd64", Architecture::Amd64));
    assert!(index.contains("apt", Architecture::Amd64));
    assert!(index.contains("dpkg", Architecture::Amd64));
    assert!(index.contains("systemd", Architecture::Amd64));

    println!("Successfully parsed {} packages", index.len());
}

/// Test the DebianRepository can fetch and parse packages from a real mirror.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_debian_repository_fetch_packages() {
    use paranoid::config::RepositoryConfig;
    use paranoid::http::Client;
    use paranoid::repository::debian::DebianRepository;

    let client = Client::new().expect("Should create client");
    let config = RepositoryConfig {
        url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
        distribution: "bookworm".to_string(),
        components: vec!["main".to_string()],
    };
    let repo = DebianRepository::new(&client, "debian", &config);

    // Fetch the full index
    let index = repo
        .fetch_index(Architecture::Amd64)
        .await
        .expect("Should fetch index");

    // Should have many packages
    assert!(
        index.len() > 50000,
        "Should have >50k packages, got {}",
        index.len()
    );

    // Verify common packages exist
    assert!(index.contains("apt", Architecture::Amd64));
    assert!(index.contains("dpkg", Architecture::Amd64));

    println!(
        "DebianRepository successfully fetched and parsed {} packages",
        index.len()
    );
}

/// Test the DebianRepository can fetch the Release file.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_debian_repository_fetch_release() {
    use paranoid::config::RepositoryConfig;
    use paranoid::http::Client;
    use paranoid::repository::debian::DebianRepository;

    let client = Client::new().expect("Should create client");
    let config = RepositoryConfig {
        url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
        distribution: "bookworm".to_string(),
        components: vec!["main".to_string()],
    };
    let repo = DebianRepository::new(&client, "debian", &config);

    let release_content = repo
        .fetch_release()
        .await
        .expect("Should fetch Release file");

    // Release file should contain expected fields
    assert!(
        release_content.contains("Origin: Debian"),
        "Should contain Origin field"
    );
    assert!(
        release_content.contains("Codename: bookworm"),
        "Should contain Codename field"
    );
    assert!(
        release_content.contains("Components:"),
        "Should contain Components field"
    );

    println!("Successfully fetched Release file ({} bytes)", release_content.len());
}

/// Test the full flow from manifest to lock file.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_full_resolve_to_lockfile() {
    use paranoid::config::Manifest;
    use paranoid::lock::LockFile;
    use paranoid::resolve::ResolveService;

    let config = r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "ca-certificates"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create service");

    // Resolve
    let resolved = service.resolve(&manifest).await.expect("Should resolve");

    // Create lock file
    let lock = LockFile::from_resolved(&manifest, resolved);

    // Verify lock file
    assert!(!lock.packages.is_empty());
    assert!(lock.matches_config(&manifest));

    // Serialize and parse
    let serialized = lock.serialize().expect("Should serialize");
    println!("Lock file:\n{}", serialized);

    let parsed = LockFile::from_str(&serialized).expect("Should parse");
    assert_eq!(parsed.packages.len(), lock.packages.len());

    // Verify content hash
    let hash = lock.content_hash().unwrap();
    println!("Lock file hash: {}", hash);
    assert!(hash.starts_with("sha256:"));
}

/// Test the resolve service can resolve packages from a manifest.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_resolve_service() {
    use paranoid::config::Manifest;
    use paranoid::resolve::ResolveService;

    let config = r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "ca-certificates"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create service");

    let result = service.resolve(&manifest).await.expect("Should resolve");

    // Should have resolved ca-certificates and its dependencies
    assert!(!result.packages.is_empty(), "Should have resolved packages");

    // Find ca-certificates in the result
    let ca_certs = result
        .packages
        .iter()
        .find(|p| p.name == "ca-certificates");
    assert!(ca_certs.is_some(), "Should have resolved ca-certificates");

    let ca_certs = ca_certs.unwrap();
    assert!(!ca_certs.sha256.is_empty(), "Should have SHA256 checksum");
    assert!(!ca_certs.filename.is_empty(), "Should have filename");

    println!(
        "Resolved {} packages (including {} unmet)",
        result.packages.len(),
        result.unmet.len()
    );

    for pkg in &result.packages {
        println!("  {} {} ({})", pkg.name, pkg.version, pkg.architecture);
    }

    if !result.unmet.is_empty() {
        println!("Unmet dependencies:");
        for unmet in &result.unmet {
            println!("  {}", unmet);
        }
    }
}

/// Test downloading packages from a real Debian mirror.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_download_packages() {
    use paranoid::config::Manifest;
    use paranoid::download::{DownloadConfig, DownloadService, PackageCache, StderrProgress};
    use paranoid::lock::LockFile;
    use paranoid::resolve::ResolveService;
    use tempfile::TempDir;

    let config = r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "ca-certificates"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create resolve service");
    let resolved = service.resolve(&manifest).await.expect("Should resolve");
    let lock = LockFile::from_resolved(&manifest, resolved);

    // Use a temporary cache directory
    let temp_dir = TempDir::new().expect("Should create temp dir");
    let cache = PackageCache::with_base_dir(temp_dir.path().to_path_buf())
        .expect("Should create cache");
    let download_config = DownloadConfig::new()
        .with_max_concurrent(4)
        .with_force_download(false);
    let download_service = DownloadService::with_cache(cache, download_config)
        .expect("Should create download service");

    let result = download_service
        .download_all(&lock, &manifest.repositories, &StderrProgress)
        .await;

    assert!(
        result.failed.is_empty(),
        "All downloads should succeed: {:?}",
        result.failed.iter().map(|f| &f.name).collect::<Vec<_>>()
    );

    for pkg in &result.succeeded {
        assert!(pkg.path.exists(), "Downloaded file should exist: {:?}", pkg.path);
        println!(
            "  {} {} -> {} (cached: {})",
            pkg.name,
            pkg.version,
            pkg.path.display(),
            pkg.from_cache
        );
    }
}

/// Test that cache hits work correctly (second download should use cache).
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_download_cache_hit() {
    use paranoid::config::Manifest;
    use paranoid::download::{DownloadConfig, DownloadService, NoProgress, PackageCache};
    use paranoid::lock::LockFile;
    use paranoid::resolve::ResolveService;
    use tempfile::TempDir;

    let config = r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "base-files"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create resolve service");
    let resolved = service.resolve(&manifest).await.expect("Should resolve");
    let lock = LockFile::from_resolved(&manifest, resolved);

    // Use a temporary cache directory
    let temp_dir = TempDir::new().expect("Should create temp dir");
    let cache = PackageCache::with_base_dir(temp_dir.path().to_path_buf())
        .expect("Should create cache");
    let download_config = DownloadConfig::new()
        .with_max_concurrent(4)
        .with_force_download(false);
    let download_service = DownloadService::with_cache(cache, download_config)
        .expect("Should create download service");

    // First download - should fetch from network
    let result1 = download_service
        .download_all(&lock, &manifest.repositories, &NoProgress)
        .await;

    assert!(result1.failed.is_empty(), "First download should succeed");
    let first_download = result1.succeeded.iter().find(|p| p.name == "base-files");
    assert!(first_download.is_some(), "Should have downloaded base-files");
    assert!(
        !first_download.unwrap().from_cache,
        "First download should NOT be from cache"
    );

    // Second download - should use cache
    let result2 = download_service
        .download_all(&lock, &manifest.repositories, &NoProgress)
        .await;

    assert!(result2.failed.is_empty(), "Second download should succeed");
    let cached_download = result2.succeeded.iter().find(|p| p.name == "base-files");
    assert!(cached_download.is_some(), "Should have base-files");
    assert!(
        cached_download.unwrap().from_cache,
        "Second download SHOULD be from cache"
    );

    println!("Cache hit test passed!");
}
