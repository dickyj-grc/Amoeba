//! The app library: ready-made app packages for known images, so a user can
//! deploy one without knowing its port, start command or API.
//!
//! `library/index.json` lists the apps and what finds them (names, images,
//! GitHub repos). Each points at a package folder holding an `amoeba.yaml`
//! and, for compose apps, its compose file. Everything is compiled into the
//! gateway, so the library works offline and ships with each release.

use crate::apps::schema::{AppManifest, AppSpec};
use crate::config::schema::AppInterface;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;

const INDEX: &str = include_str!("../../library/index.json");

/// A package's files: (name inside the package, contents).
type PackageFiles = &'static [(&'static str, &'static [u8])];

/// The files of each package folder named in the index.
const PACKAGES: &[(&str, PackageFiles)] = &[
    (
        "library/lightpanda-mcp",
        &[(
            "amoeba.yaml",
            include_bytes!("../../library/lightpanda-mcp/amoeba.yaml"),
        )],
    ),
    (
        "examples/app-packages/obscura",
        &[
            (
                "amoeba.yaml",
                include_bytes!("../../examples/app-packages/obscura/amoeba.yaml"),
            ),
            (
                "compose.yaml",
                include_bytes!("../../examples/app-packages/obscura/compose.yaml"),
            ),
        ],
    ),
    (
        "examples/app-packages/finance-db",
        &[(
            "amoeba.yaml",
            include_bytes!("../../examples/app-packages/finance-db/amoeba.yaml"),
        )],
    ),
    (
        "examples/app-packages/hello-world",
        &[
            (
                "amoeba.yaml",
                include_bytes!("../../examples/app-packages/hello-world/amoeba.yaml"),
            ),
            (
                "compose.yaml",
                include_bytes!("../../examples/app-packages/hello-world/compose.yaml"),
            ),
        ],
    ),
    (
        "examples/app-packages/streaming-echo",
        &[
            (
                "amoeba.yaml",
                include_bytes!("../../examples/app-packages/streaming-echo/amoeba.yaml"),
            ),
            (
                "compose.yaml",
                include_bytes!("../../examples/app-packages/streaming-echo/compose.yaml"),
            ),
        ],
    ),
];

#[derive(Debug, Deserialize)]
struct Index {
    apps: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct IndexEntry {
    id: String,
    title: String,
    summary: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    repos: Vec<String>,
    package: String,
}

/// One input the installer fills in, from the manifest's `schema`.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryField {
    pub description: String,
    /// The installer must fill it in: a required secret, or a required env
    /// var without a default.
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Env vars only: `string`, `number`, `integer` or `boolean`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

/// What the console shows for an app before deploying it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryEntry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    /// Image names that find this app, without tags.
    pub images: Vec<String>,
    /// GitHub repositories that find this app, as `github.com/<owner>/<repo>`.
    pub repos: Vec<String>,
    /// The service name it installs as unless the installer picks another.
    pub default_name: String,
    pub description: String,
    /// `image:tag` for an image app; absent for a compose app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    pub port: u16,
    pub public: bool,
    pub local_upstream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<AppInterface>,
    pub env: BTreeMap<String, LibraryField>,
    pub secrets: BTreeMap<String, LibraryField>,
}

fn index() -> Vec<IndexEntry> {
    serde_json::from_str::<Index>(INDEX)
        .expect("library/index.json is checked by tests")
        .apps
}

fn package_files(package: &str) -> Option<PackageFiles> {
    PACKAGES
        .iter()
        .find(|(name, _)| *name == package)
        .map(|(_, files)| *files)
}

fn manifest_text(package: &str) -> Option<&'static str> {
    package_files(package)?
        .iter()
        .find(|(name, _)| *name == "amoeba.yaml")
        .and_then(|(_, bytes)| std::str::from_utf8(bytes).ok())
}

fn describe(entry: IndexEntry) -> Option<LibraryEntry> {
    let manifest: AppManifest = serde_yaml::from_str(manifest_text(&entry.package)?).ok()?;
    let (image, command) = match &manifest.app {
        // Shown with ${PORT} filled in, as the app will run.
        AppSpec::Image { image, .. } => (Some(image.clone()), manifest.start_command().ok()?),
        AppSpec::Compose { .. } => (None, Vec::new()),
    };
    Some(LibraryEntry {
        id: entry.id,
        title: entry.title,
        summary: entry.summary,
        tags: entry.tags,
        images: entry.images,
        repos: entry.repos,
        default_name: manifest.name.clone(),
        description: manifest.description.clone(),
        image,
        command,
        port: manifest.placement.port,
        public: manifest.public,
        local_upstream: manifest.local_upstream,
        interface: manifest.interface.clone(),
        env: manifest
            .schema
            .env
            .iter()
            .map(|(k, f)| {
                (
                    k.clone(),
                    LibraryField {
                        description: f.description.clone(),
                        required: f.required && f.default.is_none(),
                        default: f.default.clone(),
                        r#type: Some(f.r#type.clone()),
                        options: f.options.clone(),
                    },
                )
            })
            .collect(),
        secrets: manifest
            .schema
            .secrets
            .iter()
            .map(|(k, f)| {
                (
                    k.clone(),
                    LibraryField {
                        description: f.description.clone(),
                        required: f.required,
                        default: None,
                        r#type: None,
                        options: None,
                    },
                )
            })
            .collect(),
    })
}

/// Every app in the library, in index order.
pub fn entries() -> Vec<LibraryEntry> {
    index().into_iter().filter_map(describe).collect()
}

pub fn entry(id: &str) -> Option<LibraryEntry> {
    index().into_iter().find(|e| e.id == id).and_then(describe)
}

/// The app's package as a zip, ready for `AppManager::install`. `name`
/// renames the service, and the console token role `mcp:<old>` in its
/// permissions follows it; `public` overrides the manifest's setting.
pub fn package_zip(id: &str, name: Option<&str>, public: Option<bool>) -> Result<Vec<u8>, String> {
    let entry = index()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| format!("no app '{id}' in the library"))?;
    let files = package_files(&entry.package)
        .ok_or_else(|| format!("library package '{}' is not bundled", entry.package))?;
    let text = manifest_text(&entry.package).ok_or("library package has no amoeba.yaml")?;
    let manifest = customize(text, name, public)?;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        for (file, bytes) in files {
            zip.start_file(*file, options).map_err(|e| e.to_string())?;
            let data: &[u8] = if *file == "amoeba.yaml" {
                manifest.as_bytes()
            } else {
                bytes
            };
            zip.write_all(data).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

/// Applies the installer's name and public choice to a manifest's YAML.
fn customize(text: &str, name: Option<&str>, public: Option<bool>) -> Result<String, String> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("library manifest: {e}"))?;
    let map = doc
        .as_mapping_mut()
        .ok_or("library manifest is not a mapping")?;
    let old_name = map
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        map.insert("name".into(), name.into());
        let (old_role, new_role) = (format!("mcp:{old_name}"), format!("mcp:{name}"));
        if let Some(perms) = map.get_mut("permissions").and_then(|p| p.as_mapping_mut()) {
            for roles in perms.values_mut() {
                if let Some(list) = roles.as_sequence_mut() {
                    for role in list.iter_mut() {
                        if role.as_str() == Some(old_role.as_str()) {
                            *role = new_role.clone().into();
                        }
                    }
                }
            }
        }
    }
    if let Some(public) = public {
        map.insert("public".into(), public.into());
    }
    serde_yaml::to_string(&doc).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[deploy#App library#Bundled packages]]
    #[test]
    fn every_library_app_is_bundled_and_valid() {
        let index = index();
        assert!(!index.is_empty());
        let mut ids = std::collections::HashSet::new();
        for e in &index {
            assert!(ids.insert(e.id.clone()), "duplicate id {}", e.id);
            let files = package_files(&e.package)
                .unwrap_or_else(|| panic!("{} is not in PACKAGES", e.package));
            let manifest: AppManifest =
                serde_yaml::from_str(manifest_text(&e.package).unwrap()).unwrap();
            crate::apps::manager::validate_manifest(&manifest)
                .unwrap_or_else(|err| panic!("{}: {err}", e.id));
            assert!(manifest.interface.is_some(), "{} has no interface", e.id);
            if let AppSpec::Compose { compose_file, .. } = &manifest.app {
                assert!(
                    files.iter().any(|(f, _)| f == compose_file),
                    "{} misses {compose_file}",
                    e.id
                );
            }
            if let AppSpec::Image { image, .. } = &manifest.app {
                assert!(
                    e.images.iter().any(|i| image.starts_with(i.as_str())),
                    "{}: image {image} is not listed in its images",
                    e.id
                );
            }
            // Each bundled file is the one on disk, so edits cannot drift.
            for (file, bytes) in files {
                let path = format!("{}/{}/{file}", env!("CARGO_MANIFEST_DIR"), e.package);
                assert_eq!(std::fs::read(&path).unwrap(), *bytes, "{path}");
            }
        }
        // Every package folder under library/ is listed.
        let dir = format!("{}/library", env!("CARGO_MANIFEST_DIR"));
        for folder in std::fs::read_dir(dir).unwrap().flatten() {
            if folder.path().is_dir() {
                let package = format!("library/{}", folder.file_name().to_string_lossy());
                assert!(
                    index.iter().any(|e| e.package == package),
                    "{package} not in index"
                );
            }
        }
        assert_eq!(entries().len(), index.len(), "an entry failed to describe");
    }

    /// Keys a manifest may use at `node` of the JSON Schema: its
    /// `properties`, merged across `oneOf` branches. `None` means any key
    /// (a map such as `permissions`).
    fn allowed_keys(node: &serde_json::Value) -> Option<Vec<String>> {
        let mut keys: Vec<String> = node
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        for branch in node
            .get("oneOf")
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
        {
            keys.extend(allowed_keys(branch).unwrap_or_default());
        }
        (!keys.is_empty()).then_some(keys)
    }

    fn check_against_schema(
        value: &serde_yaml::Value,
        node: &serde_json::Value,
        root: &serde_json::Value,
        at: &str,
    ) {
        let node = match node.get("$ref").and_then(|r| r.as_str()) {
            Some(r) => &root["$defs"][r.trim_start_matches("#/$defs/")],
            None => node,
        };
        let Some(map) = value.as_mapping() else {
            return;
        };
        let Some(allowed) = allowed_keys(node) else {
            // A map of names (permissions, env, secrets): check each value.
            if let Some(item) = node.get("additionalProperties").filter(|a| a.is_object()) {
                for (k, v) in map {
                    check_against_schema(
                        v,
                        item,
                        root,
                        &format!("{at}.{}", k.as_str().unwrap_or("?")),
                    );
                }
            }
            return;
        };
        for (key, child) in map {
            let key = key.as_str().unwrap_or_default();
            assert!(
                allowed.iter().any(|k| k == key),
                "{at}.{key} is used by a manifest but missing from docs/amoeba.schema.json"
            );
            let mut child_node = node.get("properties").and_then(|p| p.get(key));
            for branch in node
                .get("oneOf")
                .and_then(|b| b.as_array())
                .into_iter()
                .flatten()
            {
                child_node =
                    child_node.or_else(|| branch.get("properties").and_then(|p| p.get(key)));
            }
            if let Some(child_node) = child_node {
                check_against_schema(child, child_node, root, &format!("{at}.{key}"));
            }
        }
    }

    // @lat: [[deploy#App library#Manifest reference]]
    #[test]
    fn manifest_schema_covers_every_bundled_manifest() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../docs/amoeba.schema.json"))
                .expect("docs/amoeba.schema.json is valid JSON");
        for (package, _) in PACKAGES {
            let manifest: serde_yaml::Value =
                serde_yaml::from_str(manifest_text(package).unwrap()).unwrap();
            check_against_schema(&manifest, &schema, &schema, package);
        }
    }

    #[test]
    fn install_renames_the_service_and_its_token_role() {
        let zip = package_zip("lightpanda-mcp", Some("browser"), Some(true)).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("amoeba.yaml").unwrap(), &mut text)
            .unwrap();
        let manifest: AppManifest = serde_yaml::from_str(&text).unwrap();
        assert_eq!(manifest.name, "browser");
        assert!(manifest.public);
        assert_eq!(manifest.permissions["add"], ["admin", "mcp:browser"]);
        assert!(matches!(
            manifest.app,
            AppSpec::Image { ref command, .. } if command[1] == "mcp"
        ));
        assert!(package_zip("nope", None, None).is_err());
    }
}
