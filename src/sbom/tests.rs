use std::collections::HashSet;
use std::str::FromStr;

use serde_json::Value;

use super::*;
use crate::lock::ExpectedDigest;

const HASH_1: &str = "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3";
const HASH_2: &str = "b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0";

fn manifest(distribution: &str) -> Manifest {
    Manifest::from_str(&format!(
        r#"
[image]
architecture = "arm64"
name = "fly/pg"
tag = "16"

[repositories.debian]
url = "https://snapshot.debian.org/archive/debian"
snapshot = "20261005T000000Z"
distribution = "{distribution}"
components = ["main"]
signed_by = "keys/debian.asc"

[files.etcd]
url = "https://example.com/etcd-v3.6.16.tar.gz"
sha256 = "{HASH_1}"
path = "/usr/local/bin/etcd"

[[packages]]
name = "libstdc++6"
"#
    ))
    .unwrap()
}

fn lock() -> LockFile {
    LockFile::from_str(&format!(
        r#"
config_hash = "sha256:aaaa"

[[repository]]
name = "debian"
date = "2026-10-04T20:11:02Z"
inrelease = "{HASH_2}"
signed_by = []

[[package]]
name = "libssl3"
version = "3.0.16-1~deb12u1"
architecture = "arm64"
sha256 = "{HASH_1}"
filename = "pool/main/o/openssl/libssl3_3.0.16-1~deb12u1_arm64.deb"
size = 2000
repository = "debian"
source = "openssl"

[[package]]
name = "libstdc++6"
version = "12.2.0-14+b1"
architecture = "arm64"
sha256 = "{HASH_2}"
filename = "pool/main/g/gcc-12/libstdc++6_12.2.0-14+b1_arm64.deb"
size = 1000
repository = "debian"
source = "gcc-12 (12.2.0-14)"

[[file]]
name = "etcd"
url = "https://example.com/etcd-v3.6.16.tar.gz"
sha256 = "{HASH_1}"
size = 300
trust = "pinned"
version = "v3.6.16"

[[image]]
name = "default"
packages = ["libssl3", "libstdc++6"]
direct = ["libstdc++6"]
files = ["etcd"]

[[image.dep]]
parent = "libstdc++6"
children = ["libssl3"]
"#
    ))
    .unwrap()
}

/// The lock, recording `digest` for the image as it is now.
fn recorded(manifest: &Manifest, digest: &str) -> LockFile {
    let mut lock = lock();
    let inputs = build::inputs(manifest, &lock, "default").unwrap();
    lock.images[0].expected = Some(ExpectedDigest {
        digest: format!("sha256:{digest}").parse().unwrap(),
        format: Some(IMAGE_FORMAT),
        paranoid: "0.8.0".to_string(),
        epoch: 0,
        inputs,
    });
    lock
}

fn json(manifest: &Manifest, lock: &LockFile, format: Format) -> Value {
    let sbom = Sbom::new(manifest, lock, "default").unwrap();
    serde_json::from_str(&sbom.to_json(format)).unwrap()
}

#[test]
fn names_packages_with_the_urls_scanners_match() {
    let manifest = manifest("bookworm");
    let lock = lock();
    let sbom = Sbom::new(&manifest, &lock, "default").unwrap();
    let purls: Vec<&str> = sbom.packages.iter().map(|p| p.purl.as_str()).collect();
    assert_eq!(
        purls,
        [
            "pkg:deb/debian/libssl3@3.0.16-1~deb12u1?arch=arm64&distro=debian-12&upstream=openssl",
            "pkg:deb/debian/libstdc%2B%2B6@12.2.0-14%2Bb1?arch=arm64&distro=debian-12\
             &upstream=gcc-12%4012.2.0-14",
        ]
    );
    assert_eq!(
        sbom.packages[0].download,
        "https://snapshot.debian.org/archive/debian/20261005T000000Z/\
         pool/main/o/openssl/libssl3_3.0.16-1~deb12u1_arm64.deb"
    );
    assert_eq!(
        sbom.files[0].purl,
        format!(
            "pkg:generic/etcd@v3.6.16?checksum=sha256%3A{HASH_1}\
             &download_url=https%3A//example.com/etcd-v3.6.16.tar.gz"
        )
    );
}

#[test]
fn takes_the_distribution_from_the_repositories() {
    let distro = |distribution: &str| Distro::of(&manifest(distribution));
    assert_eq!(
        distro("bookworm-security"),
        Distro {
            namespace: "debian",
            qualifier: Some("debian-12"),
        }
    );
    assert_eq!(distro("noble").qualifier, Some("ubuntu-24.04"));
    assert_eq!(
        distro("sid"),
        Distro {
            namespace: "debian",
            qualifier: None,
        }
    );
}

#[test]
fn names_the_image_by_a_recorded_digest_that_holds() {
    let manifest = manifest("bookworm");
    let lock = recorded(&manifest, HASH_2);
    let sbom = Sbom::new(&manifest, &lock, "default").unwrap();
    assert_eq!(sbom.image.name, "fly/pg:16");
    assert_eq!(
        sbom.image.purl.as_deref(),
        Some(
            format!(
                "pkg:oci/pg@sha256%3A{HASH_2}?arch=arm64\
                 &repository_url=docker.io/fly/pg&tag=16"
            )
            .as_str()
        )
    );

    // Not one recorded for other inputs, or another image format.
    let mut stale = lock.clone();
    stale.images[0].expected.as_mut().unwrap().inputs = Digest::from_bytes(b"other");
    let older = {
        let mut lock = lock.clone();
        lock.images[0].expected.as_mut().unwrap().format = None;
        lock
    };
    for unrecorded in [self::lock(), stale, older] {
        let sbom = Sbom::new(&manifest, &unrecorded, "default").unwrap();
        assert_eq!(sbom.image.digest, None);
        assert_eq!(sbom.image.purl, None);
    }
}

#[test]
fn spdx_is_consistent() {
    let manifest = manifest("bookworm");
    let lock = recorded(&manifest, HASH_2);
    let doc = json(&manifest, &lock, Format::Spdx);
    assert_eq!(doc["spdxVersion"], "SPDX-2.3");
    assert_eq!(doc["creationInfo"]["created"], "2026-10-04T20:11:02Z");

    let packages = doc["packages"].as_array().unwrap();
    let ids: HashSet<&str> = packages
        .iter()
        .map(|p| p["SPDXID"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 5, "{ids:?}");
    for id in &ids {
        assert!(
            id.strip_prefix("SPDXRef-")
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
            "{id}"
        );
    }
    assert!(ids.contains("SPDXRef-deb-libstdc--6"));
    let os = packages
        .iter()
        .find(|p| p["primaryPackagePurpose"] == "OPERATING-SYSTEM")
        .unwrap();
    assert_eq!(
        (&os["name"], &os["versionInfo"]),
        (&"debian".into(), &"12".into())
    );

    let relationships = doc["relationships"].as_array().unwrap();
    for r in relationships {
        let from = r["spdxElementId"].as_str().unwrap();
        assert!(from == "SPDXRef-DOCUMENT" || ids.contains(from), "{r}");
        assert!(
            ids.contains(r["relatedSpdxElement"].as_str().unwrap()),
            "{r}"
        );
    }
    let kinds = |kind: &str| {
        relationships
            .iter()
            .filter(|r| r["relationshipType"] == kind)
            .count()
    };
    assert_eq!((kinds("DESCRIBES"), kinds("CONTAINS")), (1, 4));
    assert_eq!(kinds("DEPENDS_ON"), 1);
}

#[test]
fn cyclonedx_is_consistent() {
    let manifest = manifest("bookworm");
    let lock = recorded(&manifest, HASH_2);
    let doc = json(&manifest, &lock, Format::CycloneDx);
    assert_eq!(doc["specVersion"], "1.6");
    assert_eq!(doc["metadata"]["component"]["type"], "container");
    assert_eq!(
        doc["metadata"]["component"]["version"],
        format!("sha256:{HASH_2}")
    );

    let components = doc["components"].as_array().unwrap();
    let refs: HashSet<&str> = components
        .iter()
        .map(|c| c["bom-ref"].as_str().unwrap())
        .chain([doc["metadata"]["component"]["bom-ref"].as_str().unwrap()])
        .collect();
    assert_eq!(refs.len(), 5);
    let os = components
        .iter()
        .find(|c| c["type"] == "operating-system")
        .unwrap();
    assert_eq!(
        (&os["name"], &os["version"]),
        (&"debian".into(), &"12".into())
    );
    for dependency in doc["dependencies"].as_array().unwrap() {
        assert!(refs.contains(dependency["ref"].as_str().unwrap()));
        for on in dependency["dependsOn"].as_array().unwrap() {
            assert!(refs.contains(on.as_str().unwrap()), "{on}");
        }
    }
}

#[test]
fn the_same_lock_gives_the_same_sbom() {
    let manifest = manifest("bookworm");
    let lock = recorded(&manifest, HASH_2);
    for format in [Format::Spdx, Format::CycloneDx] {
        let make = || {
            Sbom::new(&manifest, &lock, "default")
                .unwrap()
                .to_json(format)
        };
        assert_eq!(make(), make());
    }

    // Its id is a UUID, and another lock's is another.
    let sbom = Sbom::new(&manifest, &lock, "default").unwrap();
    let id = &sbom.id;
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "5");
    assert!("89ab".contains(&id[19..20]));
    let other = self::lock();
    assert_ne!(&Sbom::new(&manifest, &other, "default").unwrap().id, id);
}
