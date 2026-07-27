//! Deserialized shape of services.json and config.toml.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UpstreamAuth {
    pub r#type: String, // "bearer_static" or "custom_header"
    pub token_env_var: Option<String>,
    pub header_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OperationRule {
    pub operation: String,
    pub match_type: String, // "http_method"
    pub values: Vec<String>,
}

/// A resource quantity as written in `services.json`: either a Kubernetes-style
/// sized string (`"16Gi"`, `"512Mi"`, `"2Ki"`, `"1Ti"`) or a bare number, which is
/// interpreted as already being in MB (for backward compatibility with the old
/// flat `memory_mb`-style config and for fields with no natural unit).
/// Internally always normalized to MB (`u64`) — the same unit `ResourceSpec`
/// already uses, so `capacity/limiter.rs`'s budget math needs no changes at all.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub struct Quantity(pub u64);

impl<'de> Deserialize<'de> for Quantity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct QuantityVisitor;

        impl serde::de::Visitor<'_> for QuantityVisitor {
            type Value = Quantity;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a quantity string like \"16Gi\"/\"512Mi\", or a bare number of MB")
            }

            fn visit_u64<E>(self, v: u64) -> Result<Quantity, E> {
                Ok(Quantity(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Quantity, E>
            where
                E: serde::de::Error,
            {
                u64::try_from(v)
                    .map(Quantity)
                    .map_err(|_| E::custom("quantity cannot be negative"))
            }

            fn visit_str<E>(self, v: &str) -> Result<Quantity, E>
            where
                E: serde::de::Error,
            {
                parse_quantity_to_mb(v).map(Quantity).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(QuantityVisitor)
    }
}

/// Parses a Kubernetes-style IEC quantity string into MB. `Ki` rounds down to
/// the nearest whole MB (sub-MB precision isn't meaningful for the memory/GPU
/// budgets this is used for).
fn parse_quantity_to_mb(s: &str) -> Result<u64, String> {
    let s = s.trim();

    for (suffix, mb_per_unit) in [("Ti", 1_048_576u64), ("Gi", 1_024), ("Mi", 1)] {
        if let Some(num) = s.strip_suffix(suffix) {
            return num
                .trim()
                .parse::<u64>()
                .map(|n| n * mb_per_unit)
                .map_err(|_| format!("invalid quantity '{s}'"));
        }
    }

    if let Some(num) = s.strip_suffix("Ki") {
        return num
            .trim()
            .parse::<u64>()
            .map(|n| n / 1024)
            .map_err(|_| format!("invalid quantity '{s}'"));
    }

    s.parse::<u64>()
        .map_err(|_| format!("invalid quantity '{s}': expected a suffix like Gi/Mi/Ki/Ti or a bare number of MB"))
}

/// Named resource quantities as they appear in `services.json` (`memory`/`gpu_vram`
/// as sized strings, `cpu_cores` as a bare count). Converts into the internal
/// `ResourceSpec` — see that type's docs for what a missing field means on each
/// side (budget vs. request).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ResourceQuantities {
    pub memory: Option<Quantity>,
    pub cpu_cores: Option<u64>,
    pub gpu_vram: Option<Quantity>,
}

impl From<ResourceQuantities> for ResourceSpec {
    fn from(q: ResourceQuantities) -> Self {
        ResourceSpec {
            memory_mb: q.memory.map(|q| q.0),
            cpu_cores: q.cpu_cores,
            gpu_vram_mb: q.gpu_vram.map(|q| q.0),
        }
    }
}

/// Named resource quantities, already normalized to MB. Every field is
/// independently optional; see field docs on `ContainerSpec`/`MachineConfig`
/// for what `None` means on each side. This internal shape is untouched by the
/// `services.json` nesting/units above — it's what `capacity/limiter.rs`'s
/// budget math (`fits_within_budget`, `sum_resource_usage`) has always operated
/// on, and continues to.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ResourceSpec {
    pub memory_mb: Option<u64>,
    pub cpu_cores: Option<u64>,
    pub gpu_vram_mb: Option<u64>,
}

/// A capacity budget for a physical/virtual machine, referenced by name from
/// `Placement::machine` (or implied when it's the only entry in `machines` —
/// see `ServiceConfig::machine_name`). A `None` field on the resulting
/// `ResourceSpec` budget means that dimension is completely unconstrained (no
/// limit enforced), regardless of what services request.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MachineConfig {
    /// Only `"vm"` is supported today; validated at load time.
    pub r#type: String,
    /// Only `"docker"` is supported today; validated at load time.
    pub drivers: Vec<String>,
    pub resources: ResourceQuantities,
}

impl MachineConfig {
    pub fn budget(&self) -> ResourceSpec {
        self.resources.clone().into()
    }
}

/// Where and how a service is reachable, and how long it stays warm once
/// activated. Exactly one of `ip`/`primary_service` is populated (validated at
/// load time): `ip` for a single-`container` workload, `primary_service` (the
/// compose service name the gateway routes to) for a `stack_spec` workload.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Placement {
    /// Only `"vm"` is supported today; validated at load time.
    pub r#type: String,
    /// Upstream host for a `container` workload: a literal IP, or a Docker
    /// container/service name resolvable via Docker DNS when this service and
    /// the orchestrator share a network.
    pub ip: Option<String>,
    /// Which service inside a `stack_spec`'s compose project the gateway
    /// routes to, for a `stack_spec` workload.
    pub primary_service: Option<String>,
    pub port: u16,
    pub cooldown_seconds: Option<u64>,
    /// The entry in the top-level `machines` map this service's usage counts
    /// against. Optional: when omitted, implied to be the sole entry in
    /// `machines` (fails validation once a second machine exists — see
    /// `ServiceConfig::machine_name`).
    pub machine: Option<String>,
}

/// Scenario 1: Amoeba owns the full container lifecycle directly via the
/// Docker Engine API (create/start/stop) rather than shelling out to compose.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ContainerSpec {
    pub image: String,
    /// Resources this service consumes while running (its footprint, not a
    /// per-request cost). Omitting `resources` entirely means it requests
    /// nothing on any dimension, exempting it from capacity accounting.
    pub resources: Option<ContainerResources>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ContainerResources {
    pub limits: ResourceQuantities,
}

/// One service within an inline `stack_spec.services` block (Scenario 3).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct InlineComposeService {
    pub image: String,
    pub container_name: Option<String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub networks: Vec<String>,
}

/// Declares whether a network referenced by an inline compose service already
/// exists (`external: true`, the default) or should be created fresh by this
/// stack (`external: false`). Networks referenced by an inline service but not
/// listed here default to external — Compose requires every referenced
/// network to be declared, and defaulting to "must already exist" fails loud
/// (a clear "network not found" from `docker compose`) rather than silently
/// creating an isolated network that breaks connectivity to a shared one.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct InlineNetworkSpec {
    #[serde(default)]
    pub external: bool,
}

/// Scenarios 2 & 3: Amoeba drives a multi-container stack through the
/// `docker compose` CLI. Exactly one of `compose_file` (Scenario 2: a
/// pre-existing file a human wrote) or `services` (Scenario 3: inline,
/// Amoeba-authored — a compose file is generated from this once) is
/// populated; see `StackSpec::mode`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StackSpec {
    pub compose_file: Option<String>,
    /// The `docker compose -p` project name. Defaults to the service's own
    /// key in the `services` map when omitted (filled in by
    /// `config::watcher::normalize_catalog`).
    pub project_name: Option<String>,
    pub compose_version: Option<String>,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    pub services: Option<HashMap<String, InlineComposeService>>,
    #[serde(default)]
    pub networks: HashMap<String, InlineNetworkSpec>,
}

/// The two mutually-exclusive ways to drive a `StackSpec`, resolved after
/// `StackSpec::compose_file`/`services` exclusivity has already been
/// validated at load time (see `config::watcher::validate_stack_spec_mode`).
pub enum StackMode<'a> {
    ComposeFile { path: &'a str, project_name: &'a str },
    Inline { project_name: &'a str },
}

impl StackSpec {
    /// Panics if called before `validate_stack_spec_mode` has confirmed
    /// exclusivity and `normalize_catalog` has filled in `project_name` — both
    /// always run as part of `config::watcher::load_catalog`.
    pub fn mode(&self) -> StackMode<'_> {
        let project_name = self
            .project_name
            .as_deref()
            .expect("project_name defaulted by normalize_catalog before mode() is callable");

        match (&self.compose_file, &self.services) {
            (Some(path), None) => StackMode::ComposeFile { path, project_name },
            (None, Some(_)) => StackMode::Inline { project_name },
            _ => unreachable!("validate_stack_spec_mode guarantees exactly one of compose_file/services"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServiceConfig {
    pub placement: Placement,
    /// Scenario 1. Exactly one of `container`/`stack_spec` is populated
    /// (validated at load time — see `config::watcher::validate_workload_exclusivity`).
    pub container: Option<ContainerSpec>,
    /// Scenarios 2 & 3.
    pub stack_spec: Option<StackSpec>,
    /// operation -> roles allowed to perform it. Missing/empty means no role is
    /// granted access (fails closed) unless `public` is set.
    #[serde(default)]
    pub permissions: HashMap<String, Vec<String>>,
    pub upstream_auth: Option<UpstreamAuth>,
    /// Skips JWT verification and the permission check entirely for this service
    /// when true. Defaults to false: auth is required unless explicitly opted out.
    #[serde(default)]
    pub public: bool,
    /// operation_rules kept for backward compatibility with services that
    /// classify operations beyond the default HTTP-method mapping.
    pub operation_rules: Option<Vec<OperationRule>>,
    /// env var name -> `"<service>/<key>"` secret reference, resolved at
    /// start-time by the driver and injected directly into the container/
    /// compose-subprocess environment. Never stored inline in the generated
    /// compose file or in this config.
    #[serde(default)]
    pub env_from_secret: HashMap<String, String>,
}

impl ServiceConfig {
    /// The upstream host to proxy to: `placement.ip` for a `container`
    /// workload, `placement.primary_service` for a `stack_spec` workload.
    pub fn upstream_host(&self) -> &str {
        self.placement
            .ip
            .as_deref()
            .or(self.placement.primary_service.as_deref())
            .expect("validated at load time: exactly one of placement.ip/primary_service is set")
    }

    /// This service's declared resource footprint while running. Only a
    /// `container` workload can declare one; `stack_spec` workloads always
    /// return the zero/empty footprint (exempt from capacity accounting, the
    /// same semantics as a `container` workload that omits `resources`).
    pub fn resource_footprint(&self) -> ResourceSpec {
        self.container
            .as_ref()
            .and_then(|c| c.resources.as_ref())
            .map(|r| r.limits.clone().into())
            .unwrap_or_default()
    }

    /// The machine this service's usage counts against: `placement.machine`
    /// if explicit, else the sole entry in `catalog.machines` when there's
    /// exactly one (implied, per `Placement::machine`'s docs). `None` if
    /// ambiguous (only possible pre-validation, since `load_catalog` rejects
    /// an omitted `machine` once a second machine exists).
    pub fn machine_name<'a>(&'a self, catalog: &'a ServiceCatalog) -> Option<&'a str> {
        if let Some(machine) = &self.placement.machine {
            return Some(machine.as_str());
        }
        if catalog.machines.len() == 1 {
            return catalog.machines.keys().next().map(String::as_str);
        }
        None
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServiceCatalog {
    pub version: u32,
    #[serde(default)]
    pub machines: HashMap<String, MachineConfig>,
    /// Multi-machine scheduling/retry/cloud-burst-fallback configuration.
    /// Parsed so the file round-trips and stays forward-compatible, but never
    /// interpreted today — local dispatch is unconditional while there's only
    /// ever one machine/driver.
    #[serde(default)]
    pub scheduling: Option<serde_json::Value>,
    pub services: HashMap<String, ServiceConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_container_service(json_extra: &str) -> String {
        format!(
            r#"{{
                "version": 1,
                "machines": {{
                    "local": {{ "type": "vm", "drivers": ["docker"], "resources": {{ "memory": "16Gi" }} }}
                }},
                "services": {{
                    "gemma4": {{
                        "placement": {{ "type": "vm", "ip": "gemma4", "port": 11434, "cooldown_seconds": 180 }},
                        "container": {{ "image": "ollama/ollama:latest" }}
                        {json_extra}
                    }}
                }}
            }}"#
        )
    }

    #[test]
    fn parses_scenario_1_plain_container() {
        let json = minimal_container_service("");
        let catalog: ServiceCatalog = serde_json::from_str(&json).unwrap();
        let svc = catalog.services.get("gemma4").expect("gemma4 present");

        assert_eq!(svc.placement.port, 11434);
        assert_eq!(svc.placement.cooldown_seconds, Some(180));
        assert_eq!(svc.upstream_host(), "gemma4");
        assert!(svc.container.is_some());
        assert!(svc.stack_spec.is_none());
    }

    #[test]
    fn parses_scenario_1_with_resource_limits_as_quantities() {
        let json = r#"{
            "version": 1,
            "machines": {
                "local": { "type": "vm", "drivers": ["docker"], "resources": { "memory": "16Gi", "gpu_vram": "8Gi" } }
            },
            "services": {
                "gemma4": {
                    "placement": { "type": "vm", "ip": "gemma4", "port": 11434, "cooldown_seconds": 180 },
                    "container": {
                        "image": "ollama/ollama:latest",
                        "resources": { "limits": { "memory": "8Gi", "gpu_vram": "8Gi" } }
                    }
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("gemma4").unwrap();
        let footprint = svc.resource_footprint();
        assert_eq!(footprint.memory_mb, Some(8192));
        assert_eq!(footprint.gpu_vram_mb, Some(8192));

        let machine = catalog.machines.get("local").unwrap();
        let budget = machine.budget();
        assert_eq!(budget.memory_mb, Some(16384));
        assert_eq!(budget.gpu_vram_mb, Some(8192));
    }

    #[test]
    fn parses_scenario_2_compose_file() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "services": {
                "gorules": {
                    "placement": {
                        "type": "vm", "cooldown_seconds": 1800, "primary_service": "gorules", "port": 80
                    },
                    "stack_spec": {
                        "compose_file": "/etc/amoeba/stacks/gorules/docker-compose.yml",
                        "project_name": "gorules"
                    },
                    "env_from_secret": { "DB_PASSWORD": "gorules/db_password" }
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("gorules").unwrap();
        assert_eq!(svc.upstream_host(), "gorules");
        assert!(svc.container.is_none());
        let stack = svc.stack_spec.as_ref().unwrap();
        match stack.mode() {
            StackMode::ComposeFile { path, project_name } => {
                assert_eq!(path, "/etc/amoeba/stacks/gorules/docker-compose.yml");
                assert_eq!(project_name, "gorules");
            }
            StackMode::Inline { .. } => panic!("expected ComposeFile mode"),
        }
        assert_eq!(svc.env_from_secret.get("DB_PASSWORD").unwrap(), "gorules/db_password");
    }

    #[test]
    fn parses_scenario_3_inline_stack_spec() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "services": {
                "brms": {
                    "placement": {
                        "type": "vm", "cooldown_seconds": 1800, "primary_service": "brms", "port": 3000
                    },
                    "stack_spec": {
                        "project_name": "brms",
                        "compose_version": "3.8",
                        "env_vars": { "LOG_LEVEL": "info" },
                        "services": {
                            "brms": {
                                "image": "gorules/brms:latest",
                                "container_name": "gorules-brms",
                                "ports": ["3000"],
                                "depends_on": ["postgres"],
                                "networks": ["gorules_network", "proxy-network"]
                            },
                            "postgres": {
                                "image": "postgres:18.4",
                                "container_name": "gorules-postgres",
                                "networks": ["gorules_network"]
                            }
                        }
                    },
                    "env_from_secret": { "DB_PASSWORD": "brms/db_password" }
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("brms").unwrap();
        let stack = svc.stack_spec.as_ref().unwrap();
        assert_eq!(stack.services.as_ref().unwrap().len(), 2);
        match stack.mode() {
            StackMode::Inline { project_name } => assert_eq!(project_name, "brms"),
            StackMode::ComposeFile { .. } => panic!("expected Inline mode"),
        }
    }

    #[test]
    fn quantity_parses_gi_mi_ki_and_bare_numbers() {
        assert_eq!(parse_quantity_to_mb("16Gi").unwrap(), 16 * 1024);
        assert_eq!(parse_quantity_to_mb("512Mi").unwrap(), 512);
        assert_eq!(parse_quantity_to_mb("2048Ki").unwrap(), 2);
        assert_eq!(parse_quantity_to_mb("1Ti").unwrap(), 1024 * 1024);
        assert_eq!(parse_quantity_to_mb("4096").unwrap(), 4096);
        assert!(parse_quantity_to_mb("not-a-quantity").is_err());
    }

    #[test]
    fn machine_name_falls_back_to_sole_machine_when_omitted() {
        let json = minimal_container_service("");
        let catalog: ServiceCatalog = serde_json::from_str(&json).unwrap();
        let svc = catalog.services.get("gemma4").unwrap();
        assert_eq!(svc.machine_name(&catalog), Some("local"));
    }

    #[test]
    fn machine_name_uses_explicit_placement_machine_when_set() {
        let json = r#"{
            "version": 1,
            "machines": {
                "local": { "type": "vm", "drivers": ["docker"], "resources": {} },
                "other": { "type": "vm", "drivers": ["docker"], "resources": {} }
            },
            "services": {
                "gemma4": {
                    "placement": { "type": "vm", "ip": "gemma4", "port": 1, "machine": "other" },
                    "container": { "image": "x" }
                }
            }
        }"#;
        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("gemma4").unwrap();
        assert_eq!(svc.machine_name(&catalog), Some("other"));
    }

    #[test]
    fn machine_name_none_when_ambiguous_across_multiple_machines() {
        let json = r#"{
            "version": 1,
            "machines": {
                "local": { "type": "vm", "drivers": ["docker"], "resources": {} },
                "other": { "type": "vm", "drivers": ["docker"], "resources": {} }
            },
            "services": {
                "gemma4": {
                    "placement": { "type": "vm", "ip": "gemma4", "port": 1 },
                    "container": { "image": "x" }
                }
            }
        }"#;
        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("gemma4").unwrap();
        assert_eq!(svc.machine_name(&catalog), None);
    }

    #[test]
    fn scheduling_block_parses_and_is_ignored() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "scheduling": {
                "strategy": "best_fit",
                "retry": { "max_attempts": 3, "interval_seconds": 15 },
                "fallback": { "enabled": true, "provider": "aws-lambda", "region": "us-east-1" }
            },
            "services": {}
        }"#;
        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        assert!(catalog.scheduling.is_some());
    }

    #[test]
    fn scheduling_block_defaults_to_none_when_omitted() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "services": {}
        }"#;
        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        assert!(catalog.scheduling.is_none());
    }
}
