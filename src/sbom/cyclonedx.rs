//! CycloneDX 1.6, as JSON.

use chrono::SecondsFormat;
use serde_json::{Value, json};

use super::Sbom;

pub(super) fn document(sbom: &Sbom<'_>) -> Value {
    let image_ref = format!("image:{}", sbom.image.id);
    let mut image = json!({
        "type": "container",
        "bom-ref": image_ref,
        "name": sbom.image.name,
    });
    if let Some(digest) = &sbom.image.digest {
        image["version"] = json!(digest.to_string());
    }
    if let Some(purl) = &sbom.image.purl {
        image["purl"] = json!(purl);
    }

    let mut components = Vec::new();
    if let Some((name, version)) = sbom.distro.os() {
        components.push(json!({
            "type": "operating-system",
            "bom-ref": format!("os:{name}-{version}"),
            "name": name,
            "version": version,
        }));
    }
    for package in &sbom.packages {
        let locked = package.locked;
        // Grype reads a package's source from Syft's properties, not from
        // the package URL's `upstream`, and matches Debian's advisories,
        // filed by source package, only with them.
        let (source, source_version) = locked.source();
        let mut properties = vec![
            json!({"name": "paranoid:repository", "value": locked.repository}),
            json!({"name": "syft:package:type", "value": "deb"}),
            json!({"name": "syft:package:metadataType", "value": "dpkg-db-entry"}),
            json!({"name": "syft:metadata:source", "value": source}),
        ];
        if source_version != locked.version {
            properties
                .push(json!({"name": "syft:metadata:sourceVersion", "value": source_version}));
        }
        components.push(json!({
            "type": "library",
            "bom-ref": package.purl,
            "name": locked.name,
            "version": locked.version,
            "purl": package.purl,
            "hashes": [{"alg": "SHA-256", "content": locked.sha256.to_string()}],
            "externalReferences": [{"type": "distribution", "url": package.download}],
            "properties": properties,
        }));
    }
    for file in &sbom.files {
        let locked = file.locked;
        let mut component = json!({
            "type": "file",
            "bom-ref": file.purl,
            "name": locked.name,
            "purl": file.purl,
            "hashes": [{"alg": "SHA-256", "content": locked.sha256.to_string()}],
            "externalReferences": [{"type": "distribution", "url": locked.url.as_str()}],
        });
        if let Some(version) = &locked.version {
            component["version"] = json!(version);
        }
        components.push(component);
    }

    // The image depends on what the manifest asks for, and each package on
    // what the lock says it pulled in.
    let reference = |name: &str| {
        sbom.packages
            .iter()
            .find(|p| p.locked.name == name)
            .map(|p| p.purl.clone())
    };
    let mut dependencies = vec![json!({
        "ref": image_ref,
        "dependsOn": sbom.direct.iter().filter_map(|d| reference(d)).collect::<Vec<_>>(),
    })];
    for edge in sbom.deps {
        let Some(parent) = reference(&edge.parent) else {
            continue;
        };
        let children: Vec<String> = edge.children.iter().filter_map(|c| reference(c)).collect();
        dependencies.push(json!({"ref": parent, "dependsOn": children}));
    }

    // Where the packages came from: each repository's signed snapshot.
    let mut properties = Vec::new();
    for repository in sbom.repositories {
        let name = &repository.name;
        properties.push(json!({
            "name": format!("paranoid:repository:{name}:date"),
            "value": repository.date.to_rfc3339_opts(SecondsFormat::Secs, true),
        }));
        properties.push(json!({
            "name": format!("paranoid:repository:{name}:inrelease"),
            "value": format!("sha256:{}", repository.inrelease),
        }));
    }

    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": format!("urn:uuid:{}", sbom.id),
        "version": 1,
        "metadata": {
            "timestamp": sbom.created.to_rfc3339_opts(SecondsFormat::Secs, true),
            "tools": {
                "components": [{
                    "type": "application",
                    "name": "paranoid",
                    "version": env!("CARGO_PKG_VERSION"),
                }],
            },
            "component": image,
            "properties": properties,
        },
        "components": components,
        "dependencies": dependencies,
    })
}
