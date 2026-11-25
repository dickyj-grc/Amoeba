//! Tokens an app uses to call other services on behalf of the person or agent
//! that called it. An app whose manifest lists `calls: [lightpanda]` receives
//! `x-amoeba-caller-token` with every authenticated request: the caller's own
//! user, org and roles, usable only on the listed services, so each service
//! checks the original caller's roles, not the app's.
//!
//! Signed with a key the gateway generates at startup, in local and JWKS mode
//! alike (the gateway cannot sign with an identity provider's key). They live
//! at most an hour and never past the caller's own token, and carry the
//! caller's `jti`, so revoking the caller's token revokes them too. After a
//! gateway restart they stop verifying; the next request gets a fresh one.

use super::Claims;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand_core::{OsRng, RngCore};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Header an app reads its caller's delegated token from.
pub const CALLER_TOKEN_HEADER: &str = "x-amoeba-caller-token";
/// Key id marking a delegated token, so verification picks the gateway's key.
pub const KID: &str = "amoeba-delegated";
/// Longest lifetime of a delegated token, in seconds.
pub const MAX_TTL_SECS: usize = 60 * 60;

fn key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        key
    })
}

fn now() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as usize)
        .unwrap_or(0)
}

/// A token for `app` to call `services` as `caller`.
pub fn issue(caller: &Claims, app: &str, services: &[String]) -> Result<String, String> {
    let claims = Claims {
        sub: caller.sub.clone(),
        org_id: caller.org_id.clone(),
        roles: caller.roles.clone(),
        exp: caller.exp.min(now() + MAX_TTL_SECS),
        jti: caller.jti.clone(),
        svc: Some(services.to_vec()),
        via: Some(app.to_string()),
    };
    let header = Header {
        kid: Some(KID.to_string()),
        ..Header::new(Algorithm::HS256)
    };
    encode(&header, &claims, &EncodingKey::from_secret(key()))
        .map_err(|e| format!("failed to issue a delegated token: {e}"))
}

/// Verifies a token carrying [`KID`]; checks only signature and expiry, the
/// caller applies revocation and the service limit.
pub fn verify(token: &str) -> Result<Claims, String> {
    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(key()),
        &Validation::new(Algorithm::HS256),
    )
    .map(|d| d.claims)
    .map_err(|e| format!("delegated token failed: {e}"))?;
    if claims.svc.is_none() || claims.via.is_none() {
        return Err("delegated token has no service limit".into());
    }
    Ok(claims)
}

/// Whether `claims` may be used on `service`: tokens limited to some services
/// (delegated ones) only on those.
pub fn allows(claims: &Claims, service: &str) -> bool {
    claims
        .svc
        .as_ref()
        .is_none_or(|services| services.iter().any(|s| s == service))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(exp: usize) -> Claims {
        Claims {
            sub: "alice".into(),
            org_id: Some("acme".into()),
            roles: vec!["analyst".into()],
            exp,
            jti: "alice-jti".into(),
            ..Default::default()
        }
    }

    #[test]
    fn carries_the_caller_and_is_limited_to_the_listed_services() {
        let token = issue(&caller(now() + 30), "scraper", &["lightpanda".into()]).unwrap();
        let claims = verify(&token).unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.org_id.as_deref(), Some("acme"));
        assert_eq!(claims.roles, ["analyst"]);
        assert_eq!(claims.jti, "alice-jti", "revoked with the caller's token");
        assert_eq!(claims.via.as_deref(), Some("scraper"));
        assert!(allows(&claims, "lightpanda"));
        assert!(!allows(&claims, "finance-db"));
        assert!(
            allows(&caller(now() + 30), "anything"),
            "ordinary tokens are unlimited"
        );
    }

    #[test]
    fn never_outlives_the_caller_or_an_hour() {
        let short = verify(&issue(&caller(now() + 30), "a", &[]).unwrap()).unwrap();
        assert_eq!(short.exp, now() + 30);
        let long = verify(&issue(&caller(now() + 86_400), "a", &[]).unwrap()).unwrap();
        assert!(long.exp <= now() + MAX_TTL_SECS);
    }

    #[test]
    fn rejects_tokens_not_signed_by_the_gateway() {
        let forged = encode(
            &Header {
                kid: Some(KID.into()),
                ..Header::new(Algorithm::HS256)
            },
            &Claims {
                svc: Some(vec!["lightpanda".into()]),
                via: Some("scraper".into()),
                ..caller(now() + 30)
            },
            &EncodingKey::from_secret(b"guessed"),
        )
        .unwrap();
        assert!(verify(&forged).is_err());
    }
}
