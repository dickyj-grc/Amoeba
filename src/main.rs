mod auth;
mod capacity;
mod config;
mod lifecycle;
mod metering;
mod routing;
mod state;

use auth::jwt::local_engine;
use auth::middleware::unified_auth_middleware;
use axum::{middleware, routing::any, Router};
use routing::proxy::proxy_handler;
use state::AppState;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() {
    // Initialize JSON Tracing
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .json()
        .init();

    // Configure Authentication Mode
    let jwt_engine = Arc::new(local_engine(
        "super_secret_local_key_change_in_production",
    ));

    // Initialize App State & Dynamic File Watchers
    let app_state = AppState::new("/etc/amoeba/services.json", jwt_engine);

    // Build Router Pipeline
    let app = Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    info!("🚀 Amoeba Compute Orchestrator running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}
