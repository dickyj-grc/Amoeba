//! Scenarios 2 & 3 driver: Amoeba drives a multi-container stack through the
//! `docker compose` CLI. For a pre-existing compose file (Scenario 2) it
//! shells out directly against it. For an inline `stack_spec.services` block
//! (Scenario 3) it regenerates an equivalent compose file on disk before
//! every `ensure_started`, then drives it through the identical CLI path.

use super::driver::{resolve_env_from_secret, DriverError};
use crate::config::schema::{InlineComposeService, StackSpec};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

pub struct ComposeDriver {
    compose_file: PathBuf,
    project_name: String,
    /// Which compose service the gateway routes to (`placement.primary_service`).
    primary_service: String,
    env_from_secret: HashMap<String, String>,
    /// `Some` for an inline (Scenario 3) stack: regenerated to `compose_file`
    /// before every `ensure_started`. `None` for a pre-existing file
    /// (Scenario 2), which Amoeba never touches.
    inline: Option<StackSpec>,
}

impl ComposeDriver {
    pub fn for_compose_file(
        path: &str,
        project_name: &str,
        primary_service: &str,
        env_from_secret: HashMap<String, String>,
    ) -> Self {
        Self {
            compose_file: PathBuf::from(path),
            project_name: project_name.to_string(),
            primary_service: primary_service.to_string(),
            env_from_secret,
            inline: None,
        }
    }

    pub fn for_inline(
        generated_path: PathBuf,
        project_name: &str,
        primary_service: &str,
        env_from_secret: HashMap<String, String>,
        stack: StackSpec,
    ) -> Self {
        Self {
            compose_file: generated_path,
            project_name: project_name.to_string(),
            primary_service: primary_service.to_string(),
            env_from_secret,
            inline: Some(stack),
        }
    }

    pub async fn ensure_started(&self) -> Result<(), DriverError> {
        if let Some(stack) = &self.inline {
            self.regenerate(stack)?;
        }

        let output = if self.service_exists().await? {
            self.run(&["start"]).await?
        } else {
            self.run(&["up", "-d"]).await?
        };
        Self::check_success("docker compose up/start", &output)
    }

    pub async fn stop(&self) -> Result<(), DriverError> {
        let output = self.run(&["stop"]).await?;
        Self::check_success("docker compose stop", &output)
    }

    /// Full teardown. Intentionally **not** called from the reaper/proxy —
    /// cooldown always means `stop`, so containers/networks stick around for
    /// a fast restart. Exposed for a future admin-triggered teardown action.
    #[allow(dead_code)]
    pub async fn down(&self) -> Result<(), DriverError> {
        let output = self.run(&["down"]).await?;
        Self::check_success("docker compose down", &output)
    }

    async fn service_exists(&self) -> Result<bool, DriverError> {
        let output = self.run(&["ps", "-q", self.primary_service.as_str()]).await?;
        Ok(!output.stdout.is_empty())
    }

    fn regenerate(&self, stack: &StackSpec) -> Result<(), DriverError> {
        let services = stack
            .services
            .as_ref()
            .expect("validated at load time: inline stack_spec always has services");
        let document = generate_compose_document(stack, services);

        if let Some(parent) = self.compose_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents =
            serde_json::to_vec_pretty(&document).expect("generated compose document is always valid JSON");
        std::fs::write(&self.compose_file, contents)?;

        if !stack.env_vars.is_empty() {
            if let Some(parent) = self.compose_file.parent() {
                let contents: String = stack.env_vars.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
                std::fs::write(parent.join(".env"), contents)?;
            }
        }

        Ok(())
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, DriverError> {
        let secret_env = resolve_env_from_secret(&self.env_from_secret)?;

        let mut cmd = Command::new("docker");
        cmd.arg("compose").arg("-f").arg(&self.compose_file).arg("-p").arg(&self.project_name);

        if let Some(stack) = &self.inline {
            if !stack.env_vars.is_empty() {
                if let Some(parent) = self.compose_file.parent() {
                    cmd.arg("--env-file").arg(parent.join(".env"));
                }
            }
        }

        cmd.args(args)
            .envs(secret_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        Ok(cmd.output().await?)
    }

    fn check_success(command: &str, output: &std::process::Output) -> Result<(), DriverError> {
        if output.status.success() {
            Ok(())
        } else {
            Err(DriverError::ComposeFailed {
                command: command.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        }
    }
}

/// Builds the compose document (as JSON — a valid superset of YAML, so
/// `docker compose -f` accepts it regardless of the `.yml` extension used on
/// disk) for an inline `stack_spec.services` block.
///
/// Every network referenced by a service defaults to `external: true` (must
/// already exist) unless `stack.networks` explicitly marks it otherwise —
/// Compose requires every referenced network to be declared, and defaulting
/// to "must already exist" fails loud (a clear "network not found" from
/// `docker compose`) rather than silently creating an isolated network that
/// breaks connectivity to a shared one (e.g. the gateway's own network).
fn generate_compose_document(stack: &StackSpec, services: &HashMap<String, InlineComposeService>) -> Value {
    let mut networks_referenced: BTreeSet<&str> = BTreeSet::new();
    let mut compose_services = Map::new();

    for (name, svc) in services {
        for net in &svc.networks {
            networks_referenced.insert(net.as_str());
        }

        let mut entry = Map::new();
        entry.insert("image".to_string(), json!(svc.image));
        if let Some(container_name) = &svc.container_name {
            entry.insert("container_name".to_string(), json!(container_name));
        }
        if !svc.ports.is_empty() {
            entry.insert("ports".to_string(), json!(svc.ports));
        }
        if !svc.depends_on.is_empty() {
            entry.insert("depends_on".to_string(), json!(svc.depends_on));
        }
        if !svc.networks.is_empty() {
            entry.insert("networks".to_string(), json!(svc.networks));
        }
        compose_services.insert(name.clone(), Value::Object(entry));
    }

    let mut networks_section = Map::new();
    for net in networks_referenced {
        let external = stack.networks.get(net).map(|spec| spec.external).unwrap_or(true);
        networks_section.insert(net.to_string(), json!({ "external": external }));
    }

    let mut document = Map::new();
    if let Some(version) = &stack.compose_version {
        document.insert("version".to_string(), json!(version));
    }
    document.insert("services".to_string(), Value::Object(compose_services));
    if !networks_section.is_empty() {
        document.insert("networks".to_string(), Value::Object(networks_section));
    }

    Value::Object(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::InlineNetworkSpec;

    fn inline_service(image: &str, networks: &[&str]) -> InlineComposeService {
        InlineComposeService {
            image: image.to_string(),
            container_name: None,
            ports: vec![],
            depends_on: vec![],
            networks: networks.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn empty_stack_spec() -> StackSpec {
        StackSpec {
            compose_file: None,
            project_name: Some("brms".to_string()),
            compose_version: Some("3.8".to_string()),
            env_vars: HashMap::new(),
            services: None,
            networks: HashMap::new(),
        }
    }

    #[test]
    fn referenced_network_without_explicit_entry_defaults_to_external() {
        let stack = empty_stack_spec();
        let services = HashMap::from([("brms".to_string(), inline_service("gorules/brms:latest", &["proxy-network"]))]);

        let document = generate_compose_document(&stack, &services);
        let networks = document.get("networks").unwrap().as_object().unwrap();
        assert_eq!(networks["proxy-network"]["external"], json!(true));
    }

    #[test]
    fn explicit_network_override_is_respected() {
        let mut stack = empty_stack_spec();
        stack
            .networks
            .insert("gorules_network".to_string(), InlineNetworkSpec { external: false });
        let services = HashMap::from([("brms".to_string(), inline_service("gorules/brms:latest", &["gorules_network"]))]);

        let document = generate_compose_document(&stack, &services);
        let networks = document.get("networks").unwrap().as_object().unwrap();
        assert_eq!(networks["gorules_network"]["external"], json!(false));
    }

    #[test]
    fn services_section_includes_expected_fields() {
        let stack = empty_stack_spec();
        let mut brms = inline_service("gorules/brms:latest", &["gorules_network", "proxy-network"]);
        brms.container_name = Some("gorules-brms".to_string());
        brms.ports = vec!["3000".to_string()];
        brms.depends_on = vec!["postgres".to_string()];
        let services = HashMap::from([("brms".to_string(), brms)]);

        let document = generate_compose_document(&stack, &services);
        let brms_entry = &document["services"]["brms"];
        assert_eq!(brms_entry["image"], json!("gorules/brms:latest"));
        assert_eq!(brms_entry["container_name"], json!("gorules-brms"));
        assert_eq!(brms_entry["ports"], json!(["3000"]));
        assert_eq!(brms_entry["depends_on"], json!(["postgres"]));
    }

    #[test]
    fn no_networks_section_when_no_service_references_any() {
        let stack = empty_stack_spec();
        let services = HashMap::from([("brms".to_string(), inline_service("gorules/brms:latest", &[]))]);

        let document = generate_compose_document(&stack, &services);
        assert!(document.get("networks").is_none());
    }
}
