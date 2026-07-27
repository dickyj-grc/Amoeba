//! Scenario 1 driver: Amoeba owns a single container's full lifecycle
//! directly via the Docker Engine API (`bollard`), rather than shelling out
//! to compose. Named `docker_container` (not `container`) to avoid confusion
//! with `lifecycle::container`, which is unrelated runtime-activity
//! bookkeeping (cooldown/connection tracking) — no Docker calls there.

use super::driver::{resolve_env_from_secret, DriverError};
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::models::HostConfig;
use bollard::Docker;
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
    env_from_secret: HashMap<String, String>,
}

impl DockerContainerDriver {
    pub fn new(
        docker: Option<Docker>,
        container_name: String,
        image: String,
        network: String,
        memory_limit_mb: Option<u64>,
        env_from_secret: HashMap<String, String>,
    ) -> Self {
        Self {
            docker,
            container_name,
            image,
            network,
            memory_limit_bytes: memory_limit_mb.map(|mb| (mb * 1024 * 1024) as i64),
            env_from_secret,
        }
    }

    fn docker(&self) -> Result<&Docker, DriverError> {
        self.docker
            .as_ref()
            .ok_or_else(|| DriverError::Unavailable("no connection to the local Docker Engine".to_string()))
    }

    pub async fn ensure_started(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;

        match docker
            .inspect_container(&self.container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => {
                let running = inspect.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                if running {
                    return Ok(());
                }
                docker
                    .start_container(&self.container_name, None::<StartContainerOptions<String>>)
                    .await?;
                Ok(())
            }
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => self.create_and_start().await,
            Err(e) => Err(e.into()),
        }
    }

    async fn create_and_start(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;
        let env = resolve_env_from_secret(&self.env_from_secret)?;
        let env: Vec<String> = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();

        let host_config = HostConfig {
            memory: self.memory_limit_bytes,
            network_mode: Some(self.network.clone()),
            ..Default::default()
        };
        let config = Config {
            image: Some(self.image.clone()),
            env: if env.is_empty() { None } else { Some(env) },
            host_config: Some(host_config),
            ..Default::default()
        };
        let options = CreateContainerOptions {
            name: self.container_name.clone(),
            platform: None,
        };

        docker.create_container(Some(options), config).await?;
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
            Err(BollardError::DockerResponseServerError { status_code: 304, .. }) => Ok(()),
            Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
