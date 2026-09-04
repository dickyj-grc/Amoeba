//! Startup configuration from `config.toml`: bind address, auth mode, key
//! material, and state-file paths. Unlike `services.json` (hot-reloaded via
//! the file watcher), this file is read once at startup — it decides how the
//! server authenticates, which cannot change mid-process.

use serde::Deserialize;
use std::net::SocketAddr;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/amoeba/config.toml";
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_USERS_FILE: &str = "/etc/amoeba/users.json";
pub const DEFAULT_REVOCATION_FILE: &str = "/etc/amoeba/revoked_tokens.json";

pub const AUTH_MODE_LOCAL_JWT: &str = "local_jwt";
pub const AUTH_MODE_JWKS: &str = "jwks";

#[derive(Debug, Deserialize)]
pub struct AmoebaConfig {
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub auth: AuthSettings,
}

#[derive(Debug, Deserialize)]
pub struct ServerSettings {
    /// Address the HTTP listener binds to, e.g. "127.0.0.1:8080".
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    /// Parsed/kept for forward compatibility; only "standalone" exists today.
    pub mode: Option<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            mode: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthSettings {
    /// "local_jwt" (issue/verify HMAC tokens locally) or "jwks" (verify tokens
    /// from an external identity provider via its JWKS endpoint).
    #[serde(default = "default_auth_mode")]
    pub mode: String,
    /// Expected `iss` claim. When set, tokens without a matching issuer are
    /// rejected. Optional in both modes.
    pub issuer: Option<String>,
    /// Expected `aud` claim. When set, tokens without a matching audience are
    /// rejected. Optional in both modes.
    pub audience: Option<String>,
    /// JWKS endpoint URL. Required when mode = "jwks".
    pub jwks_url: Option<String>,
    /// HMAC signing secret for local JWT mode. Either a literal secret or
    /// "env:VAR_NAME" to read it from an environment variable at startup —
    /// the latter is strongly preferred so secrets never live on disk.
    /// Required when mode = "local_jwt".
    pub jwt_secret: Option<String>,
    /// Username/password store backing POST /auth/login. Local JWT mode only.
    #[serde(default = "default_users_file")]
    pub users_file: String,
    /// Revoked token IDs (`jti`) are persisted here so revocations survive
    /// process restarts instead of reviving revoked tokens.
    #[serde(default = "default_revocation_file")]
    pub revocation_file: String,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            mode: default_auth_mode(),
            issuer: None,
            audience: None,
            jwks_url: None,
            jwt_secret: None,
            users_file: default_users_file(),
            revocation_file: default_revocation_file(),
        }
    }
}

impl AmoebaConfig {
    /// Reads and validates `config.toml` at `path`.
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read config file '{path}': {e}"))?;
        let config: AmoebaConfig =
            toml::from_str(&content).map_err(|e| format!("failed to parse '{path}': {e}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        if let Some(mode) = &self.server.mode {
            if mode != "standalone" {
                return Err(format!(
                    "server.mode '{mode}' is not supported: only \"standalone\" exists today"
                ));
            }
        }

        self.server
            .bind_addr
            .parse::<SocketAddr>()
            .map_err(|e| format!("server.bind_addr '{}' is not a valid address: {e}", self.server.bind_addr))?;

        match self.auth.mode.as_str() {
            AUTH_MODE_LOCAL_JWT => {
                if self.auth.jwt_secret.is_none() {
                    return Err(
                        "auth.jwt_secret is required when auth.mode = \"local_jwt\"".to_string(),
                    );
                }
            }
            AUTH_MODE_JWKS => {
                if self.auth.jwks_url.as_deref().is_none_or(str::is_empty) {
                    return Err(
                        "auth.jwks_url is required when auth.mode = \"jwks\"".to_string(),
                    );
                }
            }
            other => {
                return Err(format!(
                    "auth.mode '{other}' is not supported: expected \"{AUTH_MODE_LOCAL_JWT}\" or \"{AUTH_MODE_JWKS}\""
                ));
            }
        }
        Ok(())
    }
}

impl AuthSettings {
    /// Resolves the HMAC signing secret, supporting "env:VAR_NAME" indirection.
    /// Fails when the referenced environment variable is unset — a missing
    /// secret must never silently degrade to a known default.
    pub fn resolve_jwt_secret(&self) -> Result<Vec<u8>, String> {
        let raw = self.jwt_secret.as_deref().ok_or_else(|| {
            "auth.jwt_secret is required when auth.mode = \"local_jwt\"".to_string()
        })?;

        if let Some(var) = raw.strip_prefix("env:") {
            std::env::var(var)
                .map(String::into_bytes)
                .map_err(|_| format!("environment variable '{var}' (referenced by auth.jwt_secret) is not set"))
        } else {
            Ok(raw.as_bytes().to_vec())
        }
    }
}

fn default_bind_addr() -> String {
    DEFAULT_BIND_ADDR.to_string()
}

fn default_auth_mode() -> String {
    AUTH_MODE_LOCAL_JWT.to_string()
}

fn default_users_file() -> String {
    DEFAULT_USERS_FILE.to_string()
}

fn default_revocation_file() -> String {
    DEFAULT_REVOCATION_FILE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_jwt_toml(extra: &str) -> String {
        format!(
            r#"
            [server]
            bind_addr = "127.0.0.1:9090"

            [auth]
            mode = "local_jwt"
            jwt_secret = "test-secret"
            {extra}
            "#
        )
    }

    #[test]
    fn parses_local_jwt_mode_with_defaults() {
        let config: AmoebaConfig = toml::from_str(&local_jwt_toml("")).unwrap();
        config.validate().unwrap();
        assert_eq!(config.server.bind_addr, "127.0.0.1:9090");
        assert_eq!(config.auth.mode, AUTH_MODE_LOCAL_JWT);
        assert_eq!(config.auth.users_file, DEFAULT_USERS_FILE);
        assert_eq!(config.auth.revocation_file, DEFAULT_REVOCATION_FILE);
        assert!(config.auth.issuer.is_none());
    }

    #[test]
    fn parses_jwks_mode_with_issuer_and_audience() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [auth]
            mode = "jwks"
            jwks_url = "https://auth.example.com/.well-known/jwks.json"
            issuer = "https://auth.example.com"
            audience = "amoeba-compute"
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.auth.jwks_url.as_deref(),
            Some("https://auth.example.com/.well-known/jwks.json")
        );
        assert_eq!(config.auth.issuer.as_deref(), Some("https://auth.example.com"));
    }

    #[test]
    fn defaults_bind_addr_and_auth_mode_when_omitted() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [auth]
            jwt_secret = "x"
            "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.server.bind_addr, DEFAULT_BIND_ADDR);
        assert_eq!(config.auth.mode, AUTH_MODE_LOCAL_JWT);
    }

    #[test]
    fn rejects_unknown_auth_mode() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [auth]
            mode = "oauth2"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_jwks_mode_without_url() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [auth]
            mode = "jwks"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_local_jwt_mode_without_secret() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [auth]
            mode = "local_jwt"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_bind_addr() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [server]
            bind_addr = "not-an-address"

            [auth]
            jwt_secret = "x"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_server_mode() {
        let config: AmoebaConfig = toml::from_str(
            r#"
            [server]
            mode = "cluster"

            [auth]
            jwt_secret = "x"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn resolves_literal_secret() {
        let mut auth = AuthSettings {
            mode: AUTH_MODE_LOCAL_JWT.into(),
            issuer: None,
            audience: None,
            jwks_url: None,
            jwt_secret: Some("literal-secret".into()),
            users_file: DEFAULT_USERS_FILE.into(),
            revocation_file: DEFAULT_REVOCATION_FILE.into(),
        };
        assert_eq!(auth.resolve_jwt_secret().unwrap(), b"literal-secret");

        auth.jwt_secret = Some("env:AMOEBA_TEST_JWT_SECRET_RESOLVE".into());
        // SAFETY: no other test in this binary reads or writes this variable;
        // it exists only to exercise the "env:" indirection.
        unsafe { std::env::set_var("AMOEBA_TEST_JWT_SECRET_RESOLVE", "from-env") };
        assert_eq!(auth.resolve_jwt_secret().unwrap(), b"from-env");

        auth.jwt_secret = Some("env:AMOEBA_TEST_JWT_SECRET_MISSING".into());
        assert!(auth.resolve_jwt_secret().is_err());
    }

    #[test]
    fn example_config_file_is_valid() {
        let config = AmoebaConfig::load("config/config.example.toml")
            .expect("config/config.example.toml must parse and validate");
        assert_eq!(config.auth.mode, AUTH_MODE_JWKS);
    }
}
