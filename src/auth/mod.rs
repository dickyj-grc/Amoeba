//! Unified JWT verification: local HMAC mode and remote JWKS mode.

pub mod jwks;
pub mod jwt;
pub mod middleware;
pub mod revocation;
pub mod users;

use jsonwebtoken::{DecodingKey, Validation, decode, decode_header};
use revocation::RevocationStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub org_id: Option<String>,
    pub roles: Vec<String>,
    pub exp: usize,
    pub jti: String,
}

#[derive(Clone)]
pub enum AuthMode {
    LocalJwt {
        secret: Vec<u8>,
    },
    Jwks {
        jwks_url: String,
        cached_keys: Arc<RwLock<jsonwebtoken::jwk::JwkSet>>,
    },
}

#[derive(Clone)]
pub struct JwtEngine {
    pub mode: AuthMode,
    pub validation: Validation,
    pub revocation_store: Option<RevocationStore>,
}

impl JwtEngine {
    pub async fn verify_token(&self, token: &str) -> Result<Claims, String> {
        let claims = match &self.mode {
            AuthMode::LocalJwt { secret } => {
                let key = DecodingKey::from_secret(secret);
                decode::<Claims>(token, &key, &self.validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("Local HMAC failed: {}", e))?
            }
            AuthMode::Jwks { cached_keys, .. } => {
                let header = decode_header(token).map_err(|e| e.to_string())?;
                let kid = header.kid.ok_or("Token missing 'kid' header")?;

                let keys = cached_keys.read().await;
                let jwk = keys.find(&kid).ok_or("Key ID not found in JWKS cache")?;

                // Restrict verification to the algorithm the key is actually
                // advertised for, rather than accepting any algorithm the
                // engine-wide validation list permits for this key's family.
                let mut validation = self.validation.clone();
                if let Some(alg) = jwk.common.key_algorithm {
                    if let Ok(alg) = alg.to_string().parse::<jsonwebtoken::Algorithm>() {
                        validation.algorithms = vec![alg];
                    }
                }

                let decoding_key = DecodingKey::from_jwk(jwk).map_err(|e| e.to_string())?;
                decode::<Claims>(token, &decoding_key, &validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("JWKS validation failed: {}", e))?
            }
        };

        if let Some(store) = &self.revocation_store {
            if store.is_revoked(&claims.jti).await {
                return Err("Token has been revoked".to_string());
            }
        }

        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn local_engine(secret: &str) -> JwtEngine {
        JwtEngine {
            mode: AuthMode::LocalJwt {
                secret: secret.as_bytes().to_vec(),
            },
            validation: Validation::default(),
            revocation_store: None,
        }
    }

    fn sample_claims() -> Claims {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .checked_add(Duration::from_secs(3600))
            .unwrap()
            .as_secs() as usize;

        Claims {
            sub: "user-1".into(),
            org_id: Some("org-1".into()),
            roles: vec!["admin".into()],
            exp,
            jti: "jti-1".into(),
        }
    }

    #[tokio::test]
    async fn verifies_valid_local_token() {
        let engine = local_engine("test-secret");
        let token = encode(
            &Header::default(),
            &sample_claims(),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap();

        let verified = engine.verify_token(&token).await.unwrap();
        assert_eq!(verified.sub, "user-1");
        assert_eq!(verified.roles, vec!["admin".to_string()]);
    }

    #[tokio::test]
    async fn rejects_token_signed_with_wrong_secret() {
        let engine = local_engine("test-secret");
        let token = encode(
            &Header::default(),
            &sample_claims(),
            &EncodingKey::from_secret(b"other-secret"),
        )
        .unwrap();

        assert!(engine.verify_token(&token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_malformed_token() {
        let engine = local_engine("test-secret");
        assert!(engine.verify_token("not-a-jwt").await.is_err());
    }
}
