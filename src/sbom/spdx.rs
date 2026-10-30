//! SPDX 2.3, as JSON.

use std::collections::{HashMap, HashSet};

use chrono::SecondsFormat;
use serde_json::{Value, json};

use super::Sbom;

/// What the SPDX spec calls an unknown: here, licenses and copyrights,
/// which the lock doesn't know.
const NOASSERTION: &str = "NOASSERTION";

pub(super) fn document(sbom: &Sbom<'_>) -> Value {
    let mut ids = Ids::default();
    let image_id = ids.make("image", &sbom.image.id);

    let mut image = json!({
        "name": sbom.image.name,
        "SPDXID": image_id,
        "downloadLocation": NOASSERTION,
        "filesAnalyzed": false,
        "primaryPackagePurpose": "CONTAINER",
        "licenseConcluded": NOASSERTION,
        "licenseDeclared": NOASSERTION,
        "copyrightText": NOASSERTION,
    });
    if let Some(digest) = &sbom.image.digest {
        image["versionInfo"] = json!(digest.to_string());
    }
    if let Some(purl) = &sbom.image.purl {
        image["externalRefs"] = json!([purl_ref(purl)]);
    }

    let mut packages = vec![image];
    let mut relationships = vec![json!({
        "spdxElementId": "SPDXRef-DOCUMENT",
        "relationshipType": "DESCRIBES",
        "relatedSpdxElement": image_id,
    })];
    if let Some((name, version)) = sbom.distro.os() {
        let id = ids.make("os", &format!("{name}-{version}"));
        packages.push(json!({
            "name": name,
            "SPDXID": id,
            "versionInfo": version,
            "downloadLocation": NOASSERTION,
            "filesAnalyzed": false,
            "primaryPackagePurpose": "OPERATING-SYSTEM",
            "licenseConcluded": NOASSERTION,
            "licenseDeclared": NOASSERTION,
            "copyrightText": NOASSERTION,
        }));
        relationships.push(contains(&image_id, &id));
    }
    let mut by_name = HashMap::new();
    for package in &sbom.packages {
        let locked = package.locked;
        let id = ids.make("deb", &locked.name);
        let (source, version) = locked.source();
        packages.push(json!({
            "name": locked.name,
            "SPDXID": id,
            "versionInfo": locked.version,
            "downloadLocation": package.download,
            "filesAnalyzed": false,
            "checksums": [{"algorithm": "SHA256", "checksumValue": locked.sha256.to_string()}],
            "sourceInfo": format!("built from the {source} {version} source package"),
            "licenseConcluded": NOASSERTION,
            "licenseDeclared": NOASSERTION,
            "copyrightText": NOASSERTION,
            "externalRefs": [purl_ref(&package.purl)],
        }));
        relationships.push(contains(&image_id, &id));
        by_name.insert(locked.name.as_str(), id);
    }
    for file in &sbom.files {
        let locked = file.locked;
        let id = ids.make("file", &locked.name);
        let mut package = json!({
            "name": locked.name,
            "SPDXID": id,
            "downloadLocation": locked.url.as_str(),
            "filesAnalyzed": false,
            "checksums": [{"algorithm": "SHA256", "checksumValue": locked.sha256.to_string()}],
            "licenseConcluded": NOASSERTION,
            "licenseDeclared": NOASSERTION,
            "copyrightText": NOASSERTION,
            "externalRefs": [purl_ref(&file.purl)],
        });
        if let Some(version) = &locked.version {
            package["versionInfo"] = json!(version);
        }
        packages.push(package);
        relationships.push(contains(&image_id, &id));
    }
    for edge in sbom.deps {
        let Some(parent) = by_name.get(edge.parent.as_str()) else {
            continue;
        };
        for child in edge.children.iter().filter_map(|c| by_name.get(c.as_str())) {
            relationships.push(json!({
                "spdxElementId": parent,
                "relationshipType": "DEPENDS_ON",
                "relatedSpdxElement": child,
            }));
        }
    }

    // Where the packages came from: each repository's signed snapshot.
    let snapshots: Vec<String> = sbom
        .repositories
        .iter()
        .map(|r| {
            format!(
                "{} at {}, InRelease sha256:{}",
                r.name,
                r.date.to_rfc3339_opts(SecondsFormat::Secs, true),
                r.inrelease
            )
        })
        .collect();

    json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": sbom.image.name,
        "documentNamespace": format!("urn:uuid:{}", sbom.id),
        "creationInfo": {
            "created": sbom.created.to_rfc3339_opts(SecondsFormat::Secs, true),
            "creators": [format!("Tool: paranoid-{}", env!("CARGO_PKG_VERSION"))],
            "comment": format!("Made from a paranoid lock. Repositories: {}.", snapshots.join("; ")),
        },
        "packages": packages,
        "relationships": relationships,
    })
}

fn purl_ref(purl: &str) -> Value {
    json!({
        "referenceCategory": "PACKAGE-MANAGER",
        "referenceType": "purl",
        "referenceLocator": purl,
    })
}

fn contains(image: &str, id: &str) -> Value {
    json!({
        "spdxElementId": image,
        "relationshipType": "CONTAINS",
        "relatedSpdxElement": id,
    })
}

/// SPDX ids, which allow only letters, digits, `.` and `-`: `libstdc++6`
/// becomes `SPDXRef-deb-libstdc--6`, and a name that comes out the same as
/// an earlier one gets a number.
#[derive(Default)]
struct Ids(HashSet<String>);

impl Ids {
    fn make(&mut self, kind: &str, name: &str) -> String {
        let name: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let base = format!("SPDXRef-{kind}-{name}");
        let mut id = base.clone();
        let mut n = 2;
        while !self.0.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }
}
