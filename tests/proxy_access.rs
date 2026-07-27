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
    jti: String,
}

fn token_with_roles(roles: &[&str]) -> String {
    let claims = TestClaims {
        sub: "tester".into(),
        org_id: None,
        roles: roles.iter().map(|r| r.to_string()).collect(),
        exp: 9_999_999_999,
        jti: "test-jti".into(),
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

/// Writes a single-service catalog. `image: "does-not-exist-in-this-test"`
/// means the container driver's cold-boot always fails (no such local image),
/// so a request that gets *past* auth deterministically fails at the
/// driver-boot-check with 503 rather than needing a real backend.
fn write_catalog(path: &str, service: Value) {
    let catalog = json!({
        "version": 1,
        "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
        "services": { "svc": service }
    });
    std::fs::write(path, catalog.to_string()).unwrap();
}

fn container_service(extra: Value) -> Value {
    let mut base = json!({
        "placement": { "type": "vm", "ip": "127.0.0.1", "port": 1, "cooldown_seconds": 120 },
        "container": { "image": "does-not-exist-in-this-test" }
    });
    merge(&mut base, extra);
    base
}

fn merge(base: &mut Value, extra: Value) {
    if let Value::Object(extra_map) = extra {
        let base_map = base.as_object_mut().unwrap();
        for (k, v) in extra_map {
            base_map.insert(k, v);
        }
    }
}

fn build_app(services_file: &str) -> Router {
    // No real driver call should ever succeed against "does-not-exist-in-this-test",
    // but keep the readiness-poll budget tiny as a defensive bound anyway.
    unsafe { std::env::set_var("AMOEBA_READINESS_TIMEOUT_MS", "200") };

    let jwt_engine = std::sync::Arc::new(local_engine(JWT_SECRET));
    let state = AppState::new(services_file, "/nonexistent/users.json", jwt_engine, None);

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
    write_catalog(&path, container_service(json!({ "permissions": {"read": ["admin"]} })));

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
    write_catalog(&path, container_service(json!({})));

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
    write_catalog(&path, container_service(json!({ "permissions": {"read": ["admin"]} })));

    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);
    let res = app
        .oneshot(get_request("/v1/svc/health", Some(&token)))
        .await
        .unwrap();
    // Auth + ACL both passed; the only remaining failure is the driver-boot-check
    // (no such local image), proving we got all the way through the access-control
    // checks and reached the point where Amoeba tries to actually start the service.
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn public_service_without_token_skips_auth_entirely() {
    let path = temp_services_file("public-no-token");
    write_catalog(&path, container_service(json!({ "public": true })));

    let app = build_app(&path);
    let res = app.oneshot(get_request("/v1/svc/health", None)).await.unwrap();
    // Not 401: no token was required at all. The driver-boot-check failure (no
    // such local image) is the only reason this isn't a 2xx.
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn public_service_ignores_permissions_even_if_present() {
    let path = temp_services_file("public-with-permissions");
    write_catalog(
        &path,
        container_service(json!({ "public": true, "permissions": {"read": ["admin"]} })),
    );

    let app = build_app(&path);
    // No token, and no role could ever match "admin" anyway -- public still wins.
    let res = app.oneshot(get_request("/v1/svc/health", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn unknown_service_is_not_found_regardless_of_auth() {
    let path = temp_services_file("unknown-service");
    write_catalog(&path, container_service(json!({ "public": true })));

    let app = build_app(&path);
    let res = app
        .oneshot(get_request("/v1/does-not-exist/health", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    std::fs::remove_file(&path).ok();
}
