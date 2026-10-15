//! Integration tests for the `paranoid` CLI binary.
//!
//! These tests invoke the compiled binary via `std::process::Command` and
//! verify exit codes, stdout/stderr content, and filesystem side effects.
//!
//! Tests that require network access (real Debian mirror) are marked
//! `#[ignore]` and must be run explicitly with `cargo test -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Path to the compiled test binary.
fn paranoid_bin() -> PathBuf {
    // `cargo test` sets CARGO_BIN_EXE_paranoid for us.
    // Fall back to a reasonable default for IDE runs.
    option_env!("CARGO_BIN_EXE_paranoid")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/paranoid"))
}

fn paranoid(args: &[&str]) -> Output {
    Command::new(paranoid_bin())
        .args(args)
        .output()
        .expect("failed to run paranoid binary")
}

fn paranoid_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(paranoid_bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("failed to run paranoid binary")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Path to the hello-world example directory (manifest + pre-generated lock).
fn hello_world_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/hello-world")
}

/// Create a minimal valid manifest in a temp directory.
fn write_minimal_manifest(dir: &Path) {
    fs::write(
        dir.join("paranoid.toml"),
        r#"
[image]
architecture = "amd64"

[repositories.debian]
url = "https://deb.debian.org/debian"
distribution = "bookworm"
components = ["main"]

[[packages]]
name = "ca-certificates"
"#,
    )
    .unwrap();
}

/// Copy the hello-world lock file into a directory.
fn copy_hello_world_lock(dest_dir: &Path) {
    fs::copy(
        hello_world_dir().join("paranoid.lock"),
        dest_dir.join("paranoid.lock"),
    )
    .unwrap();
}

// ── paranoid --help ───────────────────────────────────────────────────────────

#[test]
fn help_exits_zero() {
    let out = paranoid(&["--help"]);
    assert!(out.status.success(), "exit code: {}", out.status);
    assert!(stdout(&out).contains("paranoid"));
}

#[test]
fn version_exits_zero() {
    let out = paranoid(&["--version"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("paranoid"));
}

// ── paranoid lock ─────────────────────────────────────────────────────────────

#[test]
fn lock_fails_on_missing_manifest() {
    let tmp = TempDir::new().unwrap();
    // No manifest in tmp — should fail with a clear error.
    let out = paranoid_in(tmp.path(), &["lock"]);
    assert!(!out.status.success(), "should have failed");
    let err = stderr(&out);
    assert!(
        err.contains("could not load manifest") || err.contains("no config file"),
        "unexpected error: {err}"
    );
}

#[test]
fn lock_fails_on_invalid_manifest() {
    let tmp = TempDir::new().unwrap();
    fs::write(
        tmp.path().join("paranoid.toml"),
        "this is not valid toml ][",
    )
    .unwrap();
    let out = paranoid_in(tmp.path(), &["lock"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("could not load manifest"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn lock_writes_next_to_manifest_when_f_is_dir() {
    // The fix: when -f points to a directory, paranoid.lock lands beside the
    // manifest, not in the current working directory.
    let tmp = TempDir::new().unwrap();
    let manifest_dir = tmp.path().join("myapp");
    fs::create_dir(&manifest_dir).unwrap();
    write_minimal_manifest(&manifest_dir);

    // Run from a *different* directory so we can tell where the lock landed.
    let cwd = TempDir::new().unwrap();
    let out = Command::new(paranoid_bin())
        .current_dir(cwd.path())
        .args(["lock", "-f", manifest_dir.to_str().unwrap()])
        .output()
        .unwrap();

    // This test requires network — skip if resolution fails.
    if !out.status.success() {
        let err = stderr(&out);
        if err.contains("dependency resolution failed") || err.contains("client error") {
            eprintln!("skipping lock_writes_next_to_manifest_when_f_is_dir: no network");
            return;
        }
        panic!("unexpected failure: {err}");
    }

    // Lock must be beside the manifest, NOT in cwd.
    assert!(
        manifest_dir.join("paranoid.lock").exists(),
        "lock should be in manifest dir"
    );
    assert!(
        !cwd.path().join("paranoid.lock").exists(),
        "lock must NOT be in cwd"
    );
}

#[test]
fn lock_writes_next_to_manifest_when_f_is_file() {
    // When -f points to a file, paranoid.lock should land in the same directory
    // as the file, not in the current working directory.
    let tmp = TempDir::new().unwrap();
    let manifest_dir = tmp.path().join("myapp");
    fs::create_dir(&manifest_dir).unwrap();
    write_minimal_manifest(&manifest_dir);
    let manifest_file = manifest_dir.join("paranoid.toml");

    let cwd = TempDir::new().unwrap();
    let out = Command::new(paranoid_bin())
        .current_dir(cwd.path())
        .args(["lock", "-f", manifest_file.to_str().unwrap()])
        .output()
        .unwrap();

    if !out.status.success() {
        let err = stderr(&out);
        if err.contains("dependency resolution failed") || err.contains("client error") {
            eprintln!("skipping lock_writes_next_to_manifest_when_f_is_file: no network");
            return;
        }
        panic!("unexpected failure: {err}");
    }

    // Lock must be beside the manifest file, NOT in cwd.
    assert!(
        manifest_dir.join("paranoid.lock").exists(),
        "lock should be next to the manifest file"
    );
    assert!(
        !cwd.path().join("paranoid.lock").exists(),
        "lock must NOT be in cwd"
    );
}

#[test]
fn lock_explicit_output_path_is_respected() {
    let tmp = TempDir::new().unwrap();
    write_minimal_manifest(tmp.path());
    let custom_lock = tmp.path().join("custom.lock");

    let out = Command::new(paranoid_bin())
        .current_dir(tmp.path())
        .args(["lock", "-o", custom_lock.to_str().unwrap()])
        .output()
        .unwrap();

    if !out.status.success() {
        let err = stderr(&out);
        if err.contains("dependency resolution failed") || err.contains("client error") {
            eprintln!("skipping lock_explicit_output_path: no network");
            return;
        }
        panic!("unexpected failure: {err}");
    }

    assert!(custom_lock.exists(), "lock should be at custom path");
    assert!(
        !tmp.path().join("paranoid.lock").exists(),
        "default paranoid.lock must not exist"
    );
}

#[test]
fn lock_up_to_date_skips_resolve() {
    // Copy manifest + pre-generated lock into a temp dir.
    // Running `lock` again without --force must report "up to date".
    let tmp = TempDir::new().unwrap();
    fs::copy(
        hello_world_dir().join("paranoid.toml"),
        tmp.path().join("paranoid.toml"),
    )
    .unwrap();
    copy_hello_world_lock(tmp.path());

    let out = paranoid_in(tmp.path(), &["lock"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).to_lowercase().contains("up to date"),
        "should report up to date, got: {}",
        stderr(&out)
    );
}

#[test]
fn lock_force_re_resolves_even_when_up_to_date() {
    // With --force the tool must attempt resolution (which needs the network).
    // We just verify it does NOT print "up to date".
    let tmp = TempDir::new().unwrap();
    fs::copy(
        hello_world_dir().join("paranoid.toml"),
        tmp.path().join("paranoid.toml"),
    )
    .unwrap();
    copy_hello_world_lock(tmp.path());

    let out = paranoid_in(tmp.path(), &["lock", "--force"]);

    // If resolution succeeded, "up to date" must be absent.
    // If network is unavailable, the command fails — that's acceptable.
    if out.status.success() {
        assert!(
            !stderr(&out).contains("up to date"),
            "--force should not print 'up to date'"
        );
    }
}

// ── paranoid build ────────────────────────────────────────────────────────────

#[test]
fn build_fails_when_lock_missing() {
    let tmp = TempDir::new().unwrap();
    write_minimal_manifest(tmp.path());
    // No lock file — build must fail immediately with a clear message.
    let out = paranoid_in(
        tmp.path(),
        &["build", "-o", "/tmp/paranoid-test-should-not-exist.tar"],
    );
    assert!(!out.status.success(), "should have failed");
    assert!(
        stderr(&out).contains("lock file") && stderr(&out).contains("not found"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn build_lock_default_path_alongside_manifest() {
    // When -f points to a dir that has a lock file, --lock should default to
    // that directory's paranoid.lock (not cwd/paranoid.lock).
    // We provoke the "lock not found" error from a *different* cwd to confirm
    // the binary looks in the manifest dir, not the working dir.
    let tmp = TempDir::new().unwrap();
    write_minimal_manifest(tmp.path());
    // Do NOT create a lock file in tmp — we just want to see which path the
    // error message mentions.
    let manifest_dir = tmp.path();

    let cwd = TempDir::new().unwrap();
    let out = Command::new(paranoid_bin())
        .current_dir(cwd.path())
        .args([
            "build",
            "-f",
            manifest_dir.to_str().unwrap(),
            "-o",
            "/tmp/paranoid-test-unused.tar",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let err = stderr(&out);
    // The error must reference a path inside manifest_dir, not cwd.
    assert!(
        err.contains(manifest_dir.to_str().unwrap()),
        "error should mention manifest dir; stderr: {err}"
    );
}

#[test]
fn build_warns_on_stale_lock() {
    // Use a lock file with a wrong config_hash so the stale-lock warning fires.
    let tmp = TempDir::new().unwrap();
    write_minimal_manifest(tmp.path());
    fs::write(
        tmp.path().join("paranoid.lock"),
        r#"config_hash = "sha256:0000000000000000"

[[package]]
name = "ca-certificates"
version = "20230311+deb12u1"
architecture = "all"
sha256 = "0d5f444f594e48c1e16a41d8fc628a09b24c658916a1274025c2330f2a802bed"
filename = "pool/main/c/ca-certificates/ca-certificates_20230311+deb12u1_all.deb"
size = 155260
repository = "debian"
"#,
    )
    .unwrap();

    // Build will fail (no network / packages won't download), but we only care
    // that the stale warning was printed before any download attempt.
    let out = paranoid_in(tmp.path(), &["build", "-o", "/tmp/paranoid-test-stale.tar"]);
    assert!(
        stderr(&out).contains("stale") || stderr(&out).contains("paranoid lock"),
        "expected stale warning; stderr: {}",
        stderr(&out)
    );
}

// ── paranoid inspect lock ─────────────────────────────────────────────────────

#[test]
fn inspect_lock_shows_package_table() {
    let out = paranoid(&[
        "inspect",
        "lock",
        hello_world_dir().join("paranoid.lock").to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let out_str = stdout(&out);
    // Header
    assert!(out_str.contains("Package"), "missing header: {out_str}");
    assert!(out_str.contains("Version"), "missing header: {out_str}");
    // Known packages
    assert!(
        out_str.contains("ca-certificates"),
        "missing package: {out_str}"
    );
    assert!(out_str.contains("libc6"), "missing package: {out_str}");
}

#[test]
fn inspect_lock_shows_package_count_and_size() {
    let out = paranoid(&[
        "inspect",
        "lock",
        hello_world_dir().join("paranoid.lock").to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let out_str = stdout(&out);
    assert!(out_str.contains("7"), "expected 7 packages: {out_str}");
    assert!(out_str.contains("MB"), "expected size in MB: {out_str}");
}

#[test]
fn inspect_lock_fails_on_missing_file() {
    let out = paranoid(&["inspect", "lock", "/nonexistent/path/paranoid.lock"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("lock file error"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn inspect_lock_fails_on_malformed_file() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("bad.lock"), "this is not toml ][").unwrap();
    let out = paranoid(&[
        "inspect",
        "lock",
        tmp.path().join("bad.lock").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("lock file error"),
        "stderr: {}",
        stderr(&out)
    );
}

// ── network-dependent tests (ignored by default) ──────────────────────────────

/// Full end-to-end: lock → build → verify OCI tarball structure.
#[test]
#[ignore]
fn e2e_lock_build_produces_valid_oci_tarball() {
    let tmp = TempDir::new().unwrap();
    fs::copy(
        hello_world_dir().join("paranoid.toml"),
        tmp.path().join("paranoid.toml"),
    )
    .unwrap();

    // Step 1: lock
    let lock_out = paranoid_in(tmp.path(), &["lock"]);
    assert!(
        lock_out.status.success(),
        "lock failed: {}",
        stderr(&lock_out)
    );
    assert!(tmp.path().join("paranoid.lock").exists());

    // Step 2: build
    let tar_path = tmp.path().join("image.tar");
    let build_out = paranoid_in(tmp.path(), &["build", "-o", tar_path.to_str().unwrap()]);
    assert!(
        build_out.status.success(),
        "build failed: {}",
        stderr(&build_out)
    );
    assert!(tar_path.exists(), "OCI tarball must exist");

    // Step 3: verify OCI structure inside the tarball
    let tar_bytes = fs::read(&tar_path).unwrap();
    let mut archive = tar::Archive::new(std::io::Cursor::new(&tar_bytes));
    let entry_names: Vec<String> = archive
        .entries()
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(
        entry_names.iter().any(|n| n == "oci-layout"),
        "missing oci-layout"
    );
    assert!(
        entry_names.iter().any(|n| n == "index.json"),
        "missing index.json"
    );
    assert!(
        entry_names.iter().any(|n| n.starts_with("blobs/sha256/")),
        "missing blobs"
    );
}

/// Verify that two builds from the same lock file produce bit-identical
/// tarballs, even from separate, empty caches.
#[test]
#[ignore]
fn e2e_build_is_reproducible() {
    let tmp = TempDir::new().unwrap();
    // Lock fresh: committed locks go stale as Debian rotates security updates
    // off the mirrors.
    fs::copy(
        hello_world_dir().join("paranoid.toml"),
        tmp.path().join("paranoid.toml"),
    )
    .unwrap();
    let lock_out = paranoid_in(tmp.path(), &["lock"]);
    assert!(
        lock_out.status.success(),
        "lock failed: {}",
        stderr(&lock_out)
    );

    let build = |n: u32| {
        let tar = tmp.path().join(format!("image{n}.tar"));
        let cache = tmp.path().join(format!("cache{n}"));
        let out = paranoid_in(
            tmp.path(),
            &[
                "build",
                "-o",
                tar.to_str().unwrap(),
                "--cache-dir",
                cache.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "build {n} failed: {}", stderr(&out));
        fs::read(&tar).unwrap()
    };

    assert!(build(1) == build(2), "builds must be bit-identical");
}
