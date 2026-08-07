//! App package installation and removal.
//!
//! The manager materializes an Amoeba App Package onto disk and updates the
//! service catalog (`services.json`). Amoeba's file watcher then hot-reloads
//! the new catalog without a process restart.

use super::schema::{AppManifest, AppSpec, InstallValues, ResourceSpec};
use crate::config::schema::{
    ContainerResources, ContainerSpec, MachineConfig, Placement, ResourceQuantities, ServiceCatalog,
    ServiceConfig, StackSpec,
};
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{info, warn};
use zip::ZipArchive;

const MANIFEST_NAME: &str = "amoeba.yaml";

#[derive(Debug)]
pub enum AppManagerError {
    Io(std::io::Error),
    Yaml(serde_yaml::Error),
    Json(serde_json::Error),
    Zip(zip::result::ZipError),
    Validation(String),
    Command(String),
}

impl std::fmt::Display for AppManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppManagerError::Io(e) => write!(f, "io error: {e}"),
            AppManagerError::Yaml(e) => write!(f, "yaml error: {e}"),
            AppManagerError::Json(e) => write!(f, "json error: {e}"),
            AppManagerError::Zip(e) => write!(f, "zip error: {e}"),
            AppManagerError::Validation(msg) => write!(f, "validation error: {msg}"),
            AppManagerError::Command(msg) => write!(f, "command error: {msg}"),
        }
    }
}

impl std::error::Error for AppManagerError {}

impl From<std::io::Error> for AppManagerError {
    fn from(e: std::io::Error) -> Self {
        AppManagerError::Io(e)
    }
}

impl From<serde_yaml::Error> for AppManagerError {
    fn from(e: serde_yaml::Error) -> Self {
        AppManagerError::Yaml(e)
    }
}

impl From<serde_json::Error> for AppManagerError {
    fn from(e: serde_json::Error) -> Self {
        AppManagerError::Json(e)
    }
}

impl From<zip::result::ZipError> for AppManagerError {
    fn from(e: zip::result::ZipError) -> Self {
        AppManagerError::Zip(e)
    }
}

/// Coordinates app installs/uninstalls. Holds the path to Amoeba's service
/// catalog and a mutex so concurrent catalog writes do not corrupt the file.
pub struct AppManager {
    catalog_path: PathBuf,
    lock: Mutex<()>,
}

impl AppManager {
    pub fn new(catalog_path: impl AsRef<Path>) -> Self {
        Self {
            catalog_path: catalog_path.as_ref().to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    fn config_dir(&self) -> PathBuf {
        self.catalog_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn stacks_dir(&self) -> PathBuf {
        self.config_dir().join("stacks")
    }

    fn secrets_dir(&self) -> PathBuf {
        self.config_dir().join("secrets")
    }

    /// Installs or updates an app from a `.zip` package and user-supplied
    /// values. The catalog update is serialized with the internal mutex.
    pub async fn install(
        &self,
        package_zip: &[u8],
        values: InstallValues,
    ) -> Result<AppManifest, AppManagerError> {
        let temp_dir = tempfile::tempdir()?;
        extract_zip(package_zip, temp_dir.path())?;

        let manifest_path = temp_dir.path().join(MANIFEST_NAME);
        if !manifest_path.exists() {
            return Err(AppManagerError::Validation(format!(
                "package is missing {MANIFEST_NAME}"
            )));
        }

        let manifest: AppManifest = serde_yaml::from_reader(fs::File::open(&manifest_path)?)?;
        validate_manifest(&manifest)?;
        validate_values(&manifest, &values)?;

        let app_name = manifest.service_name().to_string();
        let stack_dir = self.stacks_dir().join(&app_name);
        let app_secrets_dir = self.secrets_dir().join(&app_name);

        // Materialize compose file and .env for compose apps.
        if let AppSpec::Compose { compose_file, .. } = &manifest.app {
            let src = temp_dir.path().join(compose_file);
            if !src.exists() {
                return Err(AppManagerError::Validation(format!(
                    "compose_file '{compose_file}' not found in package"
                )));
            }
            fs::create_dir_all(&stack_dir)?;
            let dst = stack_dir.join("compose.yaml");
            fs::copy(&src, &dst)?;

            if !values.env.is_empty() {
                let dotenv = values
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}\n"))
                    .collect::<String>();
                fs::write(stack_dir.join(".env"), dotenv)?;
            }
        }

        // Materialize secrets.
        fs::create_dir_all(&app_secrets_dir)?;
        for (key, value) in &values.secrets {
            let secret_path = app_secrets_dir.join(key);
            fs::write(&secret_path, value)?;
            let mut perms = fs::metadata(&secret_path)?.permissions();
            perms.set_readonly(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o600);
            }
            fs::set_permissions(&secret_path, perms)?;
        }

        // Update catalog under the mutex.
        let _guard = self.lock.lock().await;
        let mut catalog = load_catalog(&self.catalog_path)?;
        upsert_machine(&mut catalog);
        let service_config = build_service_config(&manifest, &values, &self.stacks_dir());
        catalog.services.insert(app_name.clone(), service_config);
        save_catalog(&self.catalog_path, &catalog)?;

        info!(app = %app_name, "installed/updated Amoeba app package");
        Ok(manifest)
    }

    /// Removes an app from the catalog and cleans up its stack/secrets. Also
    /// attempts to stop any running containers.
    pub async fn uninstall(&self, app_name: &str) -> Result<(), AppManagerError> {
        let _guard = self.lock.lock().await;

        let mut catalog = load_catalog(&self.catalog_path)?;
        let service = catalog
            .services
            .remove(app_name)
            .ok_or_else(|| AppManagerError::Validation(format!("app '{app_name}' is not installed")))?;
        save_catalog(&self.catalog_path, &catalog)?;

        // Best-effort container teardown outside the critical section is fine
        // because the catalog entry is already gone; routing stops immediately.
        stop_service_containers(app_name, &service).await?;

        let stack_dir = self.stacks_dir().join(app_name);
        let app_secrets_dir = self.secrets_dir().join(app_name);

        if stack_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&stack_dir) {
                warn!(app = %app_name, "failed to remove stack dir: {e}");
            }
        }
        if app_secrets_dir.exists() {
            if let Err(e) = fs::remove_dir_all(&app_secrets_dir) {
                warn!(app = %app_name, "failed to remove secrets dir: {e}");
            }
        }

        info!(app = %app_name, "uninstalled Amoeba app package");
        Ok(())
    }

    /// Lists installed app names from the catalog (best-effort, read-only).
    pub fn list_installed(&self) -> Result<Vec<String>, AppManagerError> {
        let catalog = load_catalog(&self.catalog_path)?;
        Ok(catalog.services.keys().cloned().collect())
    }
}

fn extract_zip(zip_bytes: &[u8], dest: &Path) -> Result<(), AppManagerError> {
    let reader = Cursor::new(zip_bytes);
    let mut archive = ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = match file.enclosed_name() {
            Some(path) => dest.join(path),
            None => continue,
        };

        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut out)?;
        }
    }
    Ok(())
}

fn validate_manifest(manifest: &AppManifest) -> Result<(), AppManagerError> {
    if manifest.api_version != "v1" {
        return Err(AppManagerError::Validation(format!(
            "unsupported api_version: {}",
            manifest.api_version
        )));
    }

    if manifest.name.is_empty() || manifest.name.contains('/') || manifest.name.contains('\\') {
        return Err(AppManagerError::Validation(
            "app name must be non-empty and not contain path separators".to_string(),
        ));
    }

    if manifest.placement.port == 0 {
        return Err(AppManagerError::Validation(
            "placement.port is required".to_string(),
        ));
    }

    if let Some(resources) = &manifest.resources {
        if let Some(memory) = &resources.memory {
            crate::config::schema::parse_quantity_to_mb(memory)
                .map_err(|e| AppManagerError::Validation(format!("invalid resources.memory: {e}")))?;
        }
        if let Some(gpu_vram) = &resources.gpu_vram {
            crate::config::schema::parse_quantity_to_mb(gpu_vram)
                .map_err(|e| AppManagerError::Validation(format!("invalid resources.gpu_vram: {e}")))?;
        }
    }

    Ok(())
}

fn validate_values(
    manifest: &AppManifest,
    values: &InstallValues,
) -> Result<(), AppManagerError> {
    // Ensure required secrets are present.
    for (key, field) in &manifest.schema.secrets {
        if field.required && !values.secrets.contains_key(key) {
            return Err(AppManagerError::Validation(format!(
                "missing required secret: {key}"
            )));
        }
        if let (Some(pattern), Some(value)) = (&field.validation_pattern, values.secrets.get(key)) {
            let regex = regex_lite::Regex::new(pattern)
                .map_err(|e| AppManagerError::Validation(format!("invalid regex for {key}: {e}")))?;
            if !regex.is_match(value) {
                return Err(AppManagerError::Validation(format!(
                    "secret {key} does not match required pattern"
                )));
            }
        }
    }

    // Ensure provided env values correspond to declared fields.
    for key in values.env.keys() {
        if !manifest.schema.env.contains_key(key) {
            return Err(AppManagerError::Validation(format!(
                "unexpected env value: {key}"
            )));
        }
    }

    Ok(())
}

fn load_catalog(path: &Path) -> Result<ServiceCatalog, AppManagerError> {
    if !path.exists() {
        return Ok(ServiceCatalog {
            version: 1,
            machines: HashMap::new(),
            scheduling: None,
            services: HashMap::new(),
        });
    }
    let content = fs::read_to_string(path)?;
    let catalog: ServiceCatalog = serde_json::from_str(&content)?;
    Ok(catalog)
}

fn save_catalog(path: &Path, catalog: &ServiceCatalog) -> Result<(), AppManagerError> {
    let content = serde_json::to_string_pretty(catalog)?;
    fs::write(path, content)?;
    Ok(())
}

/// Ensures a default `local` machine exists so that single-machine apps validate.
fn upsert_machine(catalog: &mut ServiceCatalog) {
    if catalog.machines.is_empty() {
        catalog.machines.insert(
            "local".to_string(),
            MachineConfig {
                r#type: "vm".to_string(),
                drivers: vec!["docker".to_string()],
                resources: ResourceQuantities::default(),
            },
        );
    }
}

fn build_service_config(
    manifest: &AppManifest,
    values: &InstallValues,
    stacks_root: &Path,
) -> ServiceConfig {
    let upstream_host = match &manifest.app {
        AppSpec::Image { .. } => manifest.service_name().to_string(),
        AppSpec::Compose { primary_service, .. } => {
            primary_service.clone().unwrap_or_else(|| manifest.service_name().to_string())
        }
    };

    let placement = Placement {
        r#type: "vm".to_string(),
        ip: match &manifest.app {
            AppSpec::Image { .. } => Some(manifest.service_name().to_string()),
            AppSpec::Compose { .. } => None,
        },
        primary_service: match &manifest.app {
            AppSpec::Image { .. } => None,
            AppSpec::Compose { .. } => Some(upstream_host),
        },
        port: manifest.placement.port,
        cooldown_seconds: manifest.placement.cooldown_seconds,
        machine: manifest.placement.machine.clone(),
    };

    let env_from_secret: HashMap<String, String> = manifest
        .schema
        .secrets
        .keys()
        .map(|k| (k.clone(), format!("{}/{}", manifest.service_name(), k)))
        .collect();

    let container = match &manifest.app {
        AppSpec::Image { image } => {
            let mut env = values.env.clone();
            // Apply declared defaults for env vars not supplied by the user.
            for (key, field) in &manifest.schema.env {
                if !env.contains_key(key) {
                    if let Some(default) = &field.default {
                        env.insert(key.clone(), default.clone());
                    }
                }
            }

            Some(ContainerSpec {
                image: image.clone(),
                resources: manifest.resources.as_ref().map(resource_spec_to_container_resources),
                env,
            })
        }
        AppSpec::Compose { .. } => None,
    };

    let stack_spec = match &manifest.app {
        AppSpec::Compose { .. } => {
            let compose_path = stacks_root
                .join(manifest.service_name())
                .join("compose.yaml");
            Some(StackSpec {
                compose_file: Some(compose_path.to_string_lossy().to_string()),
                project_name: Some(manifest.service_name().to_string()),
                compose_version: Some("3.8".to_string()),
                env_vars: {
                    let mut env = values.env.clone();
                    for (key, field) in &manifest.schema.env {
                        if !env.contains_key(key) {
                            if let Some(default) = &field.default {
                                env.insert(key.clone(), default.clone());
                            }
                        }
                    }
                    env
                },
                services: None,
                networks: HashMap::new(),
            })
        }
        AppSpec::Image { .. } => None,
    };

    ServiceConfig {
        placement,
        container,
        stack_spec,
        permissions: manifest.permissions.clone(),
        upstream_auth: None,
        public: false,
        operation_rules: None,
        env_from_secret,
    }
}

fn resource_spec_to_container_resources(spec: &ResourceSpec) -> ContainerResources {
    ContainerResources {
        limits: ResourceQuantities {
            memory: spec.memory.as_ref().map(|m| {
                crate::config::schema::Quantity(
                    crate::config::schema::parse_quantity_to_mb(m).expect("validated earlier")
                )
            }),
            cpu_cores: spec.cpu_cores,
            gpu_vram: spec.gpu_vram.as_ref().map(|v| {
                crate::config::schema::Quantity(
                    crate::config::schema::parse_quantity_to_mb(v).expect("validated earlier")
                )
            }),
        },
    }
}

async fn stop_service_containers(
    app_name: &str,
    service: &ServiceConfig,
) -> Result<(), AppManagerError> {
    if service.stack_spec.is_some() {
        let output = Command::new("docker")
            .args(["compose", "-p", app_name, "down"])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(app = %app_name, "docker compose down failed: {stderr}");
        }
    } else if service.container.is_some() {
        for cmd in [["stop", app_name].as_slice(), &["rm", "-f", app_name]].iter() {
            let output = Command::new("docker").args(*cmd).output().await?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(app = %app_name, "docker {} failed: {}", cmd[0], stderr);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    fn package_zip_with_manifest(manifest: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options: FileOptions<()> = FileOptions::default();
            zip.start_file("amoeba.yaml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    fn image_manifest() -> &'static str {
        r#"
api_version: v1
name: test-image
version: 1.0.0
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
  cooldown_seconds: 60
permissions:
  read: [admin]
schema:
  secrets:
    TOKEN:
      description: API token
      required: true
"#
    }

    #[tokio::test]
    async fn installs_image_app_to_catalog() {
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);

        let values = InstallValues {
            secrets: HashMap::from([("TOKEN".to_string(), "secret123".to_string())]),
            ..Default::default()
        };

        manager
            .install(&package_zip_with_manifest(image_manifest()), values)
            .await
            .unwrap();

        let catalog = load_catalog(&catalog_path).unwrap();
        assert!(catalog.services.contains_key("test-image"));

        let svc = catalog.services.get("test-image").unwrap();
        assert!(svc.container.is_some());
        assert_eq!(svc.container.as_ref().unwrap().image, "hello-world:latest");
        assert_eq!(svc.placement.port, 8080);
        assert_eq!(
            svc.env_from_secret.get("TOKEN").unwrap(),
            "test-image/TOKEN"
        );

        let secret_file = temp_dir.path().join("secrets/test-image/TOKEN");
        assert_eq!(fs::read_to_string(secret_file).unwrap(), "secret123");
    }

    #[tokio::test]
    async fn rejects_missing_required_secret() {
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);

        let result = manager
            .install(&package_zip_with_manifest(image_manifest()), InstallValues::default())
            .await;

        assert!(matches!(result, Err(AppManagerError::Validation(_))));
    }

    #[tokio::test]
    async fn uninstalls_image_app() {
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);

        let values = InstallValues {
            secrets: HashMap::from([("TOKEN".to_string(), "secret123".to_string())]),
            ..Default::default()
        };
        manager
            .install(&package_zip_with_manifest(image_manifest()), values)
            .await
            .unwrap();

        manager.uninstall("test-image").await.unwrap();

        let catalog = load_catalog(&catalog_path).unwrap();
        assert!(!catalog.services.contains_key("test-image"));
        assert!(!temp_dir.path().join("secrets/test-image").exists());
    }
}
