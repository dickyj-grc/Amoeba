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
    #[serde(default)]
    pub permissions: HashMap<String, Vec<String>>,

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
    /// Run a single container from a published image.
    Image { image: String },
    /// Run a multi-service stack from a compose file inside the package.
    Compose {
        compose_file: String,
        /// Which compose service Amoeba routes to. Defaults to the app name.
        #[serde(default)]
        primary_service: Option<String>,
    },
}

/// Placement / routing configuration.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct PlacementSpec {
    pub port: u16,
    #[serde(default)]
    pub cooldown_seconds: Option<u64>,
    #[serde(default)]
    pub machine: Option<String>,
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
    #[serde(default = "default_string")]
    pub r#type: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

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

impl AppManifest {
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
        assert_eq!(manifest.permissions.get("read").unwrap(), &["admin", "analyst"]);
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
