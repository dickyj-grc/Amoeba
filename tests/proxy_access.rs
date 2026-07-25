//! End-to-end tests for per-service access control: services default to requiring
//! a JWT + matching role, and can opt out entirely via "public": true.

use amoeba::auth::jwt::local_engine;
use amoeba::routing::proxy::proxy_handler;
use amoeba::state::AppState;
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    routing::any,
    Router,
};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use serde_json::{json, Value};
use tower::ServiceExt;

const JWT_SECRET: &str = "test-proxy-access-secret";

#[derive(Serialize)]
struct TestClaims {
    sub: String,
    org_id: Option<String>,
    roles: Vec<String>,
    exp: usize,
}

fn token_with_roles(roles: &[&str]) -> String {
    let claims = TestClaims {
        sub: "tester".into(),
        org_id: None,
        roles: roles.iter().map(|r| r.to_string()).collect(),
        exp: 9_999_999_999,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

fn temp_services_file(label: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "amoeba-proxy-access-test-{}-{label}.json",
            std::process::id()
        ))
        .to_str()
        .unwrap()
        .to_string()
}

/// Writes a single-service catalog. The service points at 127.0.0.1:1, a port
/// nothing listens on, so a request that gets *past* auth deterministically
/// fails the actual proxy attempt with 502 rather than hitting a real backend.
fn write_catalog(path: &str, service: Value) {
    let catalog = json!({ "services": { "svc": service } });
    std::fs::write(path, catalog.to_string()).unwrap();
}

fn build_app(services_file: &str) -> Router {
    let jwt_engine = std::sync::Arc::new(local_engine(JWT_SECRET));
    let state = AppState::new(services_file, "/nonexistent/users.json", jwt_engine);

    Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .with_state(state)
}

fn get_request(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn private_service_without_token_is_unauthorized() {
    let path = temp_services_file("private-no-token");
    write_catalog(
        &path,
        json!({"image": "x", "ip": "127.0.0.1", "port": 1, "permissions": {"read": ["admin"]}}),
    );

    let app = build_app(&path);
    let res = app.oneshot(get_request("/v1/svc/health", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn private_service_with_no_permissions_specified_fails_closed() {
    // "permissions" omitted entirely -> defaults to {} -> every role check fails ->
    // still requires a valid token, but denies everyone with 403, never lets the
    // request through unauthenticated.
    let path = temp_services_file("private-no-permissions");
    write_catalog(&path, json!({"image": "x", "ip": "127.0.0.1", "port": 1}));

    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);
    let res = app
        .oneshot(get_request("/v1/svc/health", Some(&token)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn private_service_with_matching_role_proceeds_past_auth() {
    let path = temp_services_file("private-matching-role");
    write_catalog(
        &path,
        json!({"image": "x", "ip": "127.0.0.1", "port": 1, "permissions": {"read": ["admin"]}}),
    );

    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);
    let res = app
        .oneshot(get_request("/v1/svc/health", Some(&token)))
        .await
        .unwrap();
    // Auth + ACL both passed; the only remaining failure is the unreachable
    // backend, proving we got all the way through the access-control checks.
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn public_service_without_token_skips_auth_entirely() {
    let path = temp_services_file("public-no-token");
    write_catalog(&path, json!({"image": "x", "ip": "127.0.0.1", "port": 1, "public": true}));

    let app = build_app(&path);
    let res = app.oneshot(get_request("/v1/svc/health", None)).await.unwrap();
    // Not 401: no token was required at all. The unreachable backend is the only
    // reason this isn't a 2xx.
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn public_service_ignores_permissions_even_if_present() {
    let path = temp_services_file("public-with-permissions");
    write_catalog(
        &path,
        json!({
            "image": "x",
            "ip": "127.0.0.1",
            "port": 1,
            "public": true,
            "permissions": {"read": ["admin"]}
        }),
    );

    let app = build_app(&path);
    // No token, and no role could ever match "admin" anyway -- public still wins.
    let res = app.oneshot(get_request("/v1/svc/health", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn unknown_service_is_not_found_regardless_of_auth() {
    let path = temp_services_file("unknown-service");
    write_catalog(&path, json!({"image": "x", "ip": "127.0.0.1", "port": 1, "public": true}));

    let app = build_app(&path);
    let res = app
        .oneshot(get_request("/v1/does-not-exist/health", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    std::fs::remove_file(&path).ok();
}
