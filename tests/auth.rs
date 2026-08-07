//! End-to-end tests for local username/password login and token revocation.

use amoeba::auth::jwt::local_engine_with_revocation;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::auth::revocation::InMemoryRevocationStore;
use amoeba::auth::users::UserStore;
use amoeba::routing::auth::{login, revoke};
use amoeba::state::AppState;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header::AUTHORIZATION},
    middleware,
    routing::post,
};
use serde_json::json;
use tower::ServiceExt;

const JWT_SECRET: &str = "test-auth-secret";

fn temp_users_file(label: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "amoeba-auth-test-{}-{label}.json",
            std::process::id()
        ))
        .to_str()
        .unwrap()
        .to_string()
}

fn seed_user(path: &str) {
    let mut store = UserStore::default();
    store
        .add_user(
            "dicky",
            "hunter2",
            vec!["admin".into(), "analyst".into()],
            "org_hq".into(),
        )
        .unwrap();
    store.save(path).unwrap();
}

fn build_app(users_file: &str) -> Router {
    let revocation_store = InMemoryRevocationStore::new();
    let jwt_engine = std::sync::Arc::new(local_engine_with_revocation(
        JWT_SECRET,
        revocation_store.clone(),
    ));
    let state = AppState::new(
        "/nonexistent/services.json",
        users_file,
        jwt_engine,
        Some(revocation_store),
    );

    let revoke_route = Router::new()
        .route("/revoke", post(revoke))
        .route_layer(middleware::from_fn(require_admin_role))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            unified_auth_middleware,
        ));

    Router::new()
        .route("/login", post(login))
        .merge(revoke_route)
        .with_state(state)
}

fn json_request(
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn login_with_valid_credentials_returns_token() {
    let path = temp_users_file("login-ok");
    seed_user(&path);

    let app = build_app(&path);
    let req = json_request(
        "POST",
        "/login",
        None,
        json!({"username": "dicky", "password": "hunter2"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["token"].as_str().unwrap().starts_with("eyJ"));
    assert!(payload["expires_at"].as_u64().unwrap() > 0);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn login_with_invalid_password_returns_unauthorized() {
    let path = temp_users_file("login-bad-password");
    seed_user(&path);

    let app = build_app(&path);
    let req = json_request(
        "POST",
        "/login",
        None,
        json!({"username": "dicky", "password": "wrong-password"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn login_with_unknown_user_returns_unauthorized() {
    let path = temp_users_file("login-unknown-user");
    seed_user(&path);

    let app = build_app(&path);
    let req = json_request(
        "POST",
        "/login",
        None,
        json!({"username": "ghost", "password": "hunter2"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn admin_can_revoke_a_token() {
    let path = temp_users_file("revoke-ok");
    seed_user(&path);

    let app = build_app(&path);

    // 1. Log in to get a token.
    let login_res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/login",
            None,
            json!({"username": "dicky", "password": "hunter2"}),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(login_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let login_payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login_payload["token"].as_str().unwrap().to_string();

    // 2. Revoke it using the same token (user has admin role).
    let revoke_res = app
        .clone()
        .oneshot(json_request("POST", "/revoke", Some(&token), json!({})))
        .await
        .unwrap();
    assert_eq!(revoke_res.status(), StatusCode::NO_CONTENT);

    // 3. Re-using the token against the auth middleware now fails.
    let reuse_res = app
        .oneshot(json_request("POST", "/revoke", Some(&token), json!({})))
        .await
        .unwrap();
    assert_eq!(reuse_res.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn non_admin_cannot_revoke_a_token() {
    let path = temp_users_file("revoke-non-admin");
    let mut store = UserStore::default();
    store
        .add_user("viewer", "hunter2", vec!["viewer".into()], "org_hq".into())
        .unwrap();
    store.save(&path).unwrap();

    let app = build_app(&path);

    let login_res = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/login",
            None,
            json!({"username": "viewer", "password": "hunter2"}),
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(login_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let login_payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login_payload["token"].as_str().unwrap().to_string();

    let revoke_res = app
        .oneshot(json_request("POST", "/revoke", Some(&token), json!({})))
        .await
        .unwrap();
    assert_eq!(revoke_res.status(), StatusCode::FORBIDDEN);

    std::fs::remove_file(&path).ok();
}
