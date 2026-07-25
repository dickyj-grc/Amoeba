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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServiceConfig {
    pub image: String,
    /// Upstream host: a literal IP, or a Docker container/service name resolvable
    /// via Docker DNS when this service and the orchestrator share a network
    /// (see config/services.docker.example.json).
    pub ip: String,
    pub port: u16,
    pub cooldown_seconds: Option<u64>,
    pub operation_rules: Option<Vec<OperationRule>>,
    /// operation -> roles allowed to perform it. Missing/empty means no role is
    /// granted access (fails closed) unless `public` is set.
    #[serde(default)]
    pub permissions: HashMap<String, Vec<String>>,
    pub upstream_auth: Option<UpstreamAuth>,
    /// Skips JWT verification and the permission check entirely for this service
    /// when true. Defaults to false: auth is required unless explicitly opted out.
    #[serde(default)]
    pub public: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServiceCatalog {
    pub services: HashMap<String, ServiceConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_catalog() {
        let json = r#"{
            "services": {
                "gemma4": {
                    "image": "ollama/ollama:latest",
                    "ip": "192.168.100.20",
                    "port": 11434,
                    "cooldown_seconds": 60,
                    "operation_rules": null,
                    "permissions": {"read": ["admin"]},
                    "upstream_auth": null
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("gemma4").expect("gemma4 present");

        assert_eq!(svc.port, 11434);
        assert_eq!(svc.cooldown_seconds, Some(60));
        assert!(svc.upstream_auth.is_none());
        assert_eq!(svc.permissions.get("read").unwrap(), &vec!["admin".to_string()]);
    }

    #[test]
    fn rejects_catalog_missing_required_fields() {
        let json = r#"{"services": {"broken": {"image": "x"}}}"#;
        assert!(serde_json::from_str::<ServiceCatalog>(json).is_err());
    }

    #[test]
    fn public_defaults_to_false_when_omitted() {
        let json = r#"{
            "services": {
                "svc": {
                    "image": "x",
                    "ip": "127.0.0.1",
                    "port": 8080
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("svc").unwrap();
        assert!(!svc.public);
        assert!(svc.permissions.is_empty());
    }

    #[test]
    fn public_can_be_explicitly_set_without_permissions() {
        let json = r#"{
            "services": {
                "svc": {
                    "image": "x",
                    "ip": "127.0.0.1",
                    "port": 8080,
                    "public": true
                }
            }
        }"#;

        let catalog: ServiceCatalog = serde_json::from_str(json).unwrap();
        let svc = catalog.services.get("svc").unwrap();
        assert!(svc.public);
    }
}
