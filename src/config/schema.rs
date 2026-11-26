//! Deserialized shape of services.json and config.toml.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UpstreamAuth {
    pub r#type: String, // "bearer_static" or "custom_header"
    pub token_env_var: Option<String>,
    pub header_name: Option<String>,
    /// `"<service>/<key>"` reference into the secrets directory, read on each
    /// request. Takes precedence over `token_env_var`. App packages set this
    /// so the gateway and the container share one installed secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_secret: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OperationRule {
    pub operation: String,
    /// `"http_method"`: `values` lists the methods. `"path"`: `path` is the
    /// pattern and `values` optionally narrows the methods (empty = any).
    #[serde(default = "default_match_type")]
    pub match_type: String,
    #[serde(default)]
    pub values: Vec<String>,
    /// Subpath pattern for `"path"` rules, e.g. `"loans/{id}"` or `"queries/**"`.
    /// `{name}` and `*` match one segment; a trailing `**` matches the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn default_match_type() -> String {
    "http_method".into()
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
pub fn parse_quantity_to_mb(s: &str) -> Result<u64, String> {
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

    s.parse::<u64>().map_err(|_| {
        format!("invalid quantity '{s}': expected a suffix like Gi/Mi/Ki/Ti or a bare number of MB")
    })
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
    /// OCI runtime passed to Docker (`HostConfig.runtime`). `None` uses the
    /// daemon default (`runc`). Set `runsc` for gVisor, or a Kata/Firecracker
    /// runtime name, only when that runtime is installed on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Resources this service consumes while running (its footprint, not a
    /// per-request cost). Omitting `resources` entirely means it requests
    /// nothing on any dimension, exempting it from capacity accounting.
    pub resources: Option<ContainerResources>,
    /// Non-sensitive environment variables injected into the container at
    /// start time. Added by the app-package manager for single-image apps.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Replaces the image's `CMD` (its entrypoint stays), like the arguments
    /// after the image in `docker run`. Empty keeps the image's own command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
}

/// How callers use a service, shown by the console after deploy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppInterface {
    /// `mcp`, `openapi`, `http` or `cdp`.
    pub r#type: InterfaceType,
    /// Where the API lives under the service, e.g. `/mcp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// One or two sentences on how to call it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// A request the console's Test dialog starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#try: Option<TryRequest>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceType {
    /// Model Context Protocol over HTTP.
    Mcp,
    /// A REST API with an OpenAPI document.
    Openapi,
    /// Plain HTTP.
    Http,
    /// Chrome DevTools Protocol over WebSocket.
    Cdp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TryRequest {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

impl AppInterface {
    pub fn validate(&self) -> Result<(), String> {
        for path in self.path.iter().chain(self.r#try.as_ref().map(|t| &t.path)) {
            if !path.starts_with('/') {
                return Err(format!("interface path '{path}' must start with '/'"));
            }
        }
        if let Some(t) = &self.r#try
            && !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&t.method.as_str())
        {
            return Err(format!(
                "interface.try.method '{}' is not an HTTP method",
                t.method
            ));
        }
        if let Some(docs) = &self.docs
            && !docs.starts_with("https://")
            && !docs.starts_with("http://")
        {
            return Err("interface.docs must be an http(s) URL".into());
        }
        Ok(())
    }
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
    ComposeFile {
        path: &'a str,
        project_name: &'a str,
    },
    Inline {
        project_name: &'a str,
    },
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
            _ => unreachable!(
                "validate_stack_spec_mode guarantees exactly one of compose_file/services"
            ),
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
    ///
    /// This is the single-policy map. Together with `tenant` it is the
    /// single-tenant form. Omit both `tenant` and `tenant_permissions` to keep
    /// the historical behavior: any org with a listed role may call.
    #[serde(default)]
    pub permissions: HashMap<String, Vec<String>>,
    /// When set, only this org may call, using `permissions`. Mutually
    /// exclusive with `tenant_permissions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Per-org permission maps. Each entry is a complete policy for that org
    /// (operation -> roles). Mutually exclusive with `tenant` and `permissions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_permissions: Option<HashMap<String, HashMap<String, Vec<String>>>>,
    pub upstream_auth: Option<UpstreamAuth>,
    /// Skips JWT verification and the permission check entirely for this service
    /// when true. Defaults to false: auth is required unless explicitly opted out.
    #[serde(default)]
    pub public: bool,
    /// Ordered rules mapping method and subpath to an operation name such as
    /// `loans.create`. The first match wins; no match falls back to the
    /// HTTP-method mapping (`read`/`add`/`update`/`delete`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_rules: Option<Vec<OperationRule>>,
    /// env var name -> `"<service>/<key>"` secret reference, resolved at
    /// start-time by the driver and injected directly into the container/
    /// compose-subprocess environment. Never stored inline in the generated
    /// compose file or in this config.
    #[serde(default)]
    pub env_from_secret: HashMap<String, String>,
    /// Secret key -> `"<service>/<key>"` reference. The proxy reads the file
    /// on each request and sets `X-Amoeba-Secret-<key>`. The value is not
    /// placed in the container environment.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub header_from_secret: HashMap<String, String>,
    /// Forward requests as a local client: drop the browser's `Origin` and
    /// send `Host: localhost:<port>`. For upstreams that only accept local
    /// callers, such as MCP servers guarding against DNS rebinding.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_upstream: bool,
    /// How to call the service; from the app package, for the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<AppInterface>,
    /// Services this app calls for its caller, with the caller's identity
    /// (`x-amoeba-caller-token`). See [`crate::auth::delegation`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// How the app's containers are confined. Set for apps installed from a
    /// package (with defaults when the manifest says nothing); absent for
    /// services written by hand and apps installed before sandboxing, which
    /// keep running as before. See [`Sandbox`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Sandbox>,
    /// The MCP tools an admin approved, and any that changed since. Set for
    /// MCP apps once their tool list is first read. See [`ToolPins`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolPins>,
    /// Requests each caller may make per minute; `0` is unlimited. Unset
    /// uses the gateway's default (`AMOEBA_RATE_LIMIT_PER_MINUTE`, 600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<u32>,
    /// Where the app was installed from, so a reinstall can tell an upgrade
    /// of the same app from a different app taking its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AppSource>,
    /// Site settings layered over the app's manifest. Kept when the same app
    /// is reinstalled or upgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<AppOverrides>,
}

/// Confinement for an app's containers. The defaults suit an app an AI just
/// wrote: no Linux capabilities, a read-only root filesystem (with writable
/// `/tmp`), no root user, a process cap, and no outbound network. Each can be
/// relaxed in the manifest's `sandbox` block.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    /// Root filesystem read-only; `/tmp`, `/run` and `/var/tmp` stay writable.
    #[serde(default = "sandbox_true")]
    pub read_only_root: bool,
    /// Outbound connections beyond the gateway, e.g. to the internet or a
    /// database elsewhere. Off by default.
    #[serde(default)]
    pub egress: bool,
    /// Keep an image that runs as root as root. Off by default: such images
    /// run as user 65534 (nobody). Images with their own non-root user keep it.
    #[serde(default)]
    pub run_as_root: bool,
    /// Most processes (threads included) the container may run.
    #[serde(default = "sandbox_pids_limit")]
    pub pids_limit: u32,
    /// Linux capabilities added back after all are dropped, e.g.
    /// `NET_BIND_SERVICE` to listen on a port below 1024.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Other Docker networks to join, e.g. one holding the database the app
    /// uses. Each app otherwise has a network of its own, shared only with
    /// the gateway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<String>,
}

fn sandbox_true() -> bool {
    true
}

fn sandbox_pids_limit() -> u32 {
    256
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox {
            read_only_root: true,
            egress: false,
            run_as_root: false,
            pids_limit: sandbox_pids_limit(),
            capabilities: Vec::new(),
            networks: Vec::new(),
        }
    }
}

impl Sandbox {
    pub fn validate(&self) -> Result<(), String> {
        if !(16..=65_536).contains(&self.pids_limit) {
            return Err("sandbox.pids_limit must be between 16 and 65536".into());
        }
        if let Some(bad) = self
            .capabilities
            .iter()
            .find(|c| c.is_empty() || !c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'))
        {
            return Err(format!(
                "sandbox.capabilities: '{bad}' is not a capability name like NET_BIND_SERVICE"
            ));
        }
        if self
            .capabilities
            .iter()
            .any(|c| c == "ALL" || c == "SYS_ADMIN")
        {
            return Err("sandbox.capabilities may not add ALL or SYS_ADMIN".into());
        }
        if let Some(bad) = self.networks.iter().find(|n| {
            n.is_empty()
                || !n
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
        }) {
            return Err(format!("sandbox.networks: '{bad}' is not a network name"));
        }
        Ok(())
    }
}

/// An MCP app's tools as approved: name -> SHA-256 of the tool's definition
/// (description, input schema, annotations). A tool whose definition later
/// differs, or a new one, is listed in `changed` and cannot be called until
/// an admin approves the current list. This stops a server from quietly
/// rewording a tool to steer agents ("tool poisoning") or adding new ones.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
pub struct ToolPins {
    pub approved: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub changed: std::collections::BTreeMap<String, String>,
}

/// Where an installed app came from.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AppSource {
    /// `library` for the app library; `package` for an uploaded package,
    /// including the console's Docker Image, Git and Dockerfile tabs.
    pub kind: String,
    /// The library app's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The manifest's `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl AppSource {
    /// Same app, possibly another version.
    pub fn same_app(&self, other: &AppSource) -> bool {
        self.kind == other.kind && self.id == other.id
    }

    /// For messages, e.g. "the app library's lightpanda-mcp 1.0.0".
    pub fn describe(&self) -> String {
        let version = self
            .version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        match (self.kind.as_str(), &self.id) {
            ("library", Some(id)) => format!("the app library's {id}{version}"),
            _ => format!("an uploaded package{version}"),
        }
    }
}

/// How a site runs an app, as opposed to what the app is: who may call it,
/// its memory and CPU, and when it idles. Set at install or later, and
/// applied over the manifest. Unset fields keep the manifest's value.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<HashMap<String, Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    /// e.g. `512Mi`, `2Gi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u64>,
    /// Idle seconds before the app is stopped; `0` keeps it running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_seconds: Option<u64>,
    /// Requests each caller may make per minute; `0` is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<u32>,
}

impl AppOverrides {
    pub fn is_empty(&self) -> bool {
        self == &AppOverrides::default()
    }

    /// Fields set in `newer` replace this one's.
    pub fn merge(&mut self, newer: AppOverrides) {
        if newer.permissions.is_some() {
            self.permissions = newer.permissions;
        }
        if newer.public.is_some() {
            self.public = newer.public;
        }
        if newer.memory.is_some() {
            self.memory = newer.memory;
        }
        if newer.cpu_cores.is_some() {
            self.cpu_cores = newer.cpu_cores;
        }
        if newer.cooldown_seconds.is_some() {
            self.cooldown_seconds = newer.cooldown_seconds;
        }
        if newer.rate_limit_per_minute.is_some() {
            self.rate_limit_per_minute = newer.rate_limit_per_minute;
        }
    }

    /// Writes the set fields into `service`.
    pub fn apply(&self, service: &mut ServiceConfig) -> Result<(), String> {
        if let Some(permissions) = &self.permissions {
            service.permissions = permissions.clone();
        }
        if let Some(public) = self.public {
            service.public = public;
        }
        if let Some(seconds) = self.cooldown_seconds {
            service.placement.cooldown_seconds = (seconds > 0).then_some(seconds);
        }
        if let Some(limit) = self.rate_limit_per_minute {
            service.rate_limit_per_minute = Some(limit);
        }
        if self.memory.is_none() && self.cpu_cores.is_none() {
            return Ok(());
        }
        let container = service
            .container
            .as_mut()
            .ok_or("memory and CPU can only be changed for single-image apps")?;
        let limits = &mut container
            .resources
            .get_or_insert_with(|| ContainerResources {
                limits: ResourceQuantities::default(),
            })
            .limits;
        if let Some(memory) = &self.memory {
            let mb = parse_quantity_to_mb(memory).map_err(|e| format!("invalid memory: {e}"))?;
            if mb == 0 {
                return Err("memory must be more than 0".into());
            }
            limits.memory = Some(Quantity(mb));
        }
        if let Some(cpu) = self.cpu_cores {
            limits.cpu_cores = (cpu > 0).then_some(cpu);
        }
        Ok(())
    }

    /// A service's current settings, every field set, for the console's form.
    pub fn current(service: &ServiceConfig) -> AppOverrides {
        let limits = service
            .container
            .as_ref()
            .and_then(|c| c.resources.as_ref())
            .map(|r| &r.limits);
        AppOverrides {
            permissions: Some(service.permissions.clone()),
            public: Some(service.public),
            memory: limits.and_then(|l| l.memory).map(|q| format!("{}Mi", q.0)),
            cpu_cores: limits.and_then(|l| l.cpu_cores),
            cooldown_seconds: Some(service.placement.cooldown_seconds.unwrap_or(0)),
            rate_limit_per_minute: Some(
                service
                    .rate_limit_per_minute
                    .unwrap_or_else(crate::routing::ratelimit::default_per_minute),
            ),
        }
    }
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
    /// When set, an install is rejected once this many services already name
    /// the same org via `tenant` or `tenant_permissions`. Unset means no cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_services_per_tenant: Option<u32>,
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
        assert_eq!(
            svc.env_from_secret.get("DB_PASSWORD").unwrap(),
            "gorules/db_password"
        );
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
