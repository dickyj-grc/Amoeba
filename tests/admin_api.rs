//! End-to-end tests for the /admin/users API: proves both the CRUD behavior
//! and the access control (valid admin JWT required) actually work together.

use amoeba::auth::jwt::local_engine;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::auth::users::UserStore;
use amoeba::routing::admin::{create_user, delete_user, update_user};
use amoeba::routing::proxy::proxy_handler;
use amoeba::state::AppState;
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
    middleware,
    routing::{any, post},
    Router,
};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use serde_json::json;
use tower::ServiceExt;

const JWT_SECRET: &str = "test-admin-api-secret";

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

fn temp_users_file(label: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "amoeba-admin-api-test-{}-{label}.json",
            std::process::id()
        ))
        .to_str()
        .unwrap()
        .to_string()
}

fn build_app(users_file: &str) -> Router {
    let jwt_engine = std::sync::Arc::new(local_engine(JWT_SECRET));
    let state = AppState::new("/nonexistent/services.json", users_file, jwt_engine);

    let admin_routes = Router::new()
        .route("/users", post(create_user))
        .route(
            "/users/:username",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route_layer(middleware::from_fn(require_admin_role));

    Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .nest("/admin", admin_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            unified_auth_middleware,
        ))
        .with_state(state)
}

fn json_request(method: &str, uri: &str, token: Option<&str>, body: serde_json::Value) -> Request<Body> {
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
async fn rejects_admin_requests_without_a_token() {
    let path = temp_users_file("no-token");
    let app = build_app(&path);

    let req = json_request(
        "POST",
        "/admin/users",
        None,
        json!({"username": "x", "password": "y", "roles": ["viewer"], "org_id": "org"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_admin_requests_from_non_admin_roles() {
    let path = temp_users_file("non-admin");
    let app = build_app(&path);
    let token = token_with_roles(&["viewer"]);

    let req = json_request(
        "POST",
        "/admin/users",
        Some(&token),
        json!({"username": "x", "password": "y", "roles": ["viewer"], "org_id": "org"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_can_create_a_user() {
    let path = temp_users_file("create");
    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);

    let req = json_request(
        "POST",
        "/admin/users",
        Some(&token),
        json!({"username": "newuser", "password": "hunter2", "roles": ["viewer"], "org_id": "org1"}),
    );

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let saved = UserStore::load(&path).unwrap();
    let user = saved.users.get("newuser").expect("user was persisted");
    assert_eq!(user.roles, vec!["viewer".to_string()]);
    assert_eq!(user.org_id, "org1");

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn creating_a_duplicate_user_returns_conflict() {
    let path = temp_users_file("duplicate");
    let token = token_with_roles(&["admin"]);
    let payload = json!({"username": "dupe", "password": "hunter2", "roles": ["viewer"], "org_id": "org1"});

    let app = build_app(&path);
    let first = app
        .oneshot(json_request("POST", "/admin/users", Some(&token), payload.clone()))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    let app = build_app(&path);
    let second = app
        .oneshot(json_request("POST", "/admin/users", Some(&token), payload))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn admin_can_update_a_users_roles_and_password() {
    let path = temp_users_file("update");
    let token = token_with_roles(&["admin"]);

    // Seed a user directly via the store so this test isolates the update path.
    let mut store = UserStore::default();
    store
        .add_user("dicky", "old-password", vec!["viewer".into()], "org_hq".into())
        .unwrap();
    store.save(&path).unwrap();
    let old_hash = UserStore::load(&path)
        .unwrap()
        .users
        .get("dicky")
        .unwrap()
        .password_hash
        .clone();

    let app = build_app(&path);
    let req = json_request(
        "PATCH",
        "/admin/users/dicky",
        Some(&token),
        json!({"password": "new-password", "roles": ["admin", "analyst"]}),
    );
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let updated = UserStore::load(&path).unwrap();
    let user = updated.users.get("dicky").unwrap();
    assert_eq!(user.roles, vec!["admin".to_string(), "analyst".to_string()]);
    assert_ne!(user.password_hash, old_hash);
    assert_eq!(user.org_id, "org_hq"); // untouched field stays as-is

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn updating_an_unknown_user_returns_not_found() {
    let path = temp_users_file("update-missing");
    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);

    let req = json_request(
        "PATCH",
        "/admin/users/ghost",
        Some(&token),
        json!({"roles": ["viewer"]}),
    );
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_with_no_fields_returns_bad_request() {
    let path = temp_users_file("update-empty");
    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);

    let req = json_request("PATCH", "/admin/users/dicky", Some(&token), json!({}));
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_can_delete_a_user() {
    let path = temp_users_file("delete");
    let token = token_with_roles(&["admin"]);

    let mut store = UserStore::default();
    store
        .add_user("dicky", "hunter2", vec!["admin".into()], "org_hq".into())
        .unwrap();
    store.save(&path).unwrap();

    let app = build_app(&path);
    let req = Request::builder()
        .method("DELETE")
        .uri("/admin/users/dicky")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let remaining = UserStore::load(&path).unwrap();
    assert!(!remaining.users.contains_key("dicky"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn deleting_an_unknown_user_returns_not_found() {
    let path = temp_users_file("delete-missing");
    let app = build_app(&path);
    let token = token_with_roles(&["admin"]);

    let req = Request::builder()
        .method("DELETE")
        .uri("/admin/users/ghost")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
