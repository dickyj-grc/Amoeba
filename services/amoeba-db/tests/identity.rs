//! The service trusts only a gateway-signed identity once it has the
//! gateway's key, so an app that reaches it directly cannot claim roles with
//! plain `x-amoeba-*` headers. No database is needed: the MCP tool list
//! depends only on the caller's roles.

use amoeba_db::db::Db;
use amoeba_db::defs::Definitions;
use amoeba_db::{IdentityKey, Service, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

const SEED: [u8; 32] = [42; 32];
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

fn public_key() -> String {
    URL_SAFE_NO_PAD.encode(
        ed25519_dalek::SigningKey::from_bytes(&SEED)
            .verifying_key()
            .to_bytes(),
    )
}

/// A token as the gateway signs it.
fn identity(roles: &[&str], aud: &str, seed: [u8; 32]) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut der = PKCS8_PREFIX.to_vec();
    der.extend_from_slice(&seed);
    encode(
        &Header::new(Algorithm::EdDSA),
        &json!({
            "sub": "alice", "org_id": "acme", "roles": roles,
            "aud": aud, "iss": "amoeba", "iat": now, "exp": now + 60
        }),
        &EncodingKey::from_ed_der(&der),
    )
    .unwrap()
}

fn app(with_key: bool) -> axum::Router {
    let defs = Definitions::parse(
        &json!({
            "queries": {
                "overdue": {
                    "mode": "read",
                    "sql": "SELECT 1 AS v",
                    "roles": ["analyst"],
                    "columns": { "v": { "default": "show" } }
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    // Never connected: these requests are answered before any query runs.
    let db = Db::new(
        "postgres://unused@127.0.0.1:1/x",
        "postgres://unused@127.0.0.1:1/x",
    )
    .unwrap();
    let key = with_key.then(|| IdentityKey::new(&public_key(), "finance-db").unwrap());
    router(Service::with_identity_key(db, defs, key))
}

/// Tool names the caller is shown, or the status when refused.
async fn tools(app: axum::Router, headers: &[(&str, &str)]) -> Result<Vec<String>, StatusCode> {
    let mut req = Request::post("/mcp").header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .oneshot(
            req.body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    if res.status() != StatusCode::OK {
        return Err(res.status());
    }
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    Ok(body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect())
}

#[tokio::test]
async fn forged_headers_get_nothing_once_the_gateway_key_is_set() {
    // Reaching the service directly and claiming a role: refused.
    let forged = [
        ("x-amoeba-user", "mallory"),
        ("x-amoeba-roles", "analyst,admin"),
    ];
    assert_eq!(
        tools(app(true), &forged).await,
        Err(StatusCode::UNAUTHORIZED)
    );

    // A gateway-signed identity: its roles count, the plain headers do not.
    let token = identity(&["analyst"], "finance-db", SEED);
    let signed = [
        ("x-amoeba-identity", token.as_str()),
        ("x-amoeba-roles", ""),
    ];
    assert_eq!(tools(app(true), &signed).await.unwrap(), ["query_overdue"]);

    // Signed for another service, or by another key: refused.
    let other_aud = identity(&["analyst"], "other-db", SEED);
    assert_eq!(
        tools(app(true), &[("x-amoeba-identity", other_aud.as_str())]).await,
        Err(StatusCode::UNAUTHORIZED)
    );
    let other_key = identity(&["analyst"], "finance-db", [7; 32]);
    assert_eq!(
        tools(app(true), &[("x-amoeba-identity", other_key.as_str())]).await,
        Err(StatusCode::UNAUTHORIZED)
    );
}

#[tokio::test]
async fn without_the_key_the_plain_headers_still_work() {
    // Older setups without AMOEBA_IDENTITY_PUBLIC_KEY keep working.
    let headers = [("x-amoeba-roles", "analyst")];
    assert_eq!(
        tools(app(false), &headers).await.unwrap(),
        ["query_overdue"]
    );
}
