//! Amoeba database service: one reviewed service holds the database
//! connection, and callers can only use approved definitions (tables opened
//! for specific operations, and named queries). It runs behind the Amoeba
//! gateway, which authenticates callers and sends their identity signed, as
//! `x-amoeba-identity`. With the gateway's public key in
//! `AMOEBA_IDENTITY_PUBLIC_KEY` (Amoeba sets it), that token is required and
//! the plain `x-amoeba-user`, `x-amoeba-org` and `x-amoeba-roles` headers are
//! ignored, so a caller that reaches the service without the gateway cannot
//! claim any roles.

pub mod db;
pub mod defs;
pub mod mcp;
pub mod ops;
pub mod sql;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use db::{Db, DbError};
use defs::Definitions;
use ops::{ApiError, Call};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// The caller, as the gateway verified it.
#[derive(Debug, Clone, Default)]
pub struct Identity {
    pub user: String,
    pub org: Option<String>,
    pub roles: Vec<String>,
}

/// The gateway's Ed25519 public key and this service's name, the `aud` its
/// identity tokens carry.
#[derive(Clone)]
pub struct IdentityKey {
    public_key: Vec<u8>,
    service: String,
}

impl IdentityKey {
    /// From `AMOEBA_IDENTITY_PUBLIC_KEY` (base64url, no padding) and
    /// `AMOEBA_SERVICE_NAME`, as Amoeba sets them.
    pub fn new(public_key: &str, service: &str) -> Result<Self, String> {
        use base64::Engine;
        let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(public_key.trim())
            .map_err(|e| format!("AMOEBA_IDENTITY_PUBLIC_KEY is not base64url: {e}"))?;
        if public_key.len() != 32 {
            return Err("AMOEBA_IDENTITY_PUBLIC_KEY is not an Ed25519 public key".into());
        }
        Ok(Self {
            public_key,
            service: service.to_string(),
        })
    }
}

#[derive(serde::Deserialize)]
struct IdentityClaims {
    sub: String,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
}

impl Identity {
    /// The caller of a request: from the signed `x-amoeba-identity` token
    /// when the service has the gateway's key, else (older setups) from the
    /// plain headers. A missing or invalid token is a 401.
    pub fn from_request(svc: &Service, headers: &HeaderMap) -> Result<Self, ApiError> {
        let Some(key) = &svc.identity_key else {
            return Ok(Self::from_headers(headers));
        };
        let unauthenticated = |message: &str| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                message.to_string(),
            )
        };
        let token = headers
            .get("x-amoeba-identity")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                unauthenticated(
                    "missing x-amoeba-identity: call this service through the Amoeba gateway",
                )
            })?;
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
        validation.set_audience(&[&key.service]);
        validation.set_issuer(&["amoeba"]);
        let claims = jsonwebtoken::decode::<IdentityClaims>(
            token,
            &jsonwebtoken::DecodingKey::from_ed_der(&key.public_key),
            &validation,
        )
        .map_err(|_| unauthenticated("invalid x-amoeba-identity"))?
        .claims;
        Ok(Identity {
            user: claims.sub,
            org: claims.org_id,
            roles: claims.roles,
        })
    }

    /// The caller from the plain gateway headers. Trust only when nothing
    /// but the gateway can reach the service.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let get = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Identity {
            user: get("x-amoeba-user").unwrap_or_default(),
            org: get("x-amoeba-org"),
            roles: get("x-amoeba-roles")
                .map(|r| {
                    r.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// One generation of definitions plus the column types looked up for them.
/// Replaced as a whole on reload, which also drops the type cache.
pub struct Loaded {
    pub defs: Definitions,
    types: tokio::sync::Mutex<HashMap<String, Arc<HashMap<String, String>>>>,
}

impl Loaded {
    fn new(defs: Definitions) -> Self {
        Self {
            defs,
            types: Default::default(),
        }
    }

    async fn column_types(
        &self,
        svc: &Service,
        table: &str,
    ) -> Result<Arc<HashMap<String, String>>, DbError> {
        let mut cache = self.types.lock().await;
        if let Some(t) = cache.get(table) {
            return Ok(t.clone());
        }
        let types = Arc::new(svc.db.column_types(&self.defs.schema, table).await?);
        // An empty result means the table is missing; don't cache it, so
        // creating the table later works without a reload.
        if !types.is_empty() {
            cache.insert(table.to_string(), types.clone());
        }
        Ok(types)
    }
}

pub struct Service {
    pub db: Db,
    loaded: RwLock<Arc<Loaded>>,
    /// Set: callers must present a gateway-signed identity token.
    identity_key: Option<IdentityKey>,
}

impl Service {
    pub fn new(db: Db, defs: Definitions) -> Arc<Self> {
        Self::with_identity_key(db, defs, None)
    }

    pub fn with_identity_key(
        db: Db,
        defs: Definitions,
        identity_key: Option<IdentityKey>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            loaded: RwLock::new(Arc::new(Loaded::new(defs))),
            identity_key,
        })
    }

    pub fn loaded(&self) -> Arc<Loaded> {
        self.loaded.read().expect("definitions lock").clone()
    }

    /// Swaps in new definitions. Callers already in flight finish on the
    /// old generation.
    pub fn replace_definitions(&self, defs: Definitions) {
        *self.loaded.write().expect("definitions lock") = Arc::new(Loaded::new(defs));
    }
}

pub fn router(svc: Arc<Service>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/tables/:table", get(list_rows).post(create_row))
        .route(
            "/tables/:table/:key",
            get(get_row).patch(update_row).delete(delete_row),
        )
        .route("/queries/:name", post(run_query))
        .route("/mcp", post(mcp::handle))
        .with_state(svc)
}

/// A JSON object body; an empty body is an empty object.
fn object_body(body: &Bytes) -> Result<Map<String, Value>, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "body must be a JSON object",
        )),
        Err(e) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad-request",
            format!("invalid JSON: {e}"),
        )),
    }
}

async fn respond(
    svc: &Service,
    headers: &HeaderMap,
    call: Call,
    status: StatusCode,
) -> impl IntoResponse {
    let who = match Identity::from_request(svc, headers) {
        Ok(who) => who,
        Err(e) => return e.into_response(),
    };
    match ops::execute(svc, &who, call).await {
        Ok(body) => (status, axum::Json(body)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_rows(
    State(svc): State<Arc<Service>>,
    Path(table): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let mut filters = Vec::new();
    let mut limit = None;
    let mut offset = 0;
    for (k, v) in query {
        match k.as_str() {
            "limit" => match v.parse() {
                Ok(n) => limit = Some(n),
                Err(_) => {
                    return ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "bad-request",
                        "limit must be a number",
                    )
                    .into_response();
                }
            },
            "offset" => match v.parse() {
                Ok(n) => offset = n,
                Err(_) => {
                    return ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "bad-request",
                        "offset must be a number",
                    )
                    .into_response();
                }
            },
            _ => filters.push((k, v)),
        }
    }
    let call = Call::Select {
        table,
        filters,
        limit,
        offset,
    };
    respond(&svc, &headers, call, StatusCode::OK)
        .await
        .into_response()
}

async fn get_row(
    State(svc): State<Arc<Service>>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> axum::response::Response {
    respond(&svc, &headers, Call::Get { table, key }, StatusCode::OK)
        .await
        .into_response()
}

async fn create_row(
    State(svc): State<Arc<Service>>,
    Path(table): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let values = match object_body(&body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    respond(
        &svc,
        &headers,
        Call::Insert { table, values },
        StatusCode::CREATED,
    )
    .await
    .into_response()
}

async fn update_row(
    State(svc): State<Arc<Service>>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let values = match object_body(&body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    respond(
        &svc,
        &headers,
        Call::Update { table, key, values },
        StatusCode::OK,
    )
    .await
    .into_response()
}

async fn delete_row(
    State(svc): State<Arc<Service>>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> axum::response::Response {
    respond(&svc, &headers, Call::Delete { table, key }, StatusCode::OK)
        .await
        .into_response()
}

async fn run_query(
    State(svc): State<Arc<Service>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let args = match object_body(&body) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    respond(&svc, &headers, Call::Query { name, args }, StatusCode::OK)
        .await
        .into_response()
}
