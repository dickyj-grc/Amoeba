//! Signed caller identity for apps. Next to the plain `x-amoeba-user`,
//! `x-amoeba-org` and `x-amoeba-roles` headers, the gateway sends
//! `x-amoeba-identity`: a short-lived EdDSA token with the same facts, for
//! one service (`aud`). An app that verifies it with the gateway's public key
//! cannot be fooled by a caller that reaches it without the gateway and sends
//! those plain headers itself.
//!
//! The signing key is a 32-byte Ed25519 seed kept in `AMOEBA_IDENTITY_KEY_FILE`
//! (default `/etc/amoeba/identity_key`), created on first use so tokens stay
//! valid across restarts. Every app container gets the public key as
//! `AMOEBA_IDENTITY_PUBLIC_KEY` (base64url, no padding) and its own service
//! name as `AMOEBA_SERVICE_NAME`.

use super::Claims;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

pub const IDENTITY_HEADER: &str = "x-amoeba-identity";
pub const PUBLIC_KEY_ENV: &str = "AMOEBA_IDENTITY_PUBLIC_KEY";
pub const SERVICE_NAME_ENV: &str = "AMOEBA_SERVICE_NAME";
pub const ISSUER: &str = "amoeba";
/// How long an identity token is valid. It is checked when the request
/// arrives, so this only has to cover the hop from the gateway to the app.
pub const TTL_SECS: u64 = 300;
const KEY_FILE_ENV: &str = "AMOEBA_IDENTITY_KEY_FILE";
const DEFAULT_KEY_FILE: &str = "/etc/amoeba/identity_key";

/// PKCS#8 v1 wrapping of an Ed25519 seed (RFC 8410), as jsonwebtoken expects.
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// What an identity token says.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityClaims {
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    pub roles: Vec<String>,
    /// The service the token is for.
    pub aud: String,
    pub iss: String,
    pub iat: u64,
    pub exp: u64,
    /// The app acting for the caller, when the call came through one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

struct Keys {
    encoding: EncodingKey,
    public: [u8; 32],
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let path = std::env::var(KEY_FILE_ENV).unwrap_or_else(|_| DEFAULT_KEY_FILE.to_string());
        let seed = load_or_create_seed(std::path::Path::new(&path));
        let public = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        let mut der = PKCS8_PREFIX.to_vec();
        der.extend_from_slice(&seed);
        Keys {
            encoding: EncodingKey::from_ed_der(&der),
            public,
        }
    })
}

fn load_or_create_seed(path: &std::path::Path) -> [u8; 32] {
    if let Ok(bytes) = std::fs::read(path)
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return seed;
    }
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(path, seed));
    match saved {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
        // Still usable, but apps started before the next restart will hold
        // a key that no longer matches.
        Err(e) => warn!(
            "cannot keep the identity signing key at {}: {e}; using one for this run only",
            path.display()
        ),
    }
    seed
}

/// The gateway's public key, as apps receive it in [`PUBLIC_KEY_ENV`].
pub fn public_key() -> String {
    URL_SAFE_NO_PAD.encode(keys().public)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A token telling `service` who is calling.
pub fn issue(caller: &Claims, service: &str) -> Result<String, String> {
    let now = now();
    let claims = IdentityClaims {
        sub: caller.sub.clone(),
        org_id: caller.org_id.clone(),
        roles: caller.roles.clone(),
        aud: service.to_string(),
        iss: ISSUER.to_string(),
        iat: now,
        exp: now + TTL_SECS,
        via: caller.via.clone(),
    };
    encode(&Header::new(Algorithm::EdDSA), &claims, &keys().encoding)
        .map_err(|e| format!("failed to sign the identity token: {e}"))
}

/// Verifies `token` for `service` with `public_key` (as in
/// [`PUBLIC_KEY_ENV`]). What an app runs; amoeba-db has its own copy.
pub fn verify(token: &str, public_key: &str, service: &str) -> Result<IdentityClaims, String> {
    let key = URL_SAFE_NO_PAD
        .decode(public_key.trim())
        .map_err(|e| format!("invalid public key: {e}"))?;
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_audience(&[service]);
    validation.set_issuer(&[ISSUER]);
    decode::<IdentityClaims>(token, &DecodingKey::from_ed_der(&key), &validation)
        .map(|d| d.claims)
        .map_err(|e| format!("invalid identity token: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller() -> Claims {
        Claims {
            sub: "alice".into(),
            org_id: Some("acme".into()),
            roles: vec!["analyst".into()],
            exp: 0,
            jti: "j".into(),
            ..Default::default()
        }
    }

    // @lat: [[gateway#Signed identity for apps#Apps verify the gateway's signature]]
    #[test]
    fn apps_verify_the_gateways_signature_for_their_own_name() {
        let token = issue(&caller(), "finance-db").unwrap();
        let claims = verify(&token, &public_key(), "finance-db").unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.org_id.as_deref(), Some("acme"));
        assert_eq!(claims.roles, ["analyst"]);
        assert!(claims.exp <= now() + TTL_SECS);

        // Meant for another service: refused there.
        assert!(verify(&token, &public_key(), "other").is_err());
        // Signed by anyone else: refused.
        let other_key = URL_SAFE_NO_PAD.encode([7u8; 32]);
        assert!(verify(&token, &other_key, "finance-db").is_err());
        // An HMAC token a caller made up: refused.
        let forged = encode(
            &Header::default(),
            &serde_json::json!({"sub": "mallory", "roles": ["admin"], "aud": "finance-db", "iss": "amoeba", "iat": 0, "exp": now() + 60}),
            &EncodingKey::from_secret(b"guess"),
        )
        .unwrap();
        assert!(verify(&forged, &public_key(), "finance-db").is_err());
    }

    #[test]
    fn the_seed_is_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity_key");
        let first = load_or_create_seed(&path);
        assert_eq!(load_or_create_seed(&path), first);
    }
}
