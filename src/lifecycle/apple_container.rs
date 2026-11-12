//! Alternate Scenario 1 driver: Amoeba drives a single container through
//! Apple's native `container` CLI (Containerization + Virtualization.framework
//! — one lightweight Linux micro-VM per container) instead of the Docker
//! Engine API. Selected per-machine via `machines.<name>.drivers:
//! ["apple-container"]`.
//!
//! Unlike `DockerContainerDriver`, there is no stable, host-resolvable name
//! for the upstream host here: `container` assigns each container a fresh IP
//! on every start (confirmed by hand: stopping and restarting the same
//! container moved it from `192.168.64.2` to `192.168.64.3`), and the bare
//! macOS host cannot resolve a container's name via DNS the way it can a
//! Docker container on a shared bridge network. So `ensure_started` returns
//! the freshly-resolved IP on every call — the caller (`routing::proxy`)
//! caches it in `ServiceRuntimeState` for reuse by warm requests.

use super::driver::{DriverError, SPEC_LABEL, resolve_env_from_secret, spec_fingerprint};
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;

pub struct AppleContainerDriver {
    /// Also the container's name (`container run --name <..>`): there's no
    /// `placement.ip` to reuse here, so the service's own catalog key is used.
    container_name: String,
    image: String,
    memory_limit_mb: Option<u64>,
    cpu_cores: Option<u64>,
    env: HashMap<String, String>,
    env_from_secret: HashMap<String, String>,
    /// Replaces the image's `CMD` when non-empty.
    command: Vec<String>,
}

impl AppleContainerDriver {
    pub fn new(
        container_name: String,
        image: String,
        memory_limit_mb: Option<u64>,
        cpu_cores: Option<u64>,
        env: HashMap<String, String>,
        env_from_secret: HashMap<String, String>,
        command: Vec<String>,
    ) -> Self {
        Self {
            container_name,
            image,
            memory_limit_mb,
            cpu_cores,
            env,
            env_from_secret,
            command,
        }
    }

    /// Ensures the container exists and is running, then returns its current
    /// IPv4 address. Always re-resolves the IP, even when already running,
    /// since it's cheap (one more `inspect`) and keeps this function's
    /// contract simple: "the returned string is always current."
    pub async fn ensure_started(&self) -> Result<String, DriverError> {
        let args = self.run_args(&self.resolved_env()?);
        let inspect = self.run(&["inspect", &self.container_name]).await?;

        if inspect.status.success() {
            let stdout = String::from_utf8_lossy(&inspect.stdout);
            let found = parse_inspect_output(&stdout)?;
            if found.spec.as_deref() != Some(spec_label_value(&args)) {
                // Made from an older spec (or before specs were labelled):
                // starting it would run the old image, command or env.
                let delete = self
                    .run(&["delete", "--force", &self.container_name])
                    .await?;
                Self::check_success("container delete", &delete)?;
                self.create_and_start(&args).await?;
            } else if found.state != "running" {
                let start = self.run(&["start", &self.container_name]).await?;
                Self::check_success("container start", &start)?;
            } else if let Some(ip) = found.ip {
                return Ok(ip);
            }
        } else {
            self.create_and_start(&args).await?;
        }

        self.resolve_current_ip().await
    }

    async fn resolve_current_ip(&self) -> Result<String, DriverError> {
        let inspect = self.run(&["inspect", &self.container_name]).await?;
        Self::check_success("container inspect", &inspect)?;
        let stdout = String::from_utf8_lossy(&inspect.stdout);
        parse_inspect_output(&stdout)?.ip.ok_or_else(|| {
            DriverError::Unavailable(format!(
                "container '{}' is running but reported no IPv4 address",
                self.container_name
            ))
        })
    }

    /// Plain env plus secrets; a secret wins over a plain var, as in the
    /// Docker driver.
    fn resolved_env(&self) -> Result<HashMap<String, String>, DriverError> {
        let mut env = self.env.clone();
        env.extend(resolve_env_from_secret(&self.env_from_secret)?);
        Ok(env)
    }

    /// Arguments for `container run`: the image, then `command` (which, as in
    /// `docker run`, replaces the image's `CMD`). Labelled with a fingerprint
    /// of the other arguments, so a changed spec is detected later.
    fn run_args(&self, env: &HashMap<String, String>) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            self.container_name.clone(),
        ];
        if let Some(mb) = self.memory_limit_mb {
            args.push("-m".into());
            args.push(format!("{mb}M"));
        }
        if let Some(cpus) = self.cpu_cores {
            args.push("-c".into());
            args.push(cpus.to_string());
        }
        // Sorted, so the same env always gives the same fingerprint.
        let mut env: Vec<_> = env.iter().collect();
        env.sort();
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push(self.image.clone());
        args.extend(self.command.iter().cloned());

        let label = format!(
            "{SPEC_LABEL}={}",
            spec_fingerprint(args.iter().map(String::as_str))
        );
        args.splice(4..4, ["-l".to_string(), label]);
        args
    }

    async fn create_and_start(&self, args: &[String]) -> Result<(), DriverError> {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.run(&arg_refs).await?;
        Self::check_success("container run", &output)
    }

    pub async fn stop(&self) -> Result<(), DriverError> {
        let output = self.run(&["stop", &self.container_name]).await?;
        if output.status.success() {
            return Ok(());
        }
        // Never created: nothing to do (mirrors DockerContainerDriver tolerating
        // a 404 from the Docker Engine). An already-stopped container is not an
        // error case at all here -- `container stop` itself returns exit 0 for it.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") {
            return Ok(());
        }
        Err(DriverError::CommandFailed {
            command: "container stop".to_string(),
            stderr: stderr.into_owned(),
        })
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, DriverError> {
        let mut cmd = Command::new("container");
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(cmd.output().await?)
    }

    fn check_success(command: &str, output: &std::process::Output) -> Result<(), DriverError> {
        if output.status.success() {
            Ok(())
        } else {
            Err(DriverError::CommandFailed {
                command: command.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }
}

/// What `ensure_started` needs from `container inspect`.
#[derive(Debug)]
struct Inspected {
    state: String,
    /// Any `/<prefix>` suffix stripped (Apple's CLI reports e.g.
    /// `"192.168.64.3/24"`). `None` while the container has no network entry
    /// yet, e.g. still starting.
    ip: Option<String>,
    /// The container's [`SPEC_LABEL`], if it has one.
    spec: Option<String>,
}

/// The fingerprint inside `run_args`' `-l amoeba.spec=<fingerprint>`.
fn spec_label_value(args: &[String]) -> &str {
    args.iter()
        .find_map(|a| a.strip_prefix(SPEC_LABEL)?.strip_prefix('='))
        .unwrap_or_default()
}

/// Parses `container inspect`'s JSON *array* output (one entry per requested ID).
fn parse_inspect_output(json: &str) -> Result<Inspected, DriverError> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        DriverError::Unavailable(format!("failed to parse `container inspect` output: {e}"))
    })?;

    let entry = value
        .as_array()
        .and_then(|arr| arr.first())
        .ok_or_else(|| {
            DriverError::Unavailable("`container inspect` returned no entries".to_string())
        })?;

    let state = entry["status"]["state"]
        .as_str()
        .ok_or_else(|| {
            DriverError::Unavailable("`container inspect` output missing status.state".to_string())
        })?
        .to_string();

    let ip = entry["status"]["networks"]
        .as_array()
        .and_then(|nets| nets.first())
        .and_then(|net| net["ipv4Address"].as_str())
        .map(|addr| addr.split('/').next().unwrap_or(addr).to_string());

    let spec = entry["configuration"]["labels"][SPEC_LABEL]
        .as_str()
        .map(str::to_string);

    Ok(Inspected { state, ip, spec })
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[deploy#App library#Start command]]
    #[test]
    fn start_command_follows_the_image() {
        let driver = AppleContainerDriver::new(
            "lightpanda".into(),
            "lightpanda/browser:latest".into(),
            None,
            None,
            HashMap::new(),
            HashMap::new(),
            vec![
                "/bin/lightpanda".into(),
                "mcp".into(),
                "--port".into(),
                "8080".into(),
            ],
        );
        let args = driver.run_args(&HashMap::new());
        let image_at = args
            .iter()
            .position(|a| a == "lightpanda/browser:latest")
            .unwrap();
        assert_eq!(
            args[image_at + 1..],
            ["/bin/lightpanda", "mcp", "--port", "8080"]
        );
    }

    // @lat: [[deploy#App library#Changed specs recreate the container]]
    #[test]
    fn spec_label_changes_with_the_spec_only() {
        let driver = |command: &[&str], env: &[(&str, &str)]| {
            AppleContainerDriver::new(
                "lightpanda".into(),
                "lightpanda/browser:latest".into(),
                Some(1024),
                None,
                env.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                HashMap::new(),
                command.iter().map(|c| c.to_string()).collect(),
            )
        };
        let label = |d: &AppleContainerDriver| {
            let args = d.run_args(&d.env);
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "-l" && w[1].starts_with("amoeba.spec="))
            );
            spec_label_value(&args).to_string()
        };
        let env = [("A", "1"), ("B", "2"), ("C", "3"), ("D", "4")];
        let mcp = label(&driver(&["mcp"], &env));

        // Same spec, same label, whatever the HashMap order.
        for _ in 0..8 {
            assert_eq!(label(&driver(&["mcp"], &env)), mcp);
        }
        assert_ne!(label(&driver(&["serve"], &env)), mcp);
        assert_ne!(label(&driver(&["mcp"], &[("A", "9")])), mcp);
        assert_ne!(label(&driver(&[], &env)), mcp);
    }

    /// Captured verbatim from `container inspect <name>` on a real running
    /// container (`container CLI version 1.1.0`), trimmed to the fields
    /// `parse_inspect_output` actually reads, plus a spec label.
    const RUNNING_FIXTURE: &str = r#"[
        {
            "configuration" : {
                "labels" : { "amoeba.spec" : "0123456789abcdef" }
            },
            "id" : "amoeba-test-hello",
            "status" : {
                "networks" : [
                    {
                        "hostname" : "amoeba-test-hello",
                        "ipv4Address" : "192.168.64.2/24",
                        "ipv4Gateway" : "192.168.64.1",
                        "network" : "default"
                    }
                ],
                "startedDate" : "2026-07-27T06:49:15Z",
                "state" : "running"
            }
        }
    ]"#;

    const STOPPED_FIXTURE: &str = r#"[
        {
            "id" : "amoeba-test-hello",
            "status" : {
                "networks" : [],
                "state" : "stopped"
            }
        }
    ]"#;

    #[test]
    fn parses_running_state_and_strips_ip_prefix() {
        let found = parse_inspect_output(RUNNING_FIXTURE).unwrap();
        assert_eq!(found.state, "running");
        assert_eq!(found.ip.as_deref(), Some("192.168.64.2"));
        assert_eq!(found.spec.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn parses_stopped_state_with_no_ip() {
        let found = parse_inspect_output(STOPPED_FIXTURE).unwrap();
        assert_eq!(found.state, "stopped");
        assert_eq!(found.ip, None);
        assert_eq!(found.spec, None);
    }

    #[test]
    fn rejects_empty_array() {
        assert!(parse_inspect_output("[]").is_err());
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(parse_inspect_output("not json").is_err());
    }
}
