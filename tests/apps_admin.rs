//! End-to-end tests for the /admin/apps API: install, list, and uninstall
//! Amoeba App Packages.

use amoeba::auth::jwt::local_engine;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::routing::apps_admin::{
    install_app, issue_service_token, list_apps, uninstall_app, update_app_policy,
};
use amoeba::state::AppState;
use axum::{
    Router,
    body::Body,
    extract::DefaultBodyLimit,
    http::{Request, StatusCode, header::AUTHORIZATION},
    middleware,
    routing::{delete, post},
};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use std::io::Write;
use tower::ServiceExt;

const JWT_SECRET: &str = "test-apps-admin-api-secret";

#[derive(Serialize, Deserialize)]
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

fn temp_catalog_path(label: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "amoeba-apps-admin-api-test-{}-{label}.json",
            std::process::id()
        ))
        .to_str()
        .unwrap()
        .to_string()
}

fn build_app(catalog_path: &str) -> Router {
    let jwt_engine = std::sync::Arc::new(local_engine(JWT_SECRET));
    let state = AppState::new(catalog_path, "/nonexistent/users.json", jwt_engine, None);

    let admin_routes = Router::new()
        .route("/apps", post(install_app).get(list_apps))
        .route(
            "/apps/:name",
            delete(uninstall_app).patch(update_app_policy),
        )
        .route("/apps/:name/token", post(issue_service_token))
        .route_layer(middleware::from_fn(require_admin_role))
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024));

    Router::new()
        .nest("/admin", admin_routes)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            unified_auth_middleware,
        ))
        .with_state(state)
}

fn sample_package_zip() -> Vec<u8> {
    let manifest = r#"
api_version: v1
name: test-app
version: 1.0.0
description: Test app
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
  cooldown_seconds: 60
permissions:
  read: [admin]
schema:
  secrets:
    TOKEN:
      description: API token
      required: true
"#;

    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        zip.start_file("amoeba.yaml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

fn multipart_install_request(token: Option<&str>, package: &[u8], values: &str) -> Request<Body> {
    let boundary = "----FormBoundary7MA4YWxkTrZu0gW";
    let mut body: Vec<u8> = Vec::new();
    let crlf = b"\r\n";

    body.extend_from_slice(format!("--{boundary}").as_bytes());
    body.extend_from_slice(crlf);
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"package\"; filename=\"test-app.amoeba.zip\"",
    );
    body.extend_from_slice(crlf);
    body.extend_from_slice(b"Content-Type: application/zip");
    body.extend_from_slice(crlf);
    body.extend_from_slice(crlf);
    body.extend_from_slice(package);
    body.extend_from_slice(crlf);

    body.extend_from_slice(format!("--{boundary}").as_bytes());
    body.extend_from_slice(crlf);
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"values\"");
    body.extend_from_slice(crlf);
    body.extend_from_slice(b"Content-Type: application/json");
    body.extend_from_slice(crlf);
    body.extend_from_slice(crlf);
    body.extend_from_slice(values.as_bytes());
    body.extend_from_slice(crlf);

    body.extend_from_slice(format!("--{boundary}--").as_bytes());
    body.extend_from_slice(crlf);

    let mut builder = Request::builder().method("POST").uri("/admin/apps").header(
        "content-type",
        format!("multipart/form-data; boundary={boundary}"),
    );
    if let Some(t) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn rejects_install_without_a_token() {
    let catalog = temp_catalog_path("no-token");
    let app = build_app(&catalog);
    let package = sample_package_zip();

    let req = multipart_install_request(None, &package, "{}");
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    std::fs::remove_file(&catalog).ok();
}

#[tokio::test]
async fn rejects_install_from_non_admin_role() {
    let catalog = temp_catalog_path("non-admin");
    let app = build_app(&catalog);
    let token = token_with_roles(&["viewer"]);
    let package = sample_package_zip();

    let req = multipart_install_request(Some(&token), &package, "{}");
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    std::fs::remove_file(&catalog).ok();
}

#[tokio::test]
async fn admin_can_install_list_and_uninstall_app() {
    let catalog = temp_catalog_path("full-flow");
    let token = token_with_roles(&["admin"]);
    let package = sample_package_zip();

    // Install
    let app = build_app(&catalog);
    let req = multipart_install_request(
        Some(&token),
        &package,
        r#"{"secrets":{"TOKEN":"secret123"}}"#,
    );
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // List
    let app = build_app(&catalog);
    let req = Request::builder()
        .method("GET")
        .uri("/admin/apps")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Uninstall
    let app = build_app(&catalog);
    let req = Request::builder()
        .method("DELETE")
        .uri("/admin/apps/test-app")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Cleanup
    let catalog_path = std::path::Path::new(&catalog);
    let config_dir = catalog_path.parent().unwrap();
    std::fs::remove_dir_all(config_dir.join("secrets")).ok();
    std::fs::remove_file(&catalog).ok();
}

fn tenant_package() -> Vec<u8> {
    let manifest = r#"
api_version: v1
name: tenant-app
app:
  type: image
  image: hello-world:latest
placement:
  port: 8080
tenant: org_client_alpha
permissions:
  read: [mcp:tenant-app, analyst]
"#;
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        zip.start_file("amoeba.yaml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

#[tokio::test]
async fn scoped_token_is_bound_to_the_service_tenant() {
    let catalog = temp_catalog_path("scoped-token");
    let admin = token_with_roles(&["admin"]);
    let app = build_app(&catalog);
    let res = app
        .oneshot(multipart_install_request(
            Some(&admin),
            &tenant_package(),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let app = build_app(&catalog);
    let req = Request::builder()
        .method("POST")
        .uri("/admin/apps/tenant-app/token")
        .header(AUTHORIZATION, format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["org_id"], "org_client_alpha");
    assert_eq!(json["role"], "mcp:tenant-app");

    let decoded = jsonwebtoken::decode::<TestClaims>(
        json["token"].as_str().unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .unwrap();
    assert_eq!(decoded.claims.org_id.as_deref(), Some("org_client_alpha"));
    assert_eq!(decoded.claims.roles, vec!["mcp:tenant-app".to_string()]);
    assert_eq!(decoded.claims.sub, "mcp:tenant-app");

    std::fs::remove_file(&catalog).ok();
}

#[tokio::test]
async fn policy_patch_switches_a_service_to_per_org_permissions() {
    let catalog = temp_catalog_path("policy-patch");
    let admin = token_with_roles(&["admin"]);
    let app = build_app(&catalog);
    let res = app
        .oneshot(multipart_install_request(
            Some(&admin),
            &tenant_package(),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let app = build_app(&catalog);
    let req = Request::builder()
        .method("PATCH")
        .uri("/admin/apps/tenant-app")
        .header(AUTHORIZATION, format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"tenant_permissions":{"org_client_alpha":{"read":["viewer"]},"org_client_beta":{"read":["analyst"]}}}"#,
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let saved = std::fs::read_to_string(&catalog).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let service = &parsed["services"]["tenant-app"];
    assert!(service.get("tenant").is_none());
    assert_eq!(
        service["tenant_permissions"]["org_client_beta"]["read"][0],
        "analyst"
    );
    assert!(service["tenant_permissions"]["org_client_alpha"]["add"].is_null());

    std::fs::remove_file(&catalog).ok();
}
