use amoeba::auth::jwt::local_engine_with_revocation;
use amoeba::auth::jwks::fetch_jwks;
use amoeba::auth::middleware::{require_admin_role, unified_auth_middleware};
use amoeba::auth::revocation::RevocationStore;
use amoeba::auth::{AuthMode, JwtEngine};
use amoeba::config::settings::{
    AmoebaConfig, AuthSettings, ServerSettings, AUTH_MODE_JWKS, AUTH_MODE_LOCAL_JWT,
    DEFAULT_BIND_ADDR, DEFAULT_CONFIG_PATH, DEFAULT_REVOCATION_FILE, DEFAULT_USERS_FILE,
};
use amoeba::routing::admin::{create_user, delete_user, update_user};
use amoeba::routing::apps_admin::{get_app_status, install_app, list_apps, uninstall_app};
use amoeba::routing::auth::{login, revoke};
use amoeba::routing::proxy::{proxy_handler, proxy_handler_root};
use amoeba::state::AppState;
use axum::{
    Router, middleware,
    routing::{any, post},
};
use jsonwebtoken::{Algorithm, Validation};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Algorithms accepted for tokens verified against a JWKS endpoint. The exact
/// algorithm is further restricted per key (see `JwtEngine::verify_token`).
const JWKS_ALGORITHMS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

#[tokio::main]
async fn main() {
    // Initialize JSON Tracing
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .json()
        .init();

    let config = match load_settings() {
        Ok(config) => config,
        Err(e) => {
            error!("configuration error: {e}");
            std::process::exit(1);
        }
    };

    let revocation_store = RevocationStore::persistent(&config.auth.revocation_file);

    let mut validation = Validation::default();
    if let Some(issuer) = &config.auth.issuer {
        validation.set_issuer(&[issuer.clone()]);
    }
    if let Some(audience) = &config.auth.audience {
        validation.set_audience(&[audience.clone()]);
    }

    // Configure Authentication Mode
    let jwt_engine = match config.auth.mode.as_str() {
        AUTH_MODE_LOCAL_JWT => {
            let secret = match config.auth.resolve_jwt_secret() {
                Ok(secret) => secret,
                Err(e) => {
                    error!("configuration error: {e}");
                    std::process::exit(1);
                }
            };
            let mut engine = local_engine_with_revocation(secret, revocation_store.clone());
            engine.validation = validation;
            Arc::new(engine)
        }
        AUTH_MODE_JWKS => {
            let jwks_url = config
                .auth
                .jwks_url
                .clone()
                .expect("validated by AmoebaConfig::load");
            let jwks = match fetch_jwks(&jwks_url).await {
                Ok(jwks) => jwks,
                Err(e) => {
                    error!("configuration error: failed to load initial JWKS: {e}");
                    std::process::exit(1);
                }
            };
            info!("🔑 Loaded {} JWKS public key(s) from {jwks_url}", jwks.keys.len());

            let cached_keys = Arc::new(RwLock::new(jwks));
            amoeba::auth::jwks::start_jwks_refresh_daemon(jwks_url.clone(), cached_keys.clone());

            let mut validation = validation;
            validation.algorithms = JWKS_ALGORITHMS.to_vec();
            Arc::new(JwtEngine {
                mode: AuthMode::Jwks {
                    jwks_url,
                    cached_keys,
                },
                validation,
                revocation_store: Some(revocation_store.clone()),
            })
        }
        _ => unreachable!("AmoebaConfig::load rejects unknown auth modes"),
    };

    // Initialize App State & Dynamic File Watchers
    let app_state = AppState::new(
        "/etc/amoeba/services.json",
        &config.auth.users_file,
        jwt_engine,
        Some(revocation_store),
    );

    // Admin-only user management routes: require both a valid JWT and the "admin"
    // role. route_layer calls stack innermost-first, so unified_auth_middleware
    // (added last) runs before require_admin_role, matching the order Claims must
    // be attached before the role check can read them.
    let admin_routes = Router::new()
        .route("/users", post(create_user))
        .route(
            "/users/:username",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route("/apps", post(install_app).get(list_apps))
        .route(
            "/apps/:name",
            axum::routing::get(get_app_status).delete(uninstall_app),
        )
        .route_layer(middleware::from_fn(require_admin_role))
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ));

    // Auth routes: login is public; revoke requires a valid JWT (admin role checked
    // inside the handler after the middleware attaches Claims).
    let revoke_route =
        Router::new()
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
        .route("/v1/:service_name/", any(proxy_handler_root))
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .nest("/admin", admin_routes)
        .nest("/auth", auth_routes)
        .with_state(app_state);

    let bind_addr = config.server.bind_addr;
    let listener = tokio::net::TcpListener::bind(&bind_addr).await.unwrap();
    info!("🚀 Amoeba Compute Orchestrator running on http://{bind_addr}");
    axum::serve(listener, app).await.unwrap();
}

/// Loads `config.toml` (path from `AMOEBA_CONFIG`, default
/// `/etc/amoeba/config.toml`). When the file doesn't exist, falls back to
/// legacy environment-based configuration: local JWT mode, the secret from
/// `AMOEBA_LOCAL_JWT_SECRET`, fixed state paths, `0.0.0.0:8080`. That fallback
/// still fails closed — a missing signing secret is a startup error, never a
/// silent default.
fn load_settings() -> Result<AmoebaConfig, String> {
    let config_path =
        std::env::var("AMOEBA_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string());

    if std::path::Path::new(&config_path).is_file() {
        return AmoebaConfig::load(&config_path);
    }

    warn!("{config_path} not found; falling back to environment-based defaults (local_jwt mode, fixed paths)");
    let jwt_secret = std::env::var("AMOEBA_LOCAL_JWT_SECRET").map_err(|_| {
        "AMOEBA_LOCAL_JWT_SECRET is not set and no config file was found; \
         refusing to start with an insecure default secret"
            .to_string()
    })?;

    Ok(AmoebaConfig {
        server: ServerSettings {
            bind_addr: DEFAULT_BIND_ADDR.to_string(),
            mode: None,
        },
        auth: AuthSettings {
            mode: AUTH_MODE_LOCAL_JWT.to_string(),
            issuer: None,
            audience: None,
            jwks_url: None,
            jwt_secret: Some(jwt_secret),
            users_file: DEFAULT_USERS_FILE.to_string(),
            revocation_file: DEFAULT_REVOCATION_FILE.to_string(),
        },
    })
}
