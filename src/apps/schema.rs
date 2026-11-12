//! Deserialized shape of an Amoeba App Package manifest (`amoeba.yaml`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level manifest for an Amoeba App Package.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppManifest {
    pub api_version: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,

    /// How the application is packaged and run.
    pub app: AppSpec,

    /// Routing and lifecycle placement.
    #[serde(default)]
    pub placement: PlacementSpec,

    /// Role-based permissions, keyed by operation.
    /// Single-policy map. With `tenant`, this is the single-tenant form.
    #[serde(default)]
    pub permissions: HashMap<String, Vec<String>>,

    /// Bind the single-policy map to one org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,

    /// Per-org permission maps. Exclusive with `tenant` and `permissions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_permissions: Option<HashMap<String, HashMap<String, Vec<String>>>>,

    /// Skip JWT checks. Cannot be combined with `tenant` or `tenant_permissions`.
    #[serde(default)]
    pub public: bool,

    /// Credential the gateway sends to the app on every request, taken from
    /// one of the declared `schema.secrets`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_auth: Option<AppUpstreamAuth>,

    /// Named operations for `permissions`, matched by path or HTTP method.
    /// Same shape and validation as a catalog service's `operation_rules`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_rules: Vec<crate::config::schema::OperationRule>,

    /// Call the app as a local client (no `Origin`, `Host: localhost:<port>`),
    /// for apps that refuse browser or remote callers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_upstream: bool,

    /// How to call the app once deployed (protocol, path, docs, a sample
    /// request), shown by the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<crate::config::schema::AppInterface>,

    /// Optional resource limits for capacity gating.
    #[serde(default)]
    pub resources: Option<ResourceSpec>,

    /// Schema for user-supplied configuration values and secrets.
    #[serde(default)]
    pub schema: AppSchema,
}

/// The application payload: either a single published image or a Docker
/// Compose file.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppSpec {
    /// Run a single container from a published image. `command` replaces the
    /// image's `CMD`, so one image can be run in another mode without a build.
    Image {
        image: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
    },
    /// Run a multi-service stack from a compose file inside the package.
    Compose {
        compose_file: String,
        /// Which compose service Amoeba routes to. Defaults to the app name.
        #[serde(default)]
        primary_service: Option<String>,
    },
}

/// Gateway-to-app credential for apps that require auth on every request
/// (e.g. an MCP server that refuses to start without a token). The same
/// secret usually also has `inject: env` so the app knows what to expect.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppUpstreamAuth {
    /// `bearer_static` (`Authorization: Bearer <secret>`) or `custom_header`.
    pub r#type: String,
    /// Key of a secret declared in `schema.secrets`.
    pub secret: String,
    /// Header to set for `custom_header`.
    #[serde(default)]
    pub header_name: Option<String>,
}

/// Placement / routing configuration.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct PlacementSpec {
    pub port: u16,
    #[serde(default)]
    pub cooldown_seconds: Option<u64>,
    #[serde(default)]
    pub machine: Option<String>,
    /// OCI runtime for the single-container driver (`runsc`, `kata-runtime`, …).
    /// Unset uses the Docker daemon default. The named runtime must already be
    /// installed on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// Resource footprint of the app while running.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ResourceSpec {
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub cpu_cores: Option<u64>,
    #[serde(default)]
    pub gpu_vram: Option<String>,
}

/// Schema describing configurable inputs for the install form.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AppSchema {
    #[serde(default)]
    pub env: HashMap<String, EnvField>,
    #[serde(default)]
    pub secrets: HashMap<String, SecretField>,
}

/// A non-sensitive environment/config variable.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct EnvField {
    #[serde(default)]
    pub description: String,
    /// `string`, `number`, `integer` or `boolean`; install checks values against it.
    #[serde(default = "default_string")]
    pub r#type: String,
    /// Install fails when no value is given and there is no `default`.
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
    /// When set, the value must be one of these.
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

impl EnvField {
    /// Checks one value against `type` and `options`.
    pub fn check(&self, value: &str) -> Result<(), String> {
        let expected = match self.r#type.as_str() {
            "number" if !value.parse::<f64>().is_ok_and(f64::is_finite) => Some("a number"),
            "integer" if value.parse::<i64>().is_err() => Some("an integer"),
            "boolean" if !matches!(value, "true" | "false") => Some("true or false"),
            _ => None,
        };
        if let Some(expected) = expected {
            return Err(format!("must be {expected}"));
        }
        if let Some(options) = &self.options
            && !options.iter().any(|o| o == value)
        {
            return Err(format!("must be one of: {}", options.join(", ")));
        }
        Ok(())
    }

    /// The value the app gets when the installer leaves it empty.
    pub fn fallback(&self) -> Option<&str> {
        self.default.as_deref()
    }
}

pub const ENV_TYPES: &[&str] = &["string", "number", "integer", "boolean"];

/// A sensitive value that must be written to Amoeba's secrets directory.
/// The value can be supplied at install time via the `values` payload, or
/// pre-encrypted as an age file inside the package and decrypted by Amoeba
/// using a configured age identity.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SecretField {
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub validation_pattern: Option<String>,
    /// Path inside the package to an age-encrypted file containing the secret.
    /// When present, the file is decrypted at install time and the plaintext
    /// is written to Amoeba's secrets directory.
    #[serde(default)]
    pub file: Option<String>,
    /// Where the running service receives the value. `env` puts it in the
    /// container environment. `proxy` keeps it on the gateway and sends it as
    /// `X-Amoeba-Secret-<key>` on each request, so the process never sees it
    /// at rest in its own environment.
    #[serde(default)]
    pub inject: SecretInject,
}

/// How a secret is delivered to the service.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecretInject {
    /// Inject as a container environment variable at start.
    #[default]
    Env,
    /// Inject as a request header at the proxy. Not placed in the container env.
    Proxy,
}

fn default_string() -> String {
    "string".to_string()
}

fn default_true() -> bool {
    true
}

/// Values supplied by the user at install time, keyed by field name.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct InstallValues {
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub secrets: HashMap<String, String>,
}

/// One role grant or revoke against a service's access map.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PolicyGrant {
    /// Required when the service uses `tenant_permissions`. Must match `tenant`
    /// when the service is single-tenant.
    #[serde(default)]
    pub org: Option<String>,
    pub operation: String,
    pub role: String,
}

/// Partial update of a service's access policy. Structural fields replace the
/// corresponding catalog values. `grant` / `revoke` then edit one role.
/// The result is validated as a single access form before it is saved.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct PolicyPatch {
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub clear_tenant: bool,
    #[serde(default)]
    pub permissions: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub tenant_permissions: Option<HashMap<String, HashMap<String, Vec<String>>>>,
    #[serde(default)]
    pub clear_tenant_permissions: bool,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub grant: Option<PolicyGrant>,
    #[serde(default)]
    pub revoke: Option<PolicyGrant>,
}

/// Placeholder in `app.command` for `placement.port`, so the port is written
/// once. The app also gets it as the `PORT` env var.
pub const PORT_PLACEHOLDER: &str = "${PORT}";

/// Replaces [`PORT_PLACEHOLDER`] in each entry with `port`. Any other `${...}`
/// is an error, so a typo fails at install instead of reaching the app.
pub fn expand_command(command: &[String], port: u16) -> Result<Vec<String>, String> {
    command
        .iter()
        .map(|arg| {
            let expanded = arg.replace(PORT_PLACEHOLDER, &port.to_string());
            match expanded.find("${") {
                Some(at) => Err(format!(
                    "app.command: unknown placeholder in {arg:?} at {:?}; only {PORT_PLACEHOLDER} is supported",
                    &expanded[at..]
                )),
                None => Ok(expanded),
            }
        })
        .collect()
}

impl AppManifest {
    /// `app.command` with `${PORT}` filled in; empty for compose apps.
    pub fn start_command(&self) -> Result<Vec<String>, String> {
        match &self.app {
            AppSpec::Image { command, .. } => expand_command(command, self.placement.port),
            AppSpec::Compose { .. } => Ok(Vec::new()),
        }
    }

    /// The service key to use in Amoeba's `services.json`.
    pub fn service_name(&self) -> &str {
        &self.name
    }

    /// Whether this app runs from a compose file.
    pub fn is_compose(&self) -> bool {
        matches!(self.app, AppSpec::Compose { .. })
    }

    /// Whether this app runs from a single image.
    pub fn is_image(&self) -> bool {
        matches!(self.app, AppSpec::Image { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_compose_manifest() {
        let yaml = r#"
api_version: v1
name: pdf-inspector
version: 1.0.0
description: Inspect PDFs
author: Example Org
app:
  type: compose
  compose_file: compose.yaml
placement:
  port: 8080
  cooldown_seconds: 300
permissions:
  read: [admin, analyst]
resources:
  memory: 2Gi
schema:
  env:
    LOG_LEVEL:
      type: string
      default: info
  secrets:
    API_KEY:
      description: API key
      required: true
"#;
        let manifest: AppManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.name, "pdf-inspector");
        assert!(manifest.is_compose());
        assert_eq!(manifest.placement.port, 8080);
        assert_eq!(
            manifest.permissions.get("read").unwrap(),
            &["admin", "analyst"]
        );
        assert!(manifest.schema.secrets.contains_key("API_KEY"));
    }

    #[test]
    fn parses_image_manifest() {
        let yaml = r#"
api_version: v1
name: hello
app:
  type: image
  image: hello-world:latest
placement:
  port: 80
"#;
        let manifest: AppManifest = serde_yaml::from_str(yaml).unwrap();
        assert!(manifest.is_image());
        assert_eq!(manifest.placement.port, 80);
    }
}
