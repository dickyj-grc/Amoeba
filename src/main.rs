use arc_swap::ArcSwap;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header::AUTHORIZATION, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path as FilePath,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::RwLock;
use tracing::{info, Level};

// ============================================================================
// 1. DOMAIN MODELS & CONFIGURATION
// ============================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub org_id: Option<String>,
    pub roles: Vec<String>,
    pub exp: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UpstreamAuth {
    pub r#type: String, // "bearer_static" or "custom_header"
    pub token_env_var: Option<String>,
    pub header_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OperationRule {
    pub operation: String,
    pub match_type: String, // "http_method"
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceConfig {
    pub image: String,
    pub ip: String,
    pub port: u16,
    pub cooldown_seconds: Option<u64>,
    pub operation_rules: Option<Vec<OperationRule>>,
    pub permissions: HashMap<String, Vec<String>>,
    pub upstream_auth: Option<UpstreamAuth>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceCatalog {
    pub services: HashMap<String, ServiceConfig>,
}

// Runtime tracker for active connections and activity timestamps
pub struct ServiceRuntimeState {
    pub last_accessed_unix: AtomicU64,
    pub active_connections: AtomicU64,
}

impl ServiceRuntimeState {
    pub fn new() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        Self {
            last_accessed_unix: AtomicU64::new(now),
            active_connections: AtomicU64::new(0),
        }
    }

    pub fn touch(&self) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        self.last_accessed_unix.store(now, Ordering::Relaxed);
    }
}

// ============================================================================
// 2. UNIFIED JWT ENGINE (Local HMAC + Aptiwise JWKS)
// ============================================================================

#[derive(Clone)]
pub enum AuthMode {
    LocalJwt {
        secret: Vec<u8>,
    },
    Jwks {
        jwks_url: String,
        cached_keys: Arc<RwLock<jsonwebtoken::jwk::JwkSet>>,
    },
}

#[derive(Clone)]
pub struct JwtEngine {
    pub mode: AuthMode,
    pub validation: Validation,
}

impl JwtEngine {
    pub async fn verify_token(&self, token: &str) -> Result<Claims, String> {
        match &self.mode {
            AuthMode::LocalJwt { secret } => {
                let key = DecodingKey::from_secret(secret);
                decode::<Claims>(token, &key, &self.validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("Local HMAC failed: {}", e))
            }
            AuthMode::Jwks { cached_keys, .. } => {
                let header = decode_header(token).map_err(|e| e.to_string())?;
                let kid = header.kid.ok_or("Token missing 'kid' header")?;

                let keys = cached_keys.read().await;
                let jwk = keys.find(&kid).ok_or("Key ID not found in JWKS cache")?;

                let decoding_key = DecodingKey::from_jwk(jwk).map_err(|e| e.to_string())?;
                decode::<Claims>(token, &decoding_key, &self.validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("Aptiwise JWKS validation failed: {}", e))
            }
        }
    }
}

// Background task to refresh JWKS public keys periodically
pub async fn start_jwks_refresh_daemon(jwks_url: String, cache: Arc<RwLock<jsonwebtoken::jwk::JwkSet>>) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            if let Ok(response) = client.get(&jwks_url).send().await {
                if let Ok(jwks) = response.json::<jsonwebtoken::jwk::JwkSet>().await {
                    let mut lock = cache.write().await;
                    *lock = jwks;
                    info!("🔑 Successfully updated Aptiwise JWKS public key cache.");
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        }
    });
}

// ============================================================================
// 3. APPLICATION STATE & LOCK-FREE CONFIG WATCHER
// ============================================================================

pub struct AppState {
    pub catalog: ArcSwap<ServiceCatalog>,
    pub runtime_states: ArcSwap<HashMap<String, Arc<ServiceRuntimeState>>>,
    pub jwt_engine: Arc<JwtEngine>,
    pub http_client: reqwest::Client,
}

impl AppState {
    pub fn new(config_path: &str, jwt_engine: Arc<JwtEngine>) -> Arc<Self> {
        let initial_catalog = Self::load_catalog(config_path).unwrap_or_else(|_| ServiceCatalog {
            services: HashMap::new(),
        });

        let mut initial_runtimes = HashMap::new();
        for key in initial_catalog.services.keys() {
            initial_runtimes.insert(key.clone(), Arc::new(ServiceRuntimeState::new()));
        }

        let state = Arc::new(Self {
            catalog: ArcSwap::from_pointee(initial_catalog),
            runtime_states: ArcSwap::from_pointee(initial_runtimes),
            jwt_engine,
            http_client: reqwest::Client::new(),
        });

        Self::spawn_file_watcher(state.clone(), config_path.to_string());
        Self::spawn_reaper_thread(state.clone());

        state
    }

    fn load_catalog(path: &str) -> Result<ServiceCatalog, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let catalog: ServiceCatalog = serde_json::from_str(&content)?;
        Ok(catalog)
    }

    fn spawn_file_watcher(state: Arc<Self>, path_str: String) {
        tokio::spawn(async move {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = notify::recommended_watcher(tx).unwrap();
            watcher.watch(FilePath::new(&path_str), RecursiveMode::NonRecursive).unwrap();

            for res in rx {
                if let Ok(Event { kind: EventKind::Modify(_), .. }) = res {
                    if let Ok(new_catalog) = Self::load_catalog(&path_str) {
                        info!("🔄 Modifying service catalog from disk (Lock-free reload)");
                        
                        let mut new_runtimes = (*state.runtime_states.load().clone()).clone();
                        for key in new_catalog.services.keys() {
                            new_runtimes.entry(key.clone()).or_insert_with(|| Arc::new(ServiceRuntimeState::new()));
                        }

                        state.catalog.store(Arc::new(new_catalog));
                        state.runtime_states.store(Arc::new(new_runtimes));
                    }
                }
            }
        });
    }

    fn spawn_reaper_thread(state: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                let catalog = state.catalog.load();
                let runtimes = state.runtime_states.load();
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

                for (name, service) in &catalog.services {
                    if let Some(cooldown) = service.cooldown_seconds {
                        if let Some(runtime) = runtimes.get(name) {
                            let active = runtime.active_connections.load(Ordering::Relaxed);
                            let last = runtime.last_accessed_unix.load(Ordering::Relaxed);

                            if active == 0 && (now - last) >= cooldown {
                                info!(
                                    service = %name,
                                    idle_secs = %(now - last),
                                    "💤 Scaling service to zero (Stopping micro-VM/container)"
                                );
                                // TODO: Call Docker API (`/var/run/docker.sock`) or Fly Machine API to stop container
                            }
                        }
                    }
                }
            }
        });
    }
}

// ============================================================================
// 4. MIDDLEWARE (AUTH, TELEMETRY, & ACL)
// ============================================================================

pub async fn unified_auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !auth_header.starts_with("Bearer ") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let token = &auth_header[7..];

    match state.jwt_engine.verify_token(token).await {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(err) => {
            tracing::error!("Auth failure: {}", err);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

// ============================================================================
// 5. CORE PROXY & COMPUTATION HANDLER
// ============================================================================

pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    Path((service_name, subpath)): Path<(String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let start_time = Instant::now();
    let catalog = state.catalog.load();

    // 1. Resolve target service configuration
    let service_cfg = catalog
        .services
        .get(&service_name)
        .ok_or(StatusCode::NOT_FOUND)?;

    // 2. Extract Claims from Auth Middleware
    let claims = req
        .extensions()
        .get::<Claims>()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // 3. Classify Operation (HTTP Method -> Operation Name)
    let method = req.method().to_string();
    let operation = match method.as_str() {
        "GET" | "HEAD" => "read",
        "POST" => "add",
        "PUT" | "PATCH" => "update",
        "DELETE" => "delete",
        _ => "read",
    };

    // 4. ACL Check: Evaluate Roles against Permissions Matrix
    let allowed_roles = service_cfg
        .permissions
        .get(operation)
        .cloned()
        .unwrap_or_default();

    let has_permission = claims.roles.iter().any(|r| allowed_roles.contains(r));
    if !has_permission {
        info!(
            user = %claims.sub,
            service = %service_name,
            operation = %operation,
            "⛔ Access Denied (403 Forbidden)"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    // 5. Track Runtime Activity & Connection Counter
    let runtimes = state.runtime_states.load();
    let runtime = runtimes
        .get(&service_name)
        .cloned()
        .unwrap_or_else(|| Arc::new(ServiceRuntimeState::new()));

    runtime.touch();
    runtime.active_connections.fetch_add(1, Ordering::Relaxed);

    // TODO: Insert Container Boot Check here (If container is stopped, trigger boot and await health check)

    // 6. Build Upstream Request URL
    let target_url = format!(
        "http://{}:{}/{}",
        service_cfg.ip, service_cfg.port, subpath
    );

    let mut outbound_req = state.http_client.request(req.method().clone(), &target_url);

    // 7. Handle Upstream Token Translation / Credential Injection
    if let Some(auth_cfg) = &service_cfg.upstream_auth {
        match auth_cfg.r#type.as_str() {
            "bearer_static" => {
                if let Some(env_var) = &auth_cfg.token_env_var {
                    let token = std::env::var(env_var).unwrap_or_default();
                    outbound_req = outbound_req.header(AUTHORIZATION, format!("Bearer {}", token));
                }
            }
            "custom_header" => {
                if let (Some(hdr), Some(env_var)) = (&auth_cfg.header_name, &auth_cfg.token_env_var) {
                    let val = std::env::var(env_var).unwrap_or_default();
                    outbound_req = outbound_req.header(
                        HeaderName::from_bytes(hdr.as_bytes()).unwrap(),
                        HeaderValue::from_str(&val).unwrap(),
                    );
                }
            }
            _ => {}
        }
    }

    // Forward Request Payload Body
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    outbound_req = outbound_req.body(body_bytes);

    // 8. Execute Forward Proxy Request
    let response = outbound_req.send().await;

    // Decrement Connection Counter
    runtime.active_connections.fetch_sub(1, Ordering::Relaxed);

    match response {
        Ok(res) => {
            let status = res.status();
            let res_bytes = res.bytes().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            // 9. Emit Usage Telemetry Log
            info!(
                target: "aptiwise_usage_telemetry",
                user_id = %claims.sub,
                org_id = %claims.org_id.as_deref().unwrap_or("none"),
                service = %service_name,
                operation = %operation,
                status = %status.as_u16(),
                latency_ms = %start_time.elapsed().as_millis(),
                "API Execution Processed"
            );

            Ok(Response::builder()
                .status(status)
                .body(Body::from(res_bytes))
                .unwrap())
        }
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}

// ============================================================================
// 6. MAIN APPLICATION ENTRYPOINT
// ============================================================================

#[tokio::main]
async fn main() {
    // Initialize JSON Tracing
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .json()
        .init();

    // Configure Authentication Mode
    let auth_mode = AuthMode::LocalJwt {
        secret: "super_secret_local_key_change_in_production".as_bytes().to_vec(),
    };

    let jwt_engine = Arc::new(JwtEngine {
        mode: auth_mode,
        validation: Validation::default(),
    });

    // Initialize App State & Dynamic File Watchers
    let app_state = AppState::new("/etc/aptiwise/services.json", jwt_engine);

    // Build Router Pipeline
    let app = Router::new()
        .route("/v1/:service_name/*subpath", any(proxy_handler))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            unified_auth_middleware,
        ))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    info!("🚀 Aptiwise Compute Orchestrator running on http://0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}