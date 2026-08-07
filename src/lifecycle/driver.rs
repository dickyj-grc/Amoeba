//! Dispatches a service's container lifecycle to the driver implied by its
//! workload: a single container via the Docker Engine API (`bollard`,
//! `docker_container.rs`), or a multi-container stack via the `docker
//! compose` CLI (`compose.rs`). Built once per service at catalog load/reload
//! time (see `state.rs`), mirroring how `runtime_states` is built alongside
//! the catalog.
//!
//! Not to be confused with `lifecycle::container`, which is unrelated runtime
//! bookkeeping (cooldown timers, connection counts) — no Docker calls there.

use super::apple_container::AppleContainerDriver;
use super::compose::ComposeDriver;
use super::docker_container::DockerContainerDriver;
use crate::config::schema::{ContainerSpec, ServiceCatalog, ServiceConfig, StackMode};
use bollard::Docker;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Directory secret references (`env_from_secret`) are resolved relative to.
/// Overridable via `AMOEBA_SECRETS_DIR` for tests/non-default deployments.
fn secrets_dir() -> String {
    std::env::var("AMOEBA_SECRETS_DIR").unwrap_or_else(|_| "/etc/amoeba/secrets".to_string())
}

/// Reads each `env_from_secret` reference from disk (`<secrets_dir>/<value>`,
/// trimmed) at call time. Deliberately not cached: values are resolved fresh
/// on every `ensure_started`, and are never written back into a generated
/// compose file or Amoeba's own config, only ever into the live container/
/// subprocess environment.
pub(super) fn resolve_env_from_secret(
    env_from_secret: &HashMap<String, String>,
) -> Result<Vec<(String, String)>, DriverError> {
    let dir = secrets_dir();
    let mut resolved = Vec::with_capacity(env_from_secret.len());
    for (key, secret_ref) in env_from_secret {
        let path = Path::new(&dir).join(secret_ref);
        let value = std::fs::read_to_string(&path).map_err(|e| {
            DriverError::Unavailable(format!(
                "failed to resolve secret '{secret_ref}' for env var '{key}' at {}: {e}",
                path.display()
            ))
        })?;
        resolved.push((key.clone(), value.trim_end().to_string()));
    }
    Ok(resolved)
}

#[derive(Debug)]
pub enum DriverError {
    Docker(bollard::errors::Error),
    Io(std::io::Error),
    /// A shelled-out CLI command (`docker compose`, `container`) exited non-zero.
    CommandFailed { command: String, stderr: String },
    Unavailable(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Docker(e) => write!(f, "docker engine error: {e}"),
            DriverError::Io(e) => write!(f, "io error: {e}"),
            DriverError::CommandFailed { command, stderr } => write!(f, "`{command}` failed: {stderr}"),
            DriverError::Unavailable(msg) => write!(f, "driver unavailable: {msg}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<bollard::errors::Error> for DriverError {
    fn from(e: bollard::errors::Error) -> Self {
        DriverError::Docker(e)
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Io(e)
    }
}

/// Drives a single service's container lifecycle.
pub enum ServiceDriver {
    Container(DockerContainerDriver),
    AppleContainer(AppleContainerDriver),
    Compose(ComposeDriver),
}

impl ServiceDriver {
    /// Cold-boots the service if it isn't already running; a no-op if it is
    /// already running (Docker/Compose) or a cheap re-check (apple-container,
    /// which always re-resolves its IP). Returns `Some(host)` only when the
    /// driver dynamically resolved a fresh upstream host (apple-container,
    /// whose containers get a new IP on every start with no stable DNS name)
    /// — the caller caches it in `ServiceRuntimeState`. `None` means "use the
    /// static `placement.ip`/`primary_service` config value, unchanged."
    pub async fn ensure_started(&self) -> Result<Option<String>, DriverError> {
        match self {
            ServiceDriver::Container(d) => d.ensure_started().await.map(|_| None),
            ServiceDriver::AppleContainer(d) => d.ensure_started().await.map(Some),
            ServiceDriver::Compose(d) => d.ensure_started().await.map(|_| None),
        }
    }

    /// Cooldown-triggered scale-down. Always a `stop`, never a `down`/`delete`
    /// — a compose-driven stack keeps its containers/networks around so the
    /// next activation is a fast `start`, not a full `up -d` reconciliation
    /// (and likewise for the apple-container driver's `container stop`).
    pub async fn stop(&self) -> Result<(), DriverError> {
        match self {
            ServiceDriver::Container(d) => d.stop().await,
            ServiceDriver::AppleContainer(d) => d.stop().await,
            ServiceDriver::Compose(d) => d.stop().await,
        }
    }
}

/// The Docker network bollard-managed single containers are attached to, so
/// they're reachable by name (Docker DNS) from Amoeba's own container.
/// Overridable via `AMOEBA_DOCKER_NETWORK`; defaults to the network name
/// Amoeba's own `docker-compose.yml` already uses.
fn docker_network() -> String {
    std::env::var("AMOEBA_DOCKER_NETWORK").unwrap_or_else(|_| "amoeba-net".to_string())
}

/// Builds a driver for every service in the catalog. `docker` is `None` when
/// connecting to the local Docker Engine failed at startup (e.g. no socket
/// present) — container-workload drivers then fail closed with a clear
/// `DriverError::Unavailable` on first use rather than the whole process
/// refusing to start.
pub fn build_all(
    catalog: &ServiceCatalog,
    config_dir: &Path,
    docker: Option<Docker>,
) -> HashMap<String, Arc<ServiceDriver>> {
    let network = docker_network();
    let mut drivers = HashMap::with_capacity(catalog.services.len());

    for (name, svc) in &catalog.services {
        let driver = build_one(name, svc, catalog, config_dir, docker.clone(), &network);
        drivers.insert(name.clone(), Arc::new(driver));
    }

    drivers
}

/// Whether `svc`'s machine lists `"apple-container"` among its `drivers` —
/// the deciding factor for which `container`-workload backend to build.
/// `stack_spec` workloads are unaffected: Apple's `container` CLI has no
/// compose-equivalent today, so those always go through `ComposeDriver`
/// regardless of the machine's drivers.
fn uses_apple_container(svc: &ServiceConfig, catalog: &ServiceCatalog) -> bool {
    svc.machine_name(catalog)
        .and_then(|m| catalog.machines.get(m))
        .is_some_and(|m| m.drivers.iter().any(|d| d == "apple-container"))
}

fn build_one(
    name: &str,
    svc: &ServiceConfig,
    catalog: &ServiceCatalog,
    config_dir: &Path,
    docker: Option<Docker>,
    network: &str,
) -> ServiceDriver {
    if let Some(container) = &svc.container {
        return build_container_driver(name, svc, catalog, container, docker, network);
    }

    let stack = svc
        .stack_spec
        .as_ref()
        .expect("validated at load time: exactly one of container/stack_spec is set");

    ServiceDriver::Compose(match stack.mode() {
        StackMode::ComposeFile { path, project_name } => ComposeDriver::for_compose_file(
            path,
            project_name,
            svc.upstream_host(),
            svc.env_from_secret.clone(),
        ),
        StackMode::Inline { project_name } => {
            let generated_path = generated_compose_path(config_dir, name);
            ComposeDriver::for_inline(
                generated_path,
                project_name,
                svc.upstream_host(),
                svc.env_from_secret.clone(),
                stack.clone(),
            )
        }
    })
}

fn build_container_driver(
    name: &str,
    svc: &ServiceConfig,
    catalog: &ServiceCatalog,
    container: &ContainerSpec,
    docker: Option<Docker>,
    network: &str,
) -> ServiceDriver {
    let memory_limit_mb = container
        .resources
        .as_ref()
        .and_then(|r| r.limits.memory)
        .map(|q| q.0);

    if uses_apple_container(svc, catalog) {
        let cpu_cores = container.resources.as_ref().and_then(|r| r.limits.cpu_cores);
        return ServiceDriver::AppleContainer(AppleContainerDriver::new(
            name.to_string(),
            container.image.clone(),
            memory_limit_mb,
            cpu_cores,
            svc.env_from_secret.clone(),
        ));
    }

    ServiceDriver::Container(DockerContainerDriver::new(
        docker,
        svc.upstream_host().to_string(),
        container.image.clone(),
        network.to_string(),
        memory_limit_mb,
        container.env.clone(),
        svc.env_from_secret.clone(),
    ))
}

fn generated_compose_path(config_dir: &Path, service_name: &str) -> PathBuf {
    config_dir.join("generated").join(service_name).join("compose.yml")
}
