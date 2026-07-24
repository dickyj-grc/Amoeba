use amoeba::auth::jwt::local_engine;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::routing::admin::{create_user, delete_user, update_user};
use amoeba::routing::proxy::proxy_handler;
use amoeba::state::AppState;
use axum::{
    middleware,
    routing::{any, post},
    Router,
};
use std::sync::Arc;
use tracing::{info, warn};

#[tokio::main]
async fn main() {
    // Initialize JSON Tracing
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .json()
        .init();

    // Configure Authentication Mode
    let jwt_secret = std::env::var("AMOEBA_LOCAL_JWT_SECRET").unwrap_or_else(|_| {
        warn!("AMOEBA_LOCAL_JWT_SECRET not set; using an insecure default (do not use this in production)");
        "super_secret_local_key_change_in_production".to_string()
    });
    let jwt_engine = Arc::new(local_engine(jwt_secret));

    // Initialize App State & Dynamic File Watchers
    let app_state = AppState::new(
        "/etc/amoeba/services.json",
        "/etc/amoeba/users.json",
        jwt_engine,
    );

    // Admin-only user management routes: require both a valid JWT (outer layer,
    // applied below) and the "admin" role (route_layer, scoped to just these routes).
    let admin_routes = Router::new()
        .route("/users", post(create_user))
        .route("/users/:username", axum::routing::patch(update_user).delete(delete_user))
        .route_layer(middleware::from_fn(require_admin_role));

    // Build Router Pipeline
    let app = Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .nest("/admin", admin_routes)
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    info!("🚀 Amoeba Compute Orchestrator running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}
