//! Integration tests for Debian package parsing and resolution.
//!
//! These tests use real Debian package data to verify the parser and resolver
//! work correctly with production data.

use std::str::FromStr;

use paranoid::repository::debian::{
    Architecture, PackageIndex, PackageIndexSet, Priority, Request, ResolveError, resolve,
};

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

/// Resolve one request against the fixture, returning the selected names.
fn resolve_fixture(request: Request) -> Result<Vec<String>, ResolveError> {
    let index = make_index_set(load_fixture());
    let resolution = resolve(&index, &[request], Architecture::Amd64)?;
    Ok(resolution
        .packages
        .iter()
        .map(|r| r.package.name.clone())
        .collect())
}

#[test]
fn test_resolver_with_real_packages() {
    let bash = resolve_fixture(Request::new("bash")).expect("Should resolve bash");
    assert!(bash.contains(&"bash".to_string()));

    // Both mawk and gawk provide awk. Like apt, we won't guess which one a
    // request for "awk" meant.
    assert_eq!(
        resolve_fixture(Request::new("awk")),
        Err(ResolveError::AmbiguousVirtual {
            name: "awk".into(),
            providers: vec!["gawk".into(), "mawk".into()],
        })
    );
}

#[test]
fn test_resolver_fallback_to_arch_all() {
    // ca-certificates is only available as arch: all
    let index = make_index_set(load_fixture());
    let resolution = resolve(
        &index,
        &[Request::new("ca-certificates")],
        Architecture::Amd64,
    )
    .expect("Should resolve ca-certificates via arch:all");
    let ca_certs = resolution
        .packages
        .iter()
        .find(|r| r.package.name == "ca-certificates")
        .unwrap();
    assert_eq!(ca_certs.package.architecture, Some(Architecture::All));
}

#[test]
fn test_resolver_nonexistent_package() {
    assert!(matches!(
        resolve_fixture(Request::new("this-package-does-not-exist")),
        Err(ResolveError::NotFound { .. })
    ));
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
    assert_eq!(
        result.unwrap().len(),
        0,
        "Empty file should have 0 packages"
    );
}

#[test]
fn test_parse_packages_with_only_whitespace() {
    let whitespace = "   \n\n   \n\t\n   ";
    let result = PackageIndex::from_str(whitespace);
    assert!(
        result.is_ok(),
        "Whitespace-only file should parse successfully"
    );
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

/// The key that signs bookworm, relative to the crate root (where cargo
/// runs integration tests).
const KEY: &str = "examples/keys/debian-archive-bookworm-automatic.asc";

/// A real Debian key that doesn't sign bookworm's main archive.
const WRONG_KEY: &str =
    "src/repository/debian/testdata/debian-archive-bookworm-security-automatic.asc";

/// Test the DebianRepository can fetch and parse packages from a real mirror.
/// This test is ignored by default as it requires network access.
#[tokio::test]
#[ignore]
async fn test_debian_repository_fetch_packages() {
    use paranoid::config::RepositoryConfig;
    use paranoid::http::Client;
    use paranoid::repository::debian::{DebianRepository, Keyring};

    let client = Client::new().expect("Should create client");
    let config = RepositoryConfig {
        url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
        distribution: "bookworm".to_string(),
        components: vec!["main".to_string()],
        signed_by: KEY.into(),
        snapshot: None,
        priority: None,
    };
    let keyring = Keyring::load(KEY.as_ref()).expect("Should load keyring");
    let repo = DebianRepository::new(&client, "debian", &config, &keyring);

    // Fetch the full index
    let snapshot = repo
        .fetch_release(chrono::Utc::now())
        .await
        .expect("Should fetch a signed release");
    let index = repo
        .fetch_index(&snapshot, Architecture::Amd64)
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
    use paranoid::repository::debian::{DebianRepository, Keyring};

    let client = Client::new().expect("Should create client");
    let config = RepositoryConfig {
        url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
        distribution: "bookworm".to_string(),
        components: vec!["main".to_string()],
        signed_by: KEY.into(),
        snapshot: None,
        priority: None,
    };
    let keyring = Keyring::load(KEY.as_ref()).expect("Should load keyring");
    let repo = DebianRepository::new(&client, "debian", &config, &keyring);

    let snapshot = repo
        .fetch_release(chrono::Utc::now())
        .await
        .expect("Should fetch a signed release");

    assert_eq!(snapshot.release.codename.as_deref(), Some("bookworm"));
    assert!(snapshot.release.acquire_by_hash);
    assert!(
        snapshot.release.packages_index("main", "amd64").is_ok(),
        "Should list the main/amd64 Packages index"
    );
    assert_eq!(
        snapshot
            .signers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8"]
    );
}

/// The wrong key must stop resolution before anything is trusted.
#[tokio::test]
#[ignore]
async fn test_debian_repository_rejects_the_wrong_key() {
    use paranoid::config::RepositoryConfig;
    use paranoid::http::Client;
    use paranoid::repository::debian::{DebianRepository, DebianRepositoryError, Keyring};

    let client = Client::new().expect("Should create client");
    let config = RepositoryConfig {
        url: url::Url::parse("https://deb.debian.org/debian").unwrap(),
        distribution: "bookworm".to_string(),
        components: vec!["main".to_string()],
        signed_by: WRONG_KEY.into(),
        snapshot: None,
        priority: None,
    };
    let keyring = Keyring::load(WRONG_KEY.as_ref()).expect("Should load keyring");
    let repo = DebianRepository::new(&client, "debian", &config, &keyring);

    let err = repo.fetch_release(chrono::Utc::now()).await.unwrap_err();
    assert!(
        matches!(err, DebianRepositoryError::Signature { .. }),
        "expected a signature error, got: {err}"
    );
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
signed_by = "examples/keys/debian-archive-bookworm-automatic.asc"

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
signed_by = "examples/keys/debian-archive-bookworm-automatic.asc"

[[packages]]
name = "ca-certificates"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create service");

    let result = service.resolve(&manifest).await.expect("Should resolve");

    // Should have resolved ca-certificates and its dependencies
    assert!(!result.packages.is_empty(), "Should have resolved packages");

    // Find ca-certificates in the result
    let ca_certs = result.packages.iter().find(|p| p.name == "ca-certificates");
    assert!(ca_certs.is_some(), "Should have resolved ca-certificates");

    let ca_certs = ca_certs.unwrap();
    assert_eq!(
        ca_certs.sha256.to_string().len(),
        64,
        "Should have SHA256 checksum"
    );
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
    use paranoid::download::{DownloadConfig, DownloadService, PackageCache};
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
signed_by = "examples/keys/debian-archive-bookworm-automatic.asc"

[[packages]]
name = "ca-certificates"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create resolve service");
    let resolved = service.resolve(&manifest).await.expect("Should resolve");
    let lock = LockFile::from_resolved(&manifest, resolved);

    // Use a temporary cache directory
    let temp_dir = TempDir::new().expect("Should create temp dir");
    let cache =
        PackageCache::with_base_dir(temp_dir.path().to_path_buf()).expect("Should create cache");
    let download_config = DownloadConfig::default();
    let download_service = DownloadService::with_cache(cache, download_config)
        .expect("Should create download service");

    let fetched = download_service
        .download_all(&lock, &manifest.repositories, &())
        .await
        .expect("All downloads should succeed");

    assert_eq!(fetched.len(), lock.packages.len());
    for pkg in &fetched {
        assert!(
            pkg.path.exists(),
            "Downloaded file should exist: {:?}",
            pkg.path
        );
        println!(
            "  {} {} -> {} (cached: {})",
            pkg.package.name,
            pkg.package.version,
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
    use paranoid::download::{DownloadConfig, DownloadService, PackageCache};
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
signed_by = "examples/keys/debian-archive-bookworm-automatic.asc"

[[packages]]
name = "base-files"
"#;

    let manifest = Manifest::from_str(config).expect("Should parse manifest");
    let service = ResolveService::new().expect("Should create resolve service");
    let resolved = service.resolve(&manifest).await.expect("Should resolve");
    let lock = LockFile::from_resolved(&manifest, resolved);

    // Use a temporary cache directory
    let temp_dir = TempDir::new().expect("Should create temp dir");
    let cache =
        PackageCache::with_base_dir(temp_dir.path().to_path_buf()).expect("Should create cache");
    let download_config = DownloadConfig::default();
    let download_service = DownloadService::with_cache(cache, download_config)
        .expect("Should create download service");

    // First download - should fetch from network
    let result1 = download_service
        .download_all(&lock, &manifest.repositories, &())
        .await
        .expect("First download should succeed");

    let first_download = result1.iter().find(|p| p.package.name == "base-files");
    assert!(
        first_download.is_some(),
        "Should have downloaded base-files"
    );
    assert!(
        !first_download.unwrap().from_cache,
        "First download should NOT be from cache"
    );

    // Second download - should use cache
    let result2 = download_service
        .download_all(&lock, &manifest.repositories, &())
        .await
        .expect("Second download should succeed");

    let cached_download = result2.iter().find(|p| p.package.name == "base-files");
    assert!(cached_download.is_some(), "Should have base-files");
    assert!(
        cached_download.unwrap().from_cache,
        "Second download SHOULD be from cache"
    );

    println!("Cache hit test passed!");
}
