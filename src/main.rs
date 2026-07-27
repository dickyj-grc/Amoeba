use amoeba::auth::jwt::local_engine_with_revocation;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::auth::revocation::InMemoryRevocationStore;
use amoeba::routing::admin::{create_user, delete_user, update_user};
use amoeba::routing::auth::{login, revoke};
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
    let revocation_store = InMemoryRevocationStore::new();
    let jwt_engine = Arc::new(local_engine_with_revocation(jwt_secret, revocation_store.clone()));

    // Initialize App State & Dynamic File Watchers
    let app_state = AppState::new(
        "/etc/amoeba/services.json",
        "/etc/amoeba/users.json",
        jwt_engine,
        Some(revocation_store),
    );

    // Admin-only user management routes: require both a valid JWT and the "admin"
    // role. route_layer calls stack innermost-first, so unified_auth_middleware
    // (added last) runs before require_admin_role, matching the order Claims must
    // be attached before the role check can read them.
    let admin_routes = Router::new()
        .route("/users", post(create_user))
        .route("/users/:username", axum::routing::patch(update_user).delete(delete_user))
        .route_layer(middleware::from_fn(require_admin_role))
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ));

    // Auth routes: login is public; revoke requires a valid JWT (admin role checked
    // inside the handler after the middleware attaches Claims).
    let revoke_route = Router::new()
        .route("/revoke", post(revoke))
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ));

    let auth_routes = Router::new()
        .route("/login", post(login))
        .merge(revoke_route);

    // Proxy routes handle their own auth inside proxy_handler, since whether a
    // token is required at all depends on the target service's "public" flag,
    // which isn't known until the service catalog has been consulted.
    let app = Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .nest("/admin", admin_routes)
        .nest("/auth", auth_routes)
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    info!("🚀 Amoeba Compute Orchestrator running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}
