//! End-to-end tests for the /admin/apps API: install, list, and uninstall
//! Amoeba App Packages.

use amoeba::auth::jwt::{local_engine, local_engine_with_revocation};
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::auth::revocation::RevocationStore;
use amoeba::routing::apps_admin::{
    build_app_image, build_status, cancel_build, get_library_entry, install_app,
    install_from_library, issue_service_token, list_apps, list_library, list_service_tokens,
    revoke_service_token, uninstall_app, update_app_policy,
};
use amoeba::state::AppState;
use axum::{
    Router,
    body::Body,
    extract::DefaultBodyLimit,
    http::{Request, StatusCode, header::AUTHORIZATION},
    middleware,
    routing::{delete, get, post},
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
    build_app_with(catalog_path, None)
}

fn build_app_with(catalog_path: &str, revocation: Option<RevocationStore>) -> Router {
    let jwt_engine = std::sync::Arc::new(match &revocation {
        Some(store) => local_engine_with_revocation(JWT_SECRET, store.clone()),
        None => local_engine(JWT_SECRET),
    });
    let state = AppState::new(
        catalog_path,
        "/nonexistent/users.json",
        jwt_engine,
        revocation,
    );

    let admin_routes = Router::new()
        .route("/apps", post(install_app).get(list_apps))
        .route(
            "/apps/:name",
            delete(uninstall_app).patch(update_app_policy),
        )
        .route(
            "/apps/:name/build",
            post(build_app_image).get(build_status).delete(cancel_build),
        )
        .route("/library", get(list_library))
        .route("/library/:id", get(get_library_entry))
        .route("/library/:id/install", post(install_from_library))
        .route("/apps/:name/token", post(issue_service_token))
        .route("/apps/:name/tokens", get(list_service_tokens))
        .route("/apps/:name/tokens/:jti", delete(revoke_service_token))
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

fn admin_json(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(
            AUTHORIZATION,
            format!("Bearer {}", token_with_roles(&["admin"])),
        )
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(res: axum::response::Response) -> serde_json::Value {
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
}

// @lat: [[deploy#Service tokens#Lifetime, list and revoke]]
#[tokio::test]
async fn service_tokens_can_be_long_lived_listed_and_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("services.json");
    let catalog = catalog.to_str().unwrap();
    let revocation = RevocationStore::new();
    let app = || build_app_with(catalog, Some(revocation.clone()));

    let admin = token_with_roles(&["admin"]);
    let res = app()
        .oneshot(multipart_install_request(
            Some(&admin),
            &tenant_package(),
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // Longer than a year is refused before anything is signed.
    let res = app()
        .oneshot(admin_json(
            "POST",
            "/admin/apps/tenant-app/token",
            r#"{"ttl_seconds":40000000}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let thirty_days = 30 * 24 * 60 * 60;
    let res = app()
        .oneshot(admin_json(
            "POST",
            "/admin/apps/tenant-app/token",
            &format!(r#"{{"ttl_seconds":{thirty_days},"label":"Cursor on laptop"}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let issued = json_body(res).await;
    let jti = issued["jti"].as_str().unwrap().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expires_at = issued["expires_at"].as_u64().unwrap();
    assert!(expires_at >= now + thirty_days - 5 && expires_at <= now + thirty_days + 5);
    assert_eq!(issued["service"], "tenant-app");

    // The list has the metadata, never the token.
    let res = app()
        .oneshot(admin_json("GET", "/admin/apps/tenant-app/tokens", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = json_body(res).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["jti"], jti.as_str());
    assert_eq!(list[0]["label"], "Cursor on laptop");
    assert!(list[0].get("token").is_none());
    assert!(
        !std::fs::read_to_string(dir.path().join("issued_tokens.json"))
            .unwrap()
            .contains(issued["token"].as_str().unwrap())
    );

    let revoke_uri = format!("/admin/apps/tenant-app/tokens/{jti}");
    let res = app()
        .oneshot(admin_json("DELETE", &revoke_uri, ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert!(revocation.is_revoked(&jti).await);

    let res = app()
        .oneshot(admin_json("GET", "/admin/apps/tenant-app/tokens", ""))
        .await
        .unwrap();
    assert_eq!(json_body(res).await, serde_json::json!([]));
    let res = app()
        .oneshot(admin_json("DELETE", &revoke_uri, ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// @lat: [[deploy#Source builds#Live progress]]
#[tokio::test]
async fn build_status_reports_the_latest_build_and_cancel_needs_a_running_one() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("services.json");
    let catalog = catalog.to_str().unwrap();
    // One state for every request, as in the gateway, so builds are shared.
    let app = build_app(catalog);

    let res = app
        .clone()
        .oneshot(admin_json("GET", "/admin/apps/status-app/build", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .clone()
        .oneshot(admin_json("DELETE", "/admin/apps/status-app/build", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Not a Dockerfile, so docker (or its absence) fails it fast.
    let res = app
        .clone()
        .oneshot(admin_json(
            "POST",
            "/admin/apps/status-app/build",
            r#"{"source":"dockerfile","dockerfile":"NOT A DOCKERFILE"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let res = app
        .clone()
        .oneshot(admin_json("GET", "/admin/apps/status-app/build", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let status = json_body(res).await;
    assert_eq!(status["state"], "failed");
    assert!(status["step"].as_str().unwrap().starts_with("docker build"));
    assert!(
        status["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().starts_with("$ docker build"))
    );
    let res = app
        .oneshot(admin_json("DELETE", "/admin/apps/status-app/build", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

// @lat: [[deploy#App library#Install from the library]]
#[tokio::test]
async fn library_apps_are_listed_and_install_with_their_setup() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("services.json");
    let catalog = catalog.to_str().unwrap();
    let app = build_app(catalog);

    let res = app
        .clone()
        .oneshot(admin_json("GET", "/admin/library", ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = json_body(res).await;
    let lightpanda = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "lightpanda-mcp")
        .expect("lightpanda-mcp is in the library");
    assert_eq!(lightpanda["interface"]["type"], "mcp");
    assert_eq!(lightpanda["images"][0], "lightpanda/browser");
    assert_eq!(lightpanda["port"], 9222);

    let res = app
        .clone()
        .oneshot(admin_json(
            "POST",
            "/admin/library/lightpanda-mcp/install",
            r#"{"name":"browser"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(catalog).unwrap()).unwrap();
    let svc = &saved["services"]["browser"];
    assert_eq!(svc["container"]["image"], "lightpanda/browser:latest");
    assert_eq!(svc["container"]["command"][1], "mcp");
    assert_eq!(svc["local_upstream"], true);
    assert_eq!(svc["interface"]["path"], "/mcp");
    assert_eq!(svc["permissions"]["add"][1], "mcp:browser");

    let res = app
        .clone()
        .oneshot(admin_json(
            "POST",
            "/admin/library/no-such-app/install",
            "{}",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = app
        .oneshot(admin_json(
            "POST",
            "/admin/library/lightpanda-mcp/install",
            r#"{"name":"Bad Name"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
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

fn build_request(token: &str, name: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/admin/apps/{name}/build"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

// @lat: [[deploy#Source builds#Input checks]]
#[tokio::test]
async fn build_endpoint_is_admin_only_and_rejects_unsafe_input_before_running_anything() {
    let app = build_app(&temp_catalog_path("build"));
    let admin = token_with_roles(&["admin"]);
    let git = r#"{"source":"git","url":"https://github.com/acme/api.git"}"#;

    let res = app
        .clone()
        .oneshot(build_request(&token_with_roles(&["analyst"]), "api", git))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    for (name, body) in [
        ("Api", git),
        ("api", r#"{"source":"git","url":"file:///etc"}"#),
        (
            "api",
            r#"{"source":"git","url":"https://x/y.git","ref":"--upload-pack=x"}"#,
        ),
        ("api", r#"{"source":"dockerfile","dockerfile":""}"#),
        (
            "api",
            r#"{"source":"dockerfile","dockerfile":"FROM x","context_url":"ext::sh"}"#,
        ),
    ] {
        let res = app
            .clone()
            .oneshot(build_request(&admin, name, body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{name} {body}");
    }

    // An unknown source kind fails JSON parsing.
    let res = app
        .oneshot(build_request(&admin, "api", r#"{"source":"tarball"}"#))
        .await
        .unwrap();
    assert!(res.status().is_client_error());
}
