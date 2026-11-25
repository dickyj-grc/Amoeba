//! Calls on behalf of the caller, end to end: a real gateway in front of a
//! "scraper" app that calls other services through the gateway with the token
//! it was given, as a Playwright or Stagehand app calls a hosted browser.
//!
//! Services have no idle stop, so no container driver is involved.

use amoeba::apps::state::AppLifecycleState;
use amoeba::auth::jwt::local_engine_with_revocation;
use amoeba::auth::revocation::RevocationStore;
use amoeba::routing::proxy::{proxy_handler, proxy_handler_root};
use amoeba::state::AppState;
use axum::{Router, extract::Query, http::HeaderMap, routing::any};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;

const JWT_SECRET: &str = "test-delegation-secret";
const CALLER_TOKEN: &str = "x-amoeba-caller-token";

#[derive(Serialize)]
struct TestClaims {
    sub: String,
    org_id: Option<String>,
    roles: Vec<String>,
    exp: usize,
    jti: String,
}

fn token(roles: &[&str], jti: &str) -> String {
    let claims = TestClaims {
        sub: "alice".into(),
        org_id: Some("acme".into()),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        exp: 9_999_999_999,
        jti: jti.into(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

/// Serves `app` on a free local port.
async fn serve(app: Router) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// A service that reports who the gateway said is calling.
fn whoami_app() -> Router {
    Router::new().fallback(|headers: HeaderMap| async move {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string()
        };
        format!(
            "user={} roles={} caller-token={}",
            get("x-amoeba-user"),
            get("x-amoeba-roles"),
            if headers.contains_key(CALLER_TOKEN) {
                "yes"
            } else {
                "no"
            }
        )
    })
}

/// The app acting for its caller: `GET /fetch?target=<service>` calls that
/// service through the gateway with the token it was given and reports the
/// answer; `GET /token` hands back the token itself.
fn scraper_app(gateway: u16) -> Router {
    Router::new()
        .route(
            "/token",
            any(|headers: HeaderMap| async move {
                headers
                    .get(CALLER_TOKEN)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string()
            }),
        )
        .route(
            "/fetch",
            any(
                move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    let Some(token) = headers.get(CALLER_TOKEN).and_then(|v| v.to_str().ok())
                    else {
                        return "no caller token".to_string();
                    };
                    let res = reqwest::Client::new()
                        .get(format!("http://127.0.0.1:{gateway}/v1/{}/", q["target"]))
                        .bearer_auth(token)
                        .send()
                        .await
                        .unwrap();
                    format!("{} {}", res.status().as_u16(), res.text().await.unwrap())
                },
            ),
        )
}

struct Gateway {
    port: u16,
    revocations: RevocationStore,
}

/// A gateway with `scraper` (calls `browser` and `admin-browser`), `browser`
/// (analysts may use it), `admin-browser` (admins only) and `other` (analysts
/// may use it, but the scraper did not declare it).
async fn spawn_gateway() -> Gateway {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = listener.local_addr().unwrap().port();
    let scraper = serve(scraper_app(gateway)).await;
    let whoami = serve(whoami_app()).await;

    let service = |port: u16, roles: &[&str]| {
        json!({
            "placement": { "type": "vm", "ip": "127.0.0.1", "port": port, "cooldown_seconds": null },
            "container": { "image": "not-started-in-this-test" },
            "permissions": { "read": roles }
        })
    };
    let mut scraper_service = service(scraper, &["analyst"]);
    scraper_service["calls"] = json!(["browser", "admin-browser"]);
    let dir = tempfile::tempdir().unwrap().keep();
    let catalog = dir.join("services.json");
    std::fs::write(
        &catalog,
        json!({
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "services": {
                "scraper": scraper_service,
                "browser": service(whoami, &["analyst"]),
                "admin-browser": service(whoami, &["admin"]),
                "other": service(whoami, &["analyst"]),
            }
        })
        .to_string(),
    )
    .unwrap();

    let revocations = RevocationStore::new();
    let state = AppState::new(
        catalog.to_str().unwrap(),
        "/nonexistent/users.json",
        Arc::new(local_engine_with_revocation(
            JWT_SECRET,
            revocations.clone(),
        )),
        Some(revocations.clone()),
    );
    for name in ["scraper", "browser", "admin-browser", "other"] {
        state.set_app_state(name, Arc::new(AppLifecycleState::ready()));
    }
    let router = Router::new()
        .route("/v1/:service_name/", any(proxy_handler_root))
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Gateway {
        port: gateway,
        revocations,
    }
}

async fn get(gateway: u16, path: &str, token: &str, extra: Option<(&str, &str)>) -> (u16, String) {
    let mut req = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{gateway}{path}"))
        .bearer_auth(token);
    if let Some((name, value)) = extra {
        req = req.header(name, value);
    }
    let res = req.send().await.unwrap();
    (res.status().as_u16(), res.text().await.unwrap())
}

// @lat: [[gateway#Calls for the caller#Downstream checks the caller]]
#[tokio::test]
async fn the_app_calls_other_services_as_its_caller() {
    let gw = spawn_gateway().await;
    let analyst = token(&["analyst"], "alice-1");

    // The browser sees alice and her roles, not the scraper.
    let (status, body) = get(gw.port, "/v1/scraper/fetch?target=browser", &analyst, None).await;
    assert_eq!(status, 200);
    assert_eq!(body, "200 user=alice roles=analyst caller-token=no");

    // A service that does not grant alice's role refuses her, through the
    // scraper as directly.
    let (_, body) = get(
        gw.port,
        "/v1/scraper/fetch?target=admin-browser",
        &analyst,
        None,
    )
    .await;
    assert!(body.starts_with("403"), "{body}");
}

// @lat: [[gateway#Calls for the caller#Only the declared services]]
#[tokio::test]
async fn the_token_works_only_on_the_declared_services() {
    let gw = spawn_gateway().await;
    let analyst = token(&["analyst"], "alice-2");

    // alice may use `other`, but the scraper did not declare it.
    let (status, _) = get(gw.port, "/v1/other/", &analyst, None).await;
    assert_eq!(status, 200);
    let (_, body) = get(gw.port, "/v1/scraper/fetch?target=other", &analyst, None).await;
    assert!(body.starts_with("403"), "{body}");
}

// @lat: [[gateway#Calls for the caller#Only for apps that declare calls]]
#[tokio::test]
async fn only_declaring_apps_get_a_token_and_callers_cannot_forge_one() {
    let gw = spawn_gateway().await;
    let analyst = token(&["analyst"], "alice-3");

    // `browser` declares no calls, and a token the caller sends is dropped.
    let (_, body) = get(
        gw.port,
        "/v1/browser/",
        &analyst,
        Some((CALLER_TOKEN, "forged")),
    )
    .await;
    assert!(body.ends_with("caller-token=no"), "{body}");

    // The scraper gets the gateway's token, not the caller's copy.
    let (_, issued) = get(
        gw.port,
        "/v1/scraper/token",
        &analyst,
        Some((CALLER_TOKEN, "forged")),
    )
    .await;
    assert_ne!(issued, "forged");
    assert!(!issued.is_empty());
}

// @lat: [[gateway#Calls for the caller#Revoked with the caller]]
#[tokio::test]
async fn revoking_the_callers_token_revokes_the_apps() {
    let gw = spawn_gateway().await;
    let analyst = token(&["analyst"], "alice-4");
    let (_, delegated) = get(gw.port, "/v1/scraper/token", &analyst, None).await;
    assert_eq!(get(gw.port, "/v1/browser/", &delegated, None).await.0, 200);

    gw.revocations.revoke("alice-4".into()).await;
    assert_eq!(get(gw.port, "/v1/browser/", &delegated, None).await.0, 401);
}
