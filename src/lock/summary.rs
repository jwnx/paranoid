//! What a lock file holds, for scripts.

use chrono::SecondsFormat;
use serde::Serialize;

use crate::hash::Sha256;
use crate::oci::Digest;

use super::{LockFile, LockedImage};

/// What a lock file holds, image by image.
///
/// It serializes to the JSON that `paranoid inspect lock --json` prints,
/// which paranoid keeps stable whatever happens to the lock's own layout:
/// fields may be added, but none removed or changed.
#[derive(Debug, Clone)]
pub struct LockSummary<'a> {
    lock: &'a LockFile,
    images: Vec<&'a LockedImage>,
}

impl<'a> LockSummary<'a> {
    /// `lock`'s images, or only `image` if given.
    pub fn new(lock: &'a LockFile, image: Option<&'a LockedImage>) -> Self {
        Self {
            lock,
            images: match image {
                Some(image) => vec![image],
                None => lock.images.iter().collect(),
            },
        }
    }
}

impl Serialize for LockSummary<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Json<'a> {
            images: Vec<Image<'a>>,
            repositories: Vec<Repository<'a>>,
        }
        #[derive(Serialize)]
        struct Image<'a> {
            name: &'a str,
            expected: Option<Expected<'a>>,
            direct: &'a [String],
            packages: Vec<Package<'a>>,
            files: Vec<File<'a>>,
            unmet: Vec<Unmet<'a>>,
        }
        #[derive(Serialize)]
        struct Expected<'a> {
            digest: &'a Digest,
            format: Option<u32>,
            paranoid: &'a str,
            epoch: i64,
        }
        #[derive(Serialize)]
        struct Package<'a> {
            name: &'a str,
            version: &'a str,
            architecture: &'a str,
            repository: &'a str,
            sha256: &'a Sha256,
            size: u64,
        }
        #[derive(Serialize)]
        struct File<'a> {
            name: &'a str,
            url: &'a str,
            sha256: &'a Sha256,
            size: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            version: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Unmet<'a> {
            dependency: &'a str,
            required_by: &'a str,
        }
        #[derive(Serialize)]
        struct Repository<'a> {
            name: &'a str,
            date: String,
            inrelease: &'a Sha256,
        }

        let lock = self.lock;
        Json {
            images: self
                .images
                .iter()
                .map(|image| Image {
                    name: &image.name,
                    expected: image.expected.as_ref().map(|e| Expected {
                        digest: &e.digest,
                        format: e.format,
                        paranoid: &e.paranoid,
                        epoch: e.epoch,
                    }),
                    direct: &image.direct,
                    packages: lock
                        .image_packages(image)
                        .into_iter()
                        .map(|p| Package {
                            name: &p.name,
                            version: &p.version,
                            architecture: p.architecture.as_str(),
                            repository: &p.repository,
                            sha256: &p.sha256,
                            size: p.size,
                        })
                        .collect(),
                    files: lock
                        .image_files(image)
                        .into_iter()
                        .map(|f| File {
                            name: &f.name,
                            url: f.url.as_str(),
                            sha256: &f.sha256,
                            size: f.size,
                            version: f.version.as_deref(),
                        })
                        .collect(),
                    unmet: image
                        .unmet
                        .iter()
                        .map(|u| Unmet {
                            dependency: &u.dependency,
                            required_by: &u.required_by,
                        })
                        .collect(),
                })
                .collect(),
            repositories: lock
                .repositories
                .iter()
                .map(|r| Repository {
                    name: &r.name,
                    date: r.date.to_rfc3339_opts(SecondsFormat::Secs, true),
                    inrelease: &r.inrelease,
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const HASH_1: &str = "a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3";
    const HASH_2: &str = "b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0";

    fn lock() -> LockFile {
        LockFile::from_str(&format!(
            r#"
config_hash = "sha256:aaaa"

[[repository]]
name = "debian"
date = "2026-07-11T10:16:37Z"
inrelease = "{HASH_2}"
signed_by = []

[[package]]
name = "bash"
version = "5.2"
architecture = "arm64"
sha256 = "{HASH_1}"
filename = "pool/main/b/bash/bash_5.2_arm64.deb"
size = 1000
repository = "debian"

[[package]]
name = "libc6"
version = "2.36"
architecture = "arm64"
sha256 = "{HASH_2}"
filename = "pool/main/g/glibc/libc6_2.36_arm64.deb"
size = 2000
repository = "debian"

[[file]]
name = "etcd"
url = "https://example.com/etcd-v3.6.16.tar.gz"
sha256 = "{HASH_1}"
size = 300
trust = "pinned"
version = "v3.6.16"

[[image]]
name = "server"
packages = ["bash", "libc6"]
direct = ["bash"]
files = ["etcd"]

[image.expected]
digest = "sha256:{HASH_1}"
format = 1
paranoid = "0.7.1"
epoch = 0
inputs = "sha256:{HASH_2}"

[[image]]
name = "builder"
packages = ["libc6"]
direct = ["libc6"]
"#
        ))
        .unwrap()
    }

    #[test]
    fn summary_serializes_to_the_documented_json() {
        let lock = lock();
        let libc6 = serde_json::json!({
            "name": "libc6", "version": "2.36", "architecture": "arm64",
            "repository": "debian", "sha256": HASH_2, "size": 2000,
        });
        let builder = serde_json::json!({
            "name": "builder",
            "expected": null,
            "direct": ["libc6"],
            "packages": [libc6],
            "files": [],
            "unmet": [],
        });
        let repositories = serde_json::json!([
            {"name": "debian", "date": "2026-07-11T10:16:37Z", "inrelease": HASH_2},
        ]);

        assert_eq!(
            serde_json::to_value(LockSummary::new(&lock, None)).unwrap(),
            serde_json::json!({
                "images": [
                    {
                        "name": "server",
                        "expected": {
                            "digest": format!("sha256:{HASH_1}"),
                            "format": 1,
                            "paranoid": "0.7.1",
                            "epoch": 0,
                        },
                        "direct": ["bash"],
                        "packages": [
                            {"name": "bash", "version": "5.2", "architecture": "arm64",
                             "repository": "debian", "sha256": HASH_1, "size": 1000},
                            libc6,
                        ],
                        "files": [
                            {"name": "etcd", "url": "https://example.com/etcd-v3.6.16.tar.gz",
                             "sha256": HASH_1, "size": 300, "version": "v3.6.16"},
                        ],
                        "unmet": [],
                    },
                    builder,
                ],
                "repositories": repositories,
            })
        );

        // One image of a set.
        let one = LockSummary::new(&lock, lock.image("builder"));
        assert_eq!(
            serde_json::to_value(one).unwrap(),
            serde_json::json!({"images": [builder], "repositories": repositories})
        );
    }
}
