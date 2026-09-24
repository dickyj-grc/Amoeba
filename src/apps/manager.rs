//! App package installation and removal.
//!
//! The manager materializes an Amoeba App Package onto disk and updates the
//! service catalog (`services.json`). Amoeba's file watcher then hot-reloads
//! the new catalog without a process restart.

use super::schema::{
    AppManifest, AppSpec, InstallValues, PolicyGrant, PolicyPatch, ResourceSpec, SecretInject,
};
use super::state::LifecycleStatus;
use crate::config::schema::{
    ContainerResources, ContainerSpec, MachineConfig, Placement, ResourceQuantities,
    ServiceCatalog, ServiceConfig, StackSpec,
};
use crate::lifecycle::container::wait_until_ready;
use crate::state::AppState;
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
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
    Decrypt(String),
    Quota(String),
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
            AppManagerError::Decrypt(msg) => write!(f, "decryption error: {msg}"),
            AppManagerError::Quota(msg) => write!(f, "quota exceeded: {msg}"),
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
/// catalog, an optional age identity for decrypting secret files, and a mutex
/// so concurrent catalog writes do not corrupt the file.
pub struct AppManager {
    catalog_path: PathBuf,
    age_identity: Option<age::x25519::Identity>,
    lock: Mutex<()>,
}

impl AppManager {
    pub fn new(catalog_path: impl AsRef<Path>) -> Self {
        Self::with_identity(catalog_path, age_identity_from_env())
    }

    pub fn with_identity(
        catalog_path: impl AsRef<Path>,
        identity: Option<age::x25519::Identity>,
    ) -> Self {
        Self {
            catalog_path: catalog_path.as_ref().to_path_buf(),
            age_identity: identity,
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

        let mut values = values;
        decrypt_secret_files(
            &manifest,
            temp_dir.path(),
            self.age_identity.as_ref(),
            &mut values,
        )
        .await?;
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
        crate::config::watcher::validate_service_access(&app_name, &service_config)
            .map_err(AppManagerError::Validation)?;
        enforce_tenant_quota(&catalog, &app_name, &service_config)?;
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
        let service = catalog.services.remove(app_name).ok_or_else(|| {
            AppManagerError::Validation(format!("app '{app_name}' is not installed"))
        })?;
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

    /// Reads one service from the catalog on disk.
    pub fn get_service(&self, app_name: &str) -> Result<ServiceConfig, AppManagerError> {
        let catalog = load_catalog(&self.catalog_path)?;
        catalog.services.get(app_name).cloned().ok_or_else(|| {
            AppManagerError::Validation(format!("app '{app_name}' is not installed"))
        })
    }

    /// Updates access policy and writes the catalog so the watcher hot-reloads it.
    pub async fn update_policy(
        &self,
        app_name: &str,
        patch: PolicyPatch,
    ) -> Result<ServiceConfig, AppManagerError> {
        let _guard = self.lock.lock().await;
        let mut catalog = load_catalog(&self.catalog_path)?;
        let service = catalog.services.get_mut(app_name).ok_or_else(|| {
            AppManagerError::Validation(format!("app '{app_name}' is not installed"))
        })?;
        apply_policy_patch(service, &patch)?;
        let updated = service.clone();
        crate::config::watcher::validate_service_access(app_name, &updated)
            .map_err(AppManagerError::Validation)?;
        enforce_tenant_quota(&catalog, app_name, &updated)?;
        save_catalog(&self.catalog_path, &catalog)?;
        Ok(updated)
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

    if manifest.public && (manifest.tenant.is_some() || manifest.tenant_permissions.is_some()) {
        return Err(AppManagerError::Validation(
            "a public app cannot also set tenant or tenant_permissions".into(),
        ));
    }
    if manifest.tenant.is_some() && manifest.tenant_permissions.is_some() {
        return Err(AppManagerError::Validation(
            "set tenant or tenant_permissions, not both".into(),
        ));
    }
    if manifest.tenant_permissions.is_some() && !manifest.permissions.is_empty() {
        return Err(AppManagerError::Validation(
            "tenant_permissions replaces permissions; omit permissions".into(),
        ));
    }
    if let Some(auth) = &manifest.upstream_auth {
        match auth.r#type.as_str() {
            "bearer_static" | "custom_header" => {}
            other => {
                return Err(AppManagerError::Validation(format!(
                    "unknown upstream_auth type '{other}'"
                )));
            }
        }
        if auth.r#type == "custom_header" && auth.header_name.as_deref().is_none_or(str::is_empty) {
            return Err(AppManagerError::Validation(
                "custom_header upstream_auth requires header_name".into(),
            ));
        }
        if auth.token_secret.is_none() && auth.token_env_var.is_none() {
            return Err(AppManagerError::Validation(
                "upstream_auth requires token_secret or token_env_var".into(),
            ));
        }
        if let Some(secret) = &auth.token_secret {
            let key = secret.rsplit('/').next().unwrap_or(secret);
            if !secret.contains('/') && !manifest.schema.secrets.contains_key(key) {
                return Err(AppManagerError::Validation(format!(
                    "upstream_auth token_secret '{secret}' is not declared in schema.secrets"
                )));
            }
        }
    }

    if let Some(resources) = &manifest.resources {
        if let Some(memory) = &resources.memory {
            crate::config::schema::parse_quantity_to_mb(memory).map_err(|e| {
                AppManagerError::Validation(format!("invalid resources.memory: {e}"))
            })?;
        }
        if let Some(gpu_vram) = &resources.gpu_vram {
            crate::config::schema::parse_quantity_to_mb(gpu_vram).map_err(|e| {
                AppManagerError::Validation(format!("invalid resources.gpu_vram: {e}"))
            })?;
        }
    }

    Ok(())
}

fn validate_values(manifest: &AppManifest, values: &InstallValues) -> Result<(), AppManagerError> {
    // Ensure required secrets are present.
    for (key, field) in &manifest.schema.secrets {
        if field.required && !values.secrets.contains_key(key) {
            return Err(AppManagerError::Validation(format!(
                "missing required secret: {key}"
            )));
        }
        if let (Some(pattern), Some(value)) = (&field.validation_pattern, values.secrets.get(key)) {
            let regex = regex_lite::Regex::new(pattern).map_err(|e| {
                AppManagerError::Validation(format!("invalid regex for {key}: {e}"))
            })?;
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

async fn decrypt_secret_files(
    manifest: &AppManifest,
    package_root: &Path,
    identity: Option<&age::x25519::Identity>,
    values: &mut InstallValues,
) -> Result<(), AppManagerError> {
    let identity = match identity {
        Some(id) => id,
        None => {
            // If no age identity is configured, encrypted secret files cannot be
            // decrypted. This is only an error if the package actually uses them.
            let any_encrypted = manifest.schema.secrets.values().any(|f| f.file.is_some());
            if any_encrypted {
                return Err(AppManagerError::Decrypt(
                    "package contains age-encrypted secret files but AMOEBA_AGE_SECRET_KEY is not set"
                        .to_string(),
                ));
            }
            return Ok(());
        }
    };

    for (key, field) in &manifest.schema.secrets {
        let Some(file_path) = &field.file else {
            continue;
        };

        // User-provided values take precedence over encrypted files.
        if values.secrets.contains_key(key) {
            continue;
        }

        let full_path = package_root.join(file_path);
        if !full_path.exists() {
            return Err(AppManagerError::Validation(format!(
                "secret file for '{key}' not found in package: {file_path}"
            )));
        }

        let plaintext = decrypt_age_file(&full_path, &identity).await?;
        let plaintext = String::from_utf8(plaintext).map_err(|e| {
            AppManagerError::Decrypt(format!("secret '{key}' is not valid UTF-8: {e}"))
        })?;

        values.secrets.insert(key.clone(), plaintext);
    }

    Ok(())
}

fn age_identity_from_env() -> Option<age::x25519::Identity> {
    std::env::var("AMOEBA_AGE_SECRET_KEY")
        .ok()
        .and_then(|key| age::x25519::Identity::from_str(&key).ok())
}

async fn decrypt_age_file(
    path: &Path,
    identity: &age::x25519::Identity,
) -> Result<Vec<u8>, AppManagerError> {
    let file = fs::File::open(path)?;
    let decryptor = age::Decryptor::new(file)
        .map_err(|e| AppManagerError::Decrypt(format!("failed to create age decryptor: {e}")))?;

    let mut plaintext = Vec::new();
    match decryptor {
        age::Decryptor::Recipients(d) => {
            let mut reader = d
                .decrypt(std::iter::once(identity as &dyn age::Identity))
                .map_err(|e| {
                    AppManagerError::Decrypt(format!("failed to decrypt recipients: {e}"))
                })?;
            reader.read_to_end(&mut plaintext)?;
        }
        age::Decryptor::Passphrase(_) => {
            return Err(AppManagerError::Decrypt(
                "passphrase-encrypted age files are not supported".to_string(),
            ));
        }
    }

    Ok(plaintext)
}

fn load_catalog(path: &Path) -> Result<ServiceCatalog, AppManagerError> {
    if !path.exists() {
        return Ok(ServiceCatalog {
            version: 1,
            machines: HashMap::new(),
            scheduling: None,
            max_services_per_tenant: None,
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
        AppSpec::Compose {
            primary_service, ..
        } => primary_service
            .clone()
            .unwrap_or_else(|| manifest.service_name().to_string()),
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

    let mut env_from_secret = HashMap::new();
    let mut header_from_secret = HashMap::new();
    for (key, field) in &manifest.schema.secrets {
        let secret_ref = format!("{}/{}", manifest.service_name(), key);
        match field.inject {
            SecretInject::Env => {
                env_from_secret.insert(key.clone(), secret_ref);
            }
            SecretInject::Proxy => {
                header_from_secret.insert(key.clone(), secret_ref);
            }
        }
    }

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
                runtime: manifest.placement.runtime.clone(),
                resources: manifest
                    .resources
                    .as_ref()
                    .map(resource_spec_to_container_resources),
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

    let mut upstream_auth = manifest.upstream_auth.clone();
    if let Some(auth) = upstream_auth.as_mut() {
        if let Some(secret) = auth.token_secret.as_mut() {
            if !secret.contains('/') {
                *secret = format!("{}/{}", manifest.service_name(), secret);
            }
        }
    }

    ServiceConfig {
        placement,
        container,
        stack_spec,
        permissions: manifest.permissions.clone(),
        tenant: manifest.tenant.clone(),
        tenant_permissions: manifest.tenant_permissions.clone(),
        upstream_auth,
        public: manifest.public,
        operation_rules: None,
        env_from_secret,
        header_from_secret,
    }
}

fn resource_spec_to_container_resources(spec: &ResourceSpec) -> ContainerResources {
    ContainerResources {
        limits: ResourceQuantities {
            memory: spec.memory.as_ref().map(|m| {
                crate::config::schema::Quantity(
                    crate::config::schema::parse_quantity_to_mb(m).expect("validated earlier"),
                )
            }),
            cpu_cores: spec.cpu_cores,
            gpu_vram: spec.gpu_vram.as_ref().map(|v| {
                crate::config::schema::Quantity(
                    crate::config::schema::parse_quantity_to_mb(v).expect("validated earlier"),
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

/// Spawn a background task that pulls an app's image(s), starts it once, and
/// probes its port. The task updates the per-app `AppLifecycleState` as it
/// progresses (`installed` -> `pulling` -> `ready`/`error`).
pub fn spawn_readiness_probe(state: Arc<AppState>, app_name: String) {
    tokio::spawn(async move {
        let Some(app_state) = state.app_state(&app_name) else {
            return;
        };

        // If another probe already finished (e.g. catalog reload after install),
        // leave it alone.
        let snap = app_state.snapshot();
        if matches!(snap.state, LifecycleStatus::Ready | LifecycleStatus::Error) {
            return;
        }

        app_state.transition(LifecycleStatus::Pulling, None);

        // Wait for the catalog watcher to pick up the just-written service
        // (it reloads asynchronously after the install handler writes
        // services.json). Bound the wait so a stuck watcher does not hang
        // this probe forever; a real removal still surfaces as an error.
        let service_cfg = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(cfg) = state.catalog.load().services.get(&app_name).cloned() {
                    return Some(cfg);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or(None);

        let Some(service_cfg) = service_cfg else {
            app_state.transition(
                LifecycleStatus::Error,
                Some("service removed from catalog".to_string()),
            );
            return;
        };

        if let Err(e) = pull_images(&service_cfg).await {
            app_state.transition(
                LifecycleStatus::Error,
                Some(format!("image pull failed: {e}")),
            );
            return;
        }

        // Wait for the driver to be built by the catalog watcher (it reloads
        // after the install write). Bound the wait so a stuck watcher does not
        // hang this probe forever.
        let driver = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(d) = state.drivers.load().get(&app_name).cloned() {
                    return Some(d);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or(None);

        let Some(driver) = driver else {
            app_state.transition(
                LifecycleStatus::Error,
                Some("driver was not built in time".to_string()),
            );
            return;
        };

        if let Err(e) = driver.ensure_started().await {
            app_state.transition(
                LifecycleStatus::Error,
                Some(format!("failed to start: {e}")),
            );
            return;
        }

        // A reinstall reuses the existing `ServiceRuntimeState` entry (keyed by
        // app name, not container instance), so its `last_accessed_unix` can be
        // stale from a previous run. Reset it now that a fresh container has
        // actually started, or the reaper's next sweep (every 10s) can see a
        // long-idle timestamp and scale this brand-new container back to zero
        // before anyone ever gets to use it.
        if let Some(runtime) = state.runtime_states.load().get(&app_name) {
            runtime.touch();
        }

        let host = service_cfg.upstream_host();
        if !wait_until_ready(host, service_cfg.placement.port).await {
            app_state.transition(
                LifecycleStatus::Error,
                Some("service did not become ready in time".to_string()),
            );
            return;
        }

        app_state.transition(LifecycleStatus::Ready, None);
        info!(app = %app_name, "app is ready");
    });
}

fn orgs_of(service: &ServiceConfig) -> Vec<&str> {
    if let Some(tenant) = service.tenant.as_deref() {
        return vec![tenant];
    }
    if let Some(map) = &service.tenant_permissions {
        let mut orgs: Vec<&str> = map.keys().map(String::as_str).collect();
        orgs.sort_unstable();
        return orgs;
    }
    Vec::new()
}

fn enforce_tenant_quota(
    catalog: &ServiceCatalog,
    name: &str,
    service: &ServiceConfig,
) -> Result<(), AppManagerError> {
    let Some(max) = catalog.max_services_per_tenant else {
        return Ok(());
    };
    for org in orgs_of(service) {
        let others = catalog
            .services
            .iter()
            .filter(|(other_name, other)| {
                other_name.as_str() != name && orgs_of(other).contains(&org)
            })
            .count();
        if others + 1 > max as usize {
            return Err(AppManagerError::Quota(format!(
                "tenant '{org}' already has {others} other service(s); the limit is {max}"
            )));
        }
    }
    Ok(())
}

fn apply_policy_patch(
    service: &mut ServiceConfig,
    patch: &PolicyPatch,
) -> Result<(), AppManagerError> {
    if patch.tenant.is_some() && patch.tenant_permissions.is_some() {
        return Err(AppManagerError::Validation(
            "set tenant or tenant_permissions, not both".into(),
        ));
    }
    if patch.permissions.is_some() && patch.tenant_permissions.is_some() {
        return Err(AppManagerError::Validation(
            "tenant_permissions replaces permissions; omit permissions".into(),
        ));
    }

    if let Some(by_org) = &patch.tenant_permissions {
        service.tenant = None;
        service.permissions.clear();
        service.tenant_permissions = Some(by_org.clone());
    } else if patch.clear_tenant_permissions {
        service.tenant_permissions = None;
    }

    if let Some(tenant) = &patch.tenant {
        service.tenant_permissions = None;
        service.tenant = Some(tenant.clone());
    } else if patch.clear_tenant {
        service.tenant = None;
    }

    if let Some(permissions) = &patch.permissions {
        service.permissions = permissions.clone();
    }
    if let Some(public) = patch.public {
        service.public = public;
    }
    if let Some(grant) = &patch.grant {
        edit_role(service, grant, true)?;
    }
    if let Some(revoke) = &patch.revoke {
        edit_role(service, revoke, false)?;
    }
    Ok(())
}

fn edit_role(
    service: &mut ServiceConfig,
    grant: &PolicyGrant,
    add: bool,
) -> Result<(), AppManagerError> {
    if grant.operation.is_empty() || grant.role.is_empty() {
        return Err(AppManagerError::Validation(
            "operation and role are required".into(),
        ));
    }

    if service.tenant_permissions.is_some() {
        let org = grant
            .org
            .as_deref()
            .filter(|org| !org.is_empty())
            .ok_or_else(|| {
                AppManagerError::Validation(
                    "org is required when the service uses tenant_permissions".into(),
                )
            })?;
        let map = service
            .tenant_permissions
            .as_mut()
            .and_then(|by_org| by_org.get_mut(org))
            .ok_or_else(|| {
                AppManagerError::Validation(format!("org '{org}' is not on this service"))
            })?;
        edit_role_list(map, &grant.operation, &grant.role, add);
        return Ok(());
    }

    if let Some(tenant) = &service.tenant {
        if let Some(org) = grant.org.as_deref().filter(|org| !org.is_empty()) {
            if org != tenant {
                return Err(AppManagerError::Validation(format!(
                    "service is bound to tenant '{tenant}'"
                )));
            }
        }
    } else if grant.org.as_ref().is_some_and(|org| !org.is_empty()) {
        return Err(AppManagerError::Validation(
            "set tenant or tenant_permissions before granting a role to an org".into(),
        ));
    }

    edit_role_list(&mut service.permissions, &grant.operation, &grant.role, add);
    Ok(())
}

fn edit_role_list(map: &mut HashMap<String, Vec<String>>, operation: &str, role: &str, add: bool) {
    let roles = map.entry(operation.to_string()).or_default();
    if add {
        if !roles.iter().any(|existing| existing == role) {
            roles.push(role.to_string());
        }
    } else {
        roles.retain(|existing| existing != role);
    }
}

async fn local_image_present(image: &str) -> bool {
    match Command::new("docker")
        .args(["image", "inspect", image])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

async fn pull_images(service: &ServiceConfig) -> Result<(), AppManagerError> {
    if let Some(container) = &service.container {
        if !crate::apps::deploy::should_pull_image(local_image_present(&container.image).await) {
            return Ok(());
        }
        let output = Command::new("docker")
            .args(["pull", &container.image])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            return Err(AppManagerError::Command(format!(
                "docker pull {}: {}",
                container.image,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        return Ok(());
    }

    if let Some(stack) = &service.stack_spec {
        let project_name = stack.project_name.as_deref().ok_or_else(|| {
            AppManagerError::Validation("stack_spec missing project_name".to_string())
        })?;

        let mut cmd = Command::new("docker");
        cmd.arg("compose").arg("-p").arg(project_name);
        if let Some(path) = &stack.compose_file {
            cmd.arg("-f").arg(path);
        }
        cmd.arg("pull")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().await?;
        if !output.status.success() {
            return Err(AppManagerError::Command(format!(
                "docker compose pull failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        return Ok(());
    }

    // Should be unreachable because the catalog is validated, but fail closed.
    Err(AppManagerError::Validation(
        "service has neither container nor stack_spec".to_string(),
    ))
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
            .install(
                &package_zip_with_manifest(image_manifest()),
                InstallValues::default(),
            )
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

    fn package_zip_with_encrypted_secret(
        plaintext: &str,
        recipient: &age::x25519::Recipient,
    ) -> Vec<u8> {
        let mut encrypted = Vec::new();
        let encryptor = age::Encryptor::with_recipients(vec![
            Box::new(recipient.clone()) as Box<dyn age::Recipient + Send>
        ])
        .expect("recipient is valid");
        {
            let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
            writer.write_all(plaintext.as_bytes()).unwrap();
            writer.finish().unwrap();
        }

        let manifest = format!(
            r#"
api_version: v1
name: encrypted-app
version: 1.0.0
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
permissions:
  read: [admin]
schema:
  secrets:
    API_KEY:
      description: API key
      required: true
      file: secrets/api_key.age
"#
        );

        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let options: FileOptions<()> = FileOptions::default();
            zip.start_file("amoeba.yaml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("secrets/api_key.age", options).unwrap();
            zip.write_all(&encrypted).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[tokio::test]
    async fn decrypts_age_encrypted_secret_file() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let package = package_zip_with_encrypted_secret("super-secret-key", &recipient);

        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::with_identity(&catalog_path, Some(identity));

        manager
            .install(&package, InstallValues::default())
            .await
            .unwrap();

        let secret_file = temp_dir.path().join("secrets/encrypted-app/API_KEY");
        assert_eq!(fs::read_to_string(secret_file).unwrap(), "super-secret-key");
    }

    #[tokio::test]
    async fn user_provided_secret_overrides_encrypted_file() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let package = package_zip_with_encrypted_secret("from-file", &recipient);

        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::with_identity(&catalog_path, Some(identity));

        let values = InstallValues {
            secrets: HashMap::from([("API_KEY".to_string(), "from-user".to_string())]),
            ..Default::default()
        };
        manager.install(&package, values).await.unwrap();

        let secret_file = temp_dir.path().join("secrets/encrypted-app/API_KEY");
        assert_eq!(fs::read_to_string(secret_file).unwrap(), "from-user");
    }

    #[tokio::test]
    async fn proxy_secrets_are_not_injected_into_the_container_env() {
        let manifest = r#"
api_version: v1
name: proxy-secret
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
  runtime: runsc
tenant: org_client_alpha
permissions:
  read: [analyst]
schema:
  secrets:
    API_KEY:
      required: true
      inject: proxy
"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);
        manager
            .install(
                &package_zip_with_manifest(manifest),
                InstallValues {
                    secrets: HashMap::from([("API_KEY".into(), "hidden".into())]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let catalog = load_catalog(&catalog_path).unwrap();
        let svc = catalog.services.get("proxy-secret").unwrap();
        assert!(svc.env_from_secret.is_empty());
        assert_eq!(
            svc.header_from_secret.get("API_KEY").unwrap(),
            "proxy-secret/API_KEY"
        );
        assert_eq!(svc.tenant.as_deref(), Some("org_client_alpha"));
        assert_eq!(
            svc.container.as_ref().unwrap().runtime.as_deref(),
            Some("runsc")
        );
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("secrets/proxy-secret/API_KEY")).unwrap(),
            "hidden"
        );
    }

    #[tokio::test]
    async fn upstream_auth_secret_is_stored_as_a_service_reference() {
        let manifest = r#"
api_version: v1
name: obscura
app:
  type: image
  image: hello-world:latest
placement:
  port: 3000
permissions:
  read: [admin]
upstream_auth:
  type: bearer_static
  token_secret: OBSCURA_MCP_TOKEN
schema:
  secrets:
    OBSCURA_MCP_TOKEN:
      required: true
      inject: env
"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);
        let token = "e2e-obscura-mcp-token-0123456789ab";
        manager
            .install(
                &package_zip_with_manifest(manifest),
                InstallValues {
                    secrets: HashMap::from([("OBSCURA_MCP_TOKEN".into(), token.into())]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let catalog = load_catalog(&catalog_path).unwrap();
        let svc = catalog.services.get("obscura").unwrap();
        let auth = svc.upstream_auth.as_ref().unwrap();
        assert_eq!(auth.r#type, "bearer_static");
        assert_eq!(
            auth.token_secret.as_deref(),
            Some("obscura/OBSCURA_MCP_TOKEN")
        );
        assert_eq!(
            svc.env_from_secret.get("OBSCURA_MCP_TOKEN").unwrap(),
            "obscura/OBSCURA_MCP_TOKEN"
        );
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("secrets/obscura/OBSCURA_MCP_TOKEN")).unwrap(),
            token
        );
    }

    #[tokio::test]
    async fn tenant_quota_counts_services_per_org() {
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);
        let manifest = |name: &str| {
            format!(
                r#"
api_version: v1
name: {name}
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
tenant: org_client_alpha
permissions:
  read: [analyst]
"#
            )
        };
        manager
            .install(
                &package_zip_with_manifest(&manifest("one")),
                InstallValues::default(),
            )
            .await
            .unwrap();

        let mut catalog = load_catalog(&catalog_path).unwrap();
        catalog.max_services_per_tenant = Some(1);
        save_catalog(&catalog_path, &catalog).unwrap();

        let err = manager
            .install(
                &package_zip_with_manifest(&manifest("two")),
                InstallValues::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppManagerError::Quota(_)));

        manager
            .install(
                &package_zip_with_manifest(&manifest("one")),
                InstallValues::default(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn policy_patch_grants_a_role_inside_one_org() {
        let manifest = r#"
api_version: v1
name: shared
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
tenant_permissions:
  org_client_alpha:
    read: [viewer]
  org_client_beta:
    read: [analyst]
"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let catalog_path = temp_dir.path().join("services.json");
        let manager = AppManager::new(&catalog_path);
        manager
            .install(
                &package_zip_with_manifest(manifest),
                InstallValues::default(),
            )
            .await
            .unwrap();

        manager
            .update_policy(
                "shared",
                PolicyPatch {
                    grant: Some(PolicyGrant {
                        org: Some("org_client_alpha".into()),
                        operation: "add".into(),
                        role: "analyst".into(),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let svc = manager.get_service("shared").unwrap();
        let alpha = svc
            .tenant_permissions
            .as_ref()
            .unwrap()
            .get("org_client_alpha")
            .unwrap();
        assert_eq!(alpha.get("add").unwrap(), &vec!["analyst".to_string()]);
        assert!(
            svc.tenant_permissions
                .as_ref()
                .unwrap()
                .get("org_client_beta")
                .unwrap()
                .get("add")
                .is_none()
        );
    }
}
