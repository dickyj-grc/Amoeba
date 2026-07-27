//! End-to-end tests for declared machine-capacity gating: services on a
//! machine are admitted only while its declared budget isn't exceeded by
//! currently-occupying siblings plus the new request.
//!
//! All services here are marked "public" to keep these tests focused on
//! capacity behavior rather than auth (see tests/proxy_access.rs for that).
//!
//! A fresh activation that passes the capacity gate still has to go through
//! the driver-boot-check (start the container, then wait for it to accept
//! connections) before ever reaching a real proxy attempt. Every service here
//! uses a deliberately nonexistent image, so that check always fails --
//! turning what used to read as `502 Bad Gateway` ("passed every check, hit
//! an unreachable backend") into `503 Service Unavailable` instead. The
//! `x-amoeba-rejection-reason` response header disambiguates *why*: a
//! capacity-gate rejection is tagged `machine-capacity`, while "passed the
//! gate, driver just couldn't start it" is tagged `driver-start-failed` --
//! that distinction is what these tests actually assert on. A *second*
//! request to an already-warm service skips both checks entirely (nothing
//! left to verify) and reaches the real (unreachable) backend, which is where
//! a plain `502` still shows up.

use amoeba::auth::jwt::local_engine;
use amoeba::routing::proxy::proxy_handler;
use amoeba::state::AppState;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::any,
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;

const REJECTION_REASON_HEADER: &str = "x-amoeba-rejection-reason";

fn temp_services_file(label: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "amoeba-capacity-gating-test-{}-{label}.json",
            std::process::id()
        ))
        .to_str()
        .unwrap()
        .to_string()
}

fn write_catalog(path: &str, services: Value, machines: Value) {
    let catalog = json!({ "version": 1, "machines": machines, "services": services });
    std::fs::write(path, catalog.to_string()).unwrap();
}

fn build_app(services_file: &str) -> Router {
    unsafe { std::env::set_var("AMOEBA_READINESS_TIMEOUT_MS", "200") };

    let jwt_engine = std::sync::Arc::new(local_engine("test-capacity-gating-secret"));
    let state = AppState::new(services_file, "/nonexistent/users.json", jwt_engine, None);

    Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .with_state(state)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap()
}

fn rejection_reason(res: &axum::response::Response) -> Option<&str> {
    res.headers().get(REJECTION_REASON_HEADER).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn service_without_declared_resources_is_never_capacity_rejected() {
    let path = temp_services_file("no-resources");
    write_catalog(
        &path,
        json!({
            "svc": {
                "placement": { "type": "vm", "ip": "127.0.0.1", "port": 1, "cooldown_seconds": 120 },
                "container": { "image": "does-not-exist-in-this-test" },
                // no "resources" declared: requests nothing, exempt from accounting,
                // regardless of how tiny the (implied, sole) machine's budget is.
                "public": true
            }
        }),
        json!({ "local": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1 } } }),
    );

    let app = build_app(&path);
    let res = app.oneshot(get("/v1/svc/health")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Not "machine-capacity": it was admitted; the driver just couldn't start
    // the (nonexistent) image.
    assert_eq!(rejection_reason(&res), Some("driver-start-failed"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn first_request_exceeding_machine_budget_alone_is_rejected_with_503() {
    let path = temp_services_file("exceeds-alone");
    write_catalog(
        &path,
        json!({
            "svc": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 2000 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1000 } } }),
    );

    let app = build_app(&path);
    let res = app.oneshot(get("/v1/svc/health")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&res), Some("machine-capacity"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn first_request_within_machine_budget_passes_the_capacity_gate() {
    let path = temp_services_file("within-budget");
    write_catalog(
        &path,
        json!({
            "svc": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 500 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1000 } } }),
    );

    let app = build_app(&path);
    let res = app.oneshot(get("/v1/svc/health")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&res), Some("driver-start-failed"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn second_service_on_shared_machine_rejected_once_combined_usage_exceeds_budget() {
    let path = temp_services_file("shared-machine-combined");
    write_catalog(
        &path,
        json!({
            "svc-a": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 700 } }
                },
                "public": true
            },
            "svc-b": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 400 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1000 } } }),
    );

    let app = build_app(&path);

    let first = app.clone().oneshot(get("/v1/svc-a/health")).await.unwrap();
    // Admitted (passed capacity), svc-a now warm; the driver just fails to start it.
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&first), Some("driver-start-failed"));

    let second = app.clone().oneshot(get("/v1/svc-b/health")).await.unwrap();
    // svc-a (700) is still within its cooldown, so it counts as occupying;
    // 700 + svc-b's 400 = 1100 > 1000.
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&second), Some("machine-capacity"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn warm_stateful_sibling_always_counts_toward_machine_budget() {
    let path = temp_services_file("warm-stateful-sibling");
    write_catalog(
        &path,
        json!({
            "svc-a": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1"
                    // cooldown_seconds omitted -> None -> always occupying, no
                    // request to svc-a needed at all.
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 700 } }
                },
                "public": true
            },
            "svc-b": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 60
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    "resources": { "limits": { "memory": 400 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1000 } } }),
    );

    let app = build_app(&path);
    let res = app.oneshot(get("/v1/svc-b/health")).await.unwrap();
    // svc-a's static 700 (always occupying) + svc-b's 400 = 1100 > 1000.
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&res), Some("machine-capacity"));

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn second_request_to_already_warm_service_skips_both_gates_and_reaches_the_backend() {
    let path = temp_services_file("already-warm-repeat");
    write_catalog(
        &path,
        json!({
            "svc": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    // Exactly fills the budget alone.
                    "resources": { "limits": { "memory": 1000 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 1000 } } }),
    );

    let app = build_app(&path);

    let first = app.clone().oneshot(get("/v1/svc/health")).await.unwrap();
    // Admitted (passed capacity, exactly fills the budget); driver fails to start it.
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&first), Some("driver-start-failed"));

    let second = app.clone().oneshot(get("/v1/svc/health")).await.unwrap();
    // Not a fresh activation (still within cooldown) -> both the capacity gate
    // and the driver-boot-check are skipped entirely -> reaches the real
    // (unreachable) backend -> 502, never re-rejected with 503.
    assert_eq!(second.status(), StatusCode::BAD_GATEWAY);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn catalog_with_undefined_machine_reference_fails_to_load_and_service_returns_404() {
    let path = temp_services_file("undefined-machine");
    write_catalog(
        &path,
        json!({
            "svc": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "ghost-box"
                },
                "container": { "image": "does-not-exist-in-this-test" },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": {} } }),
    );

    let app = build_app(&path);
    let res = app.oneshot(get("/v1/svc/health")).await.unwrap();
    // load_catalog fails validation -> AppState falls back to an empty catalog
    // (same as any other malformed services.json) -> service simply doesn't exist.
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn service_with_machine_but_no_resources_is_always_admitted_and_does_not_shrink_sibling_budget() {
    let path = temp_services_file("no-resources-on-machine");
    write_catalog(
        &path,
        json!({
            "svc-a": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                // "resources" omitted entirely.
                "container": { "image": "does-not-exist-in-this-test" },
                "public": true
            },
            "svc-b": {
                "placement": {
                    "type": "vm", "ip": "127.0.0.1", "port": 1,
                    "machine": "gpu-box-1", "cooldown_seconds": 120
                },
                "container": {
                    "image": "does-not-exist-in-this-test",
                    // Exactly fills the budget alone -- proves svc-a contributed 0.
                    "resources": { "limits": { "memory": 500 } }
                },
                "public": true
            }
        }),
        json!({ "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": 500 } } }),
    );

    let app = build_app(&path);

    let first = app.clone().oneshot(get("/v1/svc-a/health")).await.unwrap();
    assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&first), Some("driver-start-failed"));

    let second = app.clone().oneshot(get("/v1/svc-b/health")).await.unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejection_reason(&second), Some("driver-start-failed"));

    std::fs::remove_file(&path).ok();
}
