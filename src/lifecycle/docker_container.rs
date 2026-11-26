//! Scenario 1 driver: Amoeba owns a single container's full lifecycle
//! directly via the Docker Engine API (`bollard`), rather than shelling out
//! to compose. Named `docker_container` (not `container`) to avoid confusion
//! with `lifecycle::container`, which is unrelated runtime-activity
//! bookkeeping (cooldown/connection tracking) — no Docker calls there.

use super::driver::{DriverError, SPEC_LABEL, resolve_env_from_secret, spec_fingerprint};
use super::sandbox::{
    self, DEFAULT_MEMORY_MB, EGRESS_NETWORK, NO_ICC_OPTION, SCRATCH_PATHS, gateway_container,
};
use crate::config::schema::Sandbox;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::models::HostConfig;
use bollard::network::{
    ConnectNetworkOptions, CreateNetworkOptions, DisconnectNetworkOptions, InspectNetworkOptions,
};
use std::collections::HashMap;

pub struct DockerContainerDriver {
    /// `None` when connecting to the local Docker Engine failed at startup
    /// (e.g. no socket present) — every call then fails closed instead of the
    /// whole process refusing to start over an unreachable daemon.
    docker: Option<Docker>,
    /// Also the container's name: it must equal `placement.ip` so Docker DNS
    /// resolves it from Amoeba's own container on the shared network.
    container_name: String,
    image: String,
    network: String,
    memory_limit_bytes: Option<i64>,
    /// OCI runtime name (`runsc`, `kata-runtime`, …). `None` leaves the daemon default.
    runtime: Option<String>,
    /// Non-sensitive environment variables set at container creation time.
    env: HashMap<String, String>,
    env_from_secret: HashMap<String, String>,
    /// Replaces the image's `CMD` when non-empty.
    command: Vec<String>,
    /// Confinement and a network of its own; `None` runs it as before
    /// sandboxing, on the shared `network`.
    sandbox: Option<Sandbox>,
}

impl DockerContainerDriver {
    #[allow(clippy::too_many_arguments)] // one per container setting
    pub fn new(
        docker: Option<Docker>,
        container_name: String,
        image: String,
        network: String,
        memory_limit_mb: Option<u64>,
        runtime: Option<String>,
        env: HashMap<String, String>,
        env_from_secret: HashMap<String, String>,
        command: Vec<String>,
        sandbox: Option<Sandbox>,
    ) -> Self {
        Self {
            docker,
            container_name,
            image,
            network,
            memory_limit_bytes: memory_limit_mb.map(|mb| (mb * 1024 * 1024) as i64),
            runtime,
            env,
            env_from_secret,
            command,
            sandbox,
        }
    }

    /// The app's own network, when it is sandboxed and the gateway can join
    /// it; otherwise the shared one, as before.
    fn app_network(&self) -> Option<String> {
        self.sandbox.as_ref()?;
        if gateway_container().is_none() {
            tracing::warn!(
                app = %self.container_name,
                "the gateway is not in a container, so it cannot join a network of the app's own; using the shared network"
            );
            return None;
        }
        Some(sandbox::network_name(&self.container_name))
    }

    fn network(&self) -> String {
        self.app_network().unwrap_or_else(|| self.network.clone())
    }

    /// Creates the app's network, internal so it reaches nothing but the
    /// gateway, and attaches the gateway to it. An app with egress also joins
    /// [`EGRESS_NETWORK`] once created; the gateway never does, so no app
    /// network can become the gateway's default route.
    async fn prepare_network(&self) -> Result<(), DriverError> {
        let Some(name) = self.app_network() else {
            return Ok(());
        };
        let docker = self.docker()?;
        let internal = true;
        if self.sandbox.as_ref().is_some_and(|s| s.egress) {
            self.ensure_egress_network().await?;
        }
        let gateway = gateway_container().expect("app_network checked it");
        match docker
            .inspect_network(&name, None::<InspectNetworkOptions<String>>)
            .await
        {
            Ok(network) if network.internal == Some(internal) => {}
            Ok(_) => {
                // Egress changed: the internal flag cannot be changed in place.
                let _ = docker
                    .disconnect_network(
                        &name,
                        DisconnectNetworkOptions {
                            container: gateway.clone(),
                            force: true,
                        },
                    )
                    .await;
                docker.remove_network(&name).await?;
                self.create_network(&name, internal).await?;
            }
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => self.create_network(&name, internal).await?,
            Err(e) => return Err(e.into()),
        }
        match docker
            .connect_network(
                &name,
                ConnectNetworkOptions {
                    container: gateway,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(()) => Ok(()),
            // Already attached.
            Err(BollardError::DockerResponseServerError {
                status_code: 403 | 409,
                ..
            }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn ensure_egress_network(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;
        match docker
            .inspect_network(EGRESS_NETWORK, None::<InspectNetworkOptions<String>>)
            .await
        {
            Ok(_) => Ok(()),
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                let created = docker
                    .create_network(CreateNetworkOptions {
                        name: EGRESS_NETWORK.to_string(),
                        driver: "bridge".to_string(),
                        options: HashMap::from([(
                            NO_ICC_OPTION.0.to_string(),
                            NO_ICC_OPTION.1.to_string(),
                        )]),
                        ..Default::default()
                    })
                    .await;
                match created {
                    // Another app created it first.
                    Ok(_)
                    | Err(BollardError::DockerResponseServerError {
                        status_code: 409, ..
                    }) => Ok(()),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn create_network(&self, name: &str, internal: bool) -> Result<(), DriverError> {
        self.docker()?
            .create_network(CreateNetworkOptions {
                name: name.to_string(),
                driver: "bridge".to_string(),
                internal,
                labels: HashMap::from([("amoeba.app".to_string(), self.container_name.clone())]),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    fn docker(&self) -> Result<&Docker, DriverError> {
        self.docker.as_ref().ok_or_else(|| {
            DriverError::Unavailable("no connection to the local Docker Engine".to_string())
        })
    }

    pub async fn ensure_started(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;
        let config = self.config().await?;
        let spec = config
            .labels
            .as_ref()
            .and_then(|l| l.get(SPEC_LABEL))
            .cloned();

        match docker
            .inspect_container(&self.container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => {
                let found = inspect
                    .config
                    .as_ref()
                    .and_then(|c| c.labels.as_ref())
                    .and_then(|l| l.get(SPEC_LABEL));
                if found != spec.as_ref() {
                    // Made from an older spec (or before specs were labelled):
                    // starting it would run the old image, command or env.
                    docker
                        .remove_container(
                            &self.container_name,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await?;
                    self.prepare_network().await?;
                    return self.create_and_start(config).await;
                }
                let running = inspect
                    .state
                    .as_ref()
                    .and_then(|s| s.running)
                    .unwrap_or(false);
                if running {
                    return Ok(());
                }
                docker
                    .start_container(&self.container_name, None::<StartContainerOptions<String>>)
                    .await?;
                Ok(())
            }
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                self.prepare_network().await?;
                self.create_and_start(config).await
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The container to create: [`Self::config_for_user`] with the user a
    /// sandboxed root image runs as.
    async fn config(&self) -> Result<Config<String>, DriverError> {
        let user = match &self.sandbox {
            Some(sandbox) => sandbox::run_as_user(&self.image, false, sandbox.run_as_root).await,
            None => None,
        };
        self.config_for_user(user)
    }

    /// The container to create, labelled with a fingerprint of its settings
    /// so a changed spec is detected later.
    fn config_for_user(&self, user: Option<String>) -> Result<Config<String>, DriverError> {
        let mut env = self.env.clone();
        env.extend(resolve_env_from_secret(&self.env_from_secret)?);
        // Sorted, so the same env always gives the same fingerprint.
        let mut env: Vec<String> = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        env.sort();

        let network = self.network();
        let memory_limit = self.memory_limit_bytes.or(self
            .sandbox
            .as_ref()
            .map(|_| (DEFAULT_MEMORY_MB * 1024 * 1024) as i64));
        let memory = memory_limit.map(|b| b.to_string());
        let sandbox_spec = self
            .sandbox
            .as_ref()
            .map(|s| serde_json::to_string(s).unwrap_or_default())
            .unwrap_or_default();
        let fingerprint = spec_fingerprint(
            [
                self.image.as_str(),
                network.as_str(),
                sandbox_spec.as_str(),
                user.as_deref().unwrap_or(""),
                memory.as_deref().unwrap_or(""),
                self.runtime.as_deref().unwrap_or(""),
                "env",
            ]
            .into_iter()
            .chain(env.iter().map(String::as_str))
            .chain(["command"])
            .chain(self.command.iter().map(String::as_str)),
        );

        let mut host_config = HostConfig {
            memory: memory_limit,
            network_mode: Some(network),
            runtime: self.runtime.clone(),
            ..Default::default()
        };
        if let Some(sandbox) = &self.sandbox {
            host_config.cap_drop = Some(vec!["ALL".to_string()]);
            if !sandbox.capabilities.is_empty() {
                host_config.cap_add = Some(sandbox.capabilities.clone());
            }
            host_config.security_opt = Some(vec!["no-new-privileges:true".to_string()]);
            host_config.pids_limit = Some(i64::from(sandbox.pids_limit));
            if sandbox.read_only_root {
                host_config.readonly_rootfs = Some(true);
                host_config.tmpfs = Some(
                    SCRATCH_PATHS
                        .iter()
                        .map(|p| (p.to_string(), "rw,nosuid,size=64m".to_string()))
                        .collect(),
                );
            }
        }
        Ok(Config {
            image: Some(self.image.clone()),
            env: if env.is_empty() { None } else { Some(env) },
            cmd: (!self.command.is_empty()).then(|| self.command.clone()),
            user,
            labels: Some(HashMap::from([(SPEC_LABEL.to_string(), fingerprint)])),
            host_config: Some(host_config),
            ..Default::default()
        })
    }

    async fn create_and_start(&self, config: Config<String>) -> Result<(), DriverError> {
        let docker = self.docker()?;
        let options = CreateContainerOptions {
            name: self.container_name.clone(),
            platform: None,
        };

        docker.create_container(Some(options), config).await?;
        // Outbound access, and extra networks the app may use, such as one
        // holding its database.
        let egress = self
            .app_network()
            .and(self.sandbox.as_ref())
            .filter(|s| s.egress)
            .map(|_| EGRESS_NETWORK.to_string());
        for network in egress
            .iter()
            .chain(self.sandbox.iter().flat_map(|s| &s.networks))
        {
            docker
                .connect_network(
                    network,
                    ConnectNetworkOptions {
                        container: self.container_name.clone(),
                        ..Default::default()
                    },
                )
                .await?;
        }
        docker
            .start_container(&self.container_name, None::<StartContainerOptions<String>>)
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;

        match docker
            .stop_container(&self.container_name, Some(StopContainerOptions { t: 10 }))
            .await
        {
            Ok(()) => Ok(()),
            // Already stopped (304) or never created (404): nothing to do.
            Err(BollardError::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(BollardError::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(command: &[&str], env: &[(&str, &str)]) -> String {
        let driver = DockerContainerDriver::new(
            None,
            "lightpanda".into(),
            "lightpanda/browser:latest".into(),
            "amoeba-net".into(),
            Some(1024),
            None,
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            HashMap::new(),
            command.iter().map(|c| c.to_string()).collect(),
            None,
        );
        driver.config_for_user(None).unwrap().labels.unwrap()[SPEC_LABEL].clone()
    }

    // @lat: [[gateway#Sandbox#What the container gets]]
    #[test]
    fn a_sandboxed_container_is_confined() {
        let driver = DockerContainerDriver::new(
            None,
            "scraper".into(),
            "img".into(),
            "amoeba-net".into(),
            None,
            None,
            HashMap::new(),
            HashMap::new(),
            Vec::new(),
            Some(Sandbox {
                capabilities: vec!["NET_BIND_SERVICE".into()],
                ..Default::default()
            }),
        );
        let config = driver
            .config_for_user(Some(sandbox::NON_ROOT_USER.into()))
            .unwrap();
        let host = config.host_config.unwrap();
        assert_eq!(host.cap_drop.unwrap(), ["ALL"]);
        assert_eq!(host.cap_add.unwrap(), ["NET_BIND_SERVICE"]);
        assert_eq!(host.security_opt.unwrap(), ["no-new-privileges:true"]);
        assert_eq!(host.readonly_rootfs, Some(true));
        assert!(host.tmpfs.unwrap().contains_key("/tmp"));
        assert_eq!(host.pids_limit, Some(256));
        assert_eq!(host.memory, Some((DEFAULT_MEMORY_MB * 1024 * 1024) as i64));
        assert_eq!(config.user.as_deref(), Some(sandbox::NON_ROOT_USER));

        // Unsandboxed services (written by hand, or installed before) are untouched.
        let plain = DockerContainerDriver::new(
            None,
            "old".into(),
            "img".into(),
            "amoeba-net".into(),
            None,
            None,
            HashMap::new(),
            HashMap::new(),
            Vec::new(),
            None,
        );
        let host = plain.config_for_user(None).unwrap().host_config.unwrap();
        assert!(host.cap_drop.is_none() && host.readonly_rootfs.is_none() && host.memory.is_none());
        assert_eq!(host.network_mode.as_deref(), Some("amoeba-net"));
    }

    #[test]
    fn spec_label_changes_with_the_spec_only() {
        let env = [("A", "1"), ("B", "2"), ("C", "3"), ("D", "4")];
        let mcp = label(&["mcp"], &env);
        for _ in 0..8 {
            assert_eq!(label(&["mcp"], &env), mcp);
        }
        assert_ne!(label(&["serve"], &env), mcp);
        assert_ne!(label(&["mcp"], &[("A", "9")]), mcp);
        assert_ne!(label(&[], &env), mcp);
    }
}
