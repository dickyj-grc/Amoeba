//! Scenario 1 driver: Amoeba owns a single container's full lifecycle
//! directly via the Docker Engine API (`bollard`), rather than shelling out
//! to compose. Named `docker_container` (not `container`) to avoid confusion
//! with `lifecycle::container`, which is unrelated runtime-activity
//! bookkeeping (cooldown/connection tracking) — no Docker calls there.

use super::driver::{DriverError, SPEC_LABEL, resolve_env_from_secret, spec_fingerprint};
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::models::HostConfig;
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
        }
    }

    fn docker(&self) -> Result<&Docker, DriverError> {
        self.docker.as_ref().ok_or_else(|| {
            DriverError::Unavailable("no connection to the local Docker Engine".to_string())
        })
    }

    pub async fn ensure_started(&self) -> Result<(), DriverError> {
        let docker = self.docker()?;
        let config = self.config()?;
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
            }) => self.create_and_start(config).await,
            Err(e) => Err(e.into()),
        }
    }

    /// The container to create, labelled with a fingerprint of its settings
    /// so a changed spec is detected later.
    fn config(&self) -> Result<Config<String>, DriverError> {
        let mut env = self.env.clone();
        env.extend(resolve_env_from_secret(&self.env_from_secret)?);
        // Sorted, so the same env always gives the same fingerprint.
        let mut env: Vec<String> = env.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        env.sort();

        let memory = self.memory_limit_bytes.map(|b| b.to_string());
        let fingerprint = spec_fingerprint(
            [
                self.image.as_str(),
                self.network.as_str(),
                memory.as_deref().unwrap_or(""),
                self.runtime.as_deref().unwrap_or(""),
                "env",
            ]
            .into_iter()
            .chain(env.iter().map(String::as_str))
            .chain(["command"])
            .chain(self.command.iter().map(String::as_str)),
        );

        let host_config = HostConfig {
            memory: self.memory_limit_bytes,
            network_mode: Some(self.network.clone()),
            runtime: self.runtime.clone(),
            ..Default::default()
        };
        Ok(Config {
            image: Some(self.image.clone()),
            env: if env.is_empty() { None } else { Some(env) },
            cmd: (!self.command.is_empty()).then(|| self.command.clone()),
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
        );
        driver.config().unwrap().labels.unwrap()[SPEC_LABEL].clone()
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
