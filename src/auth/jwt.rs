//! Local HMAC JWT issuing and verification (standalone/offline mode).

use super::revocation::InMemoryRevocationStore;
use super::{AuthMode, Claims, JwtEngine};
use jsonwebtoken::{EncodingKey, Header, Validation, encode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Builds a JWT engine configured for local HMAC verification.
pub fn local_engine(secret: impl Into<Vec<u8>>) -> JwtEngine {
    JwtEngine {
        mode: AuthMode::LocalJwt {
            secret: secret.into(),
        },
        validation: Validation::default(),
        revocation_store: None,
    }
}

/// Builds a local HMAC engine with an in-memory revocation store.
pub fn local_engine_with_revocation(
    secret: impl Into<Vec<u8>>,
    revocation_store: InMemoryRevocationStore,
) -> JwtEngine {
    JwtEngine {
        mode: AuthMode::LocalJwt {
            secret: secret.into(),
        },
        validation: Validation::default(),
        revocation_store: Some(revocation_store),
    }
}

/// Computes a Unix timestamp `ttl` into the future.
pub fn exp_from_now(ttl: Duration) -> Result<usize, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .checked_add(ttl)
        .ok_or_else(|| "token expiry overflowed".to_string())
        .map(|d| d.as_secs() as usize)
}

impl JwtEngine {
    /// Issues a new locally-signed HMAC JWT for the given subject, roles, and org.
    /// `ttl` controls how long the token remains valid. A fresh UUIDv4 is used as
    /// the `jti` so the token can be individually revoked.
    pub fn issue_token(
        &self,
        sub: &str,
        org_id: Option<&str>,
        roles: &[String],
        ttl: Duration,
    ) -> Result<String, String> {
        let AuthMode::LocalJwt { secret } = &self.mode else {
            return Err("issue_token is only supported in local HMAC mode".to_string());
        };

        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .checked_add(ttl)
            .ok_or("token expiry overflowed")?
            .as_secs() as usize;

        let claims = Claims {
            sub: sub.to_string(),
            org_id: org_id.map(String::from),
            roles: roles.to_vec(),
            exp,
            jti: uuid::Uuid::new_v4().to_string(),
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .map_err(|e| format!("failed to issue token: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_local_jwt_engine() {
        let engine = local_engine("secret");
        assert!(matches!(engine.mode, AuthMode::LocalJwt { .. }));
    }

    #[tokio::test]
    async fn issues_and_verifies_a_local_token() {
        let engine = local_engine("secret");
        let token = engine
            .issue_token(
                "dicky",
                Some("org_hq"),
                &["admin".into()],
                Duration::from_secs(3600),
            )
            .unwrap();

        let claims = engine.verify_token(&token).await.unwrap();
        assert_eq!(claims.sub, "dicky");
        assert_eq!(claims.org_id, Some("org_hq".to_string()));
        assert_eq!(claims.roles, vec!["admin".to_string()]);
        assert!(!claims.jti.is_empty());
    }
}
