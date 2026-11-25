//! WebSocket pass-through and query forwarding, end to end: a real gateway on a
//! local port in front of a real app, with real clients.
//!
//! The services have no idle stop, so the proxy treats them as already
//! running and never asks a container driver to start them. That keeps these
//! tests free of Docker while still covering auth, the role check, header
//! rewriting and connection counting.

use amoeba::apps::state::AppLifecycleState;
use amoeba::auth::jwt::local_engine;
use amoeba::routing::proxy::{proxy_handler, proxy_handler_root};
use amoeba::state::AppState;
use axum::{Router, routing::any};
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, Mutex, atomic::Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{
    self, Message, client::IntoClientRequest, handshake::server::Request as HandshakeRequest,
};

const JWT_SECRET: &str = "test-websocket-secret";

#[derive(Serialize)]
struct TestClaims {
    sub: String,
    org_id: Option<String>,
    roles: Vec<String>,
    exp: usize,
    jti: String,
}

fn token(roles: &[&str]) -> String {
    let claims = TestClaims {
        sub: "alice".into(),
        org_id: Some("acme".into()),
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

/// What the app saw in the handshake.
#[derive(Debug, Clone, Default)]
struct Seen {
    path_and_query: String,
    headers: Vec<(String, String)>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A WebSocket app that echoes every message and records its handshakes.
// tokio-tungstenite fixes the callback's error type, a full HTTP response.
#[allow(clippy::result_large_err)]
async fn spawn_echo_app() -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let record = record.clone();
            tokio::spawn(async move {
                let callback = |req: &HandshakeRequest, res| {
                    record.lock().unwrap().push(Seen {
                        path_and_query: req.uri().to_string(),
                        headers: req
                            .headers()
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                            .collect(),
                    });
                    Ok(res)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else {
                    return;
                };
                while let Some(Ok(message)) = ws.next().await {
                    if message.is_text() || message.is_binary() {
                        if ws.send(message).await.is_err() {
                            break;
                        }
                    } else if message.is_close() {
                        break;
                    }
                }
            });
        }
    });
    (port, seen)
}

/// A plain HTTP app that answers with the path and query it received.
async fn spawn_http_app() -> u16 {
    let app = Router::new().fallback(|uri: axum::http::Uri| async move { uri.to_string() });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// Starts the gateway on a local port with one service, `app`, on `app_port`.
async fn spawn_gateway(app_port: u16, extra: serde_json::Value) -> (u16, Arc<AppState>) {
    let mut service = json!({
        // No idle stop: treated as running, so no container driver is asked.
        "placement": { "type": "vm", "ip": "127.0.0.1", "port": app_port, "cooldown_seconds": null },
        "container": { "image": "not-started-in-this-test" },
        "permissions": { "read": ["browser-user"], "add": ["browser-user"] }
    });
    for (k, v) in extra.as_object().unwrap() {
        service[k] = v.clone();
    }
    let dir = tempfile::tempdir().unwrap().keep();
    let catalog = dir.join("services.json");
    std::fs::write(
        &catalog,
        json!({
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
            "services": { "app": service }
        })
        .to_string(),
    )
    .unwrap();

    let state = AppState::new(
        catalog.to_str().unwrap(),
        "/nonexistent/users.json",
        Arc::new(local_engine(JWT_SECRET)),
        None,
    );
    state.set_app_state("app", Arc::new(AppLifecycleState::ready()));
    let router = Router::new()
        .route("/v1/:service_name/", any(proxy_handler_root))
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (port, state)
}

fn ws_request(
    url: &str,
    token: Option<&str>,
    origin: Option<&str>,
) -> tungstenite::handshake::client::Request {
    let mut request = url.into_client_request().unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    request
}

fn active_connections(state: &AppState) -> u64 {
    state.runtime_states.load()["app"]
        .active_connections
        .load(Ordering::Relaxed)
}

/// Waits for a background tunnel task to notice a close.
async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

// @lat: [[gateway#WebSockets#Messages pass both ways]]
#[tokio::test]
async fn websocket_messages_pass_through_the_gateway() {
    let (app_port, seen) = spawn_echo_app().await;
    let (gateway, state) = spawn_gateway(app_port, json!({})).await;

    let url = format!("ws://127.0.0.1:{gateway}/v1/app/devtools/browser/1?session=abc");
    let (mut ws, response) =
        tokio_tungstenite::connect_async(ws_request(&url, Some(&token(&["browser-user"])), None))
            .await
            .expect("handshake through the gateway");
    assert_eq!(response.status(), 101);

    ws.send(Message::text("Page.navigate")).await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::text("Page.navigate")
    );
    ws.send(Message::binary(vec![0u8, 1, 2, 255]))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::binary(vec![0u8, 1, 2, 255])
    );

    // The app sees the path and query, the caller's verified identity, and
    // never the caller's token.
    let handshake = seen.lock().unwrap()[0].clone();
    assert_eq!(handshake.path_and_query, "/devtools/browser/1?session=abc");
    assert_eq!(handshake.header("x-amoeba-user"), Some("alice"));
    assert_eq!(handshake.header("x-amoeba-org"), Some("acme"));
    assert_eq!(handshake.header("authorization"), None);
    assert_eq!(
        handshake.header("host"),
        Some(format!("127.0.0.1:{app_port}").as_str())
    );

    // An open session keeps the app counted as busy, so it is never stopped
    // under the caller; closing it frees the slot.
    assert_eq!(active_connections(&state), 1);
    ws.close(None).await.unwrap();
    drop(ws);
    assert!(
        eventually(|| active_connections(&state) == 0).await,
        "the session was still counted after closing"
    );
}

// @lat: [[gateway#WebSockets#Same checks as any request]]
#[tokio::test]
async fn websocket_handshakes_need_a_token_and_a_role() {
    let (app_port, seen) = spawn_echo_app().await;
    let (gateway, _state) = spawn_gateway(app_port, json!({})).await;
    let url = format!("ws://127.0.0.1:{gateway}/v1/app/");

    let status = |result: Result<_, tungstenite::Error>| match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Ok(_) => 101,
        Err(e) => panic!("unexpected error: {e}"),
    };
    assert_eq!(
        status(tokio_tungstenite::connect_async(ws_request(&url, None, None)).await),
        401
    );
    assert_eq!(
        status(
            tokio_tungstenite::connect_async(ws_request(&url, Some(&token(&["viewer"])), None))
                .await
        ),
        403
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refused caller reached the app"
    );
}

// @lat: [[gateway#WebSockets#Local-only apps]]
#[tokio::test]
async fn local_upstream_websockets_look_local_to_the_app() {
    let (app_port, seen) = spawn_echo_app().await;
    let (gateway, _state) = spawn_gateway(app_port, json!({ "local_upstream": true })).await;
    let url = format!("ws://127.0.0.1:{gateway}/v1/app/");

    let (mut ws, _) = tokio_tungstenite::connect_async(ws_request(
        &url,
        Some(&token(&["browser-user"])),
        Some("https://console.example"),
    ))
    .await
    .unwrap();
    ws.send(Message::text("ping")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("ping"));

    // Chrome and Lightpanda refuse a non-local Host or a browser Origin.
    let handshake = seen.lock().unwrap()[0].clone();
    assert_eq!(
        handshake.header("host"),
        Some(format!("localhost:{app_port}").as_str())
    );
    assert_eq!(handshake.header("origin"), None);
}

// @lat: [[gateway#WebSockets#Refused upgrades]]
#[tokio::test]
async fn an_app_that_refuses_the_upgrade_answers_the_caller() {
    // A plain HTTP app answers the handshake with 200, not 101.
    let app_port = spawn_http_app().await;
    let (gateway, state) = spawn_gateway(app_port, json!({})).await;
    let url = format!("ws://127.0.0.1:{gateway}/v1/app/socket");

    match tokio_tungstenite::connect_async(ws_request(&url, Some(&token(&["browser-user"])), None))
        .await
    {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 200),
        other => panic!("expected the app's own answer, got {other:?}"),
    }
    assert!(eventually(|| active_connections(&state) == 0).await);
}

// @lat: [[gateway#Query strings reach the app]]
#[tokio::test]
async fn query_strings_reach_the_app() {
    let app_port = spawn_http_app().await;
    let (gateway, _state) = spawn_gateway(app_port, json!({})).await;

    let body = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{gateway}/v1/app/tables/loans?status=overdue&limit=5"
        ))
        .bearer_auth(token(&["browser-user"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "/tables/loans?status=overdue&limit=5");
}
