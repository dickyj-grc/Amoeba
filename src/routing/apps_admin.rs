//! HTTP handlers for `/admin/apps` — install, list, and uninstall Amoeba App
//! Packages. These routes are gated by the same admin role middleware as
//! `/admin/users`.

use crate::apps::build::{BuildRequest, build_image, validate_image_name};
use crate::apps::manager::{AppManager, AppManagerError, spawn_readiness_probe};
use crate::apps::schema::{InstallValues, PolicyPatch};
use crate::apps::state::{AppLifecycleSnapshot, AppLifecycleState};
use crate::auth::issued::{IssuedToken, now_secs};
use crate::routing::operation::scoped_role;
use crate::state::AppState;
use axum::{
    Json,
    body::Bytes,
    extract::{Multipart, Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Response body returned after a successful install.
#[derive(Serialize)]
pub struct InstallResponse {
    pub name: String,
    pub version: String,
    pub description: String,
    pub state: AppLifecycleSnapshot,
}

pub async fn install_app(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<InstallResponse>), (StatusCode, String)> {
    let mut package_bytes: Option<Bytes> = None;
    let mut values: InstallValues = InstallValues::default();

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("failed to read multipart field: {e}"),
        )
    })? {
        let name = field.name().unwrap_or_default().to_string();
        let data = field.bytes().await.map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("failed to read field bytes: {e}"),
            )
        })?;

        match name.as_str() {
            "package" => package_bytes = Some(data),
            "values" => {
                values = serde_json::from_slice(&data)
                    .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid values JSON: {e}")))?;
            }
            _ => {}
        }
    }

    let package_bytes = package_bytes.ok_or((
        StatusCode::BAD_REQUEST,
        "missing 'package' field".to_string(),
    ))?;

    let manager = app_manager(&state);
    let manifest = manager
        .install(&package_bytes, values)
        .await
        .map_err(map_manager_error)?;

    let app_name = manifest.service_name().to_string();
    reload_catalog(&state);
    let app_state = Arc::new(AppLifecycleState::installed());
    state.set_app_state(&app_name, app_state.clone());
    spawn_readiness_probe(state.clone(), app_name.clone());

    Ok((
        StatusCode::CREATED,
        Json(InstallResponse {
            name: app_name,
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            state: app_state.snapshot(),
        }),
    ))
}

#[derive(Serialize)]
pub struct AppsListResponse {
    pub apps: Vec<String>,
}

pub async fn list_apps(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AppsListResponse>, (StatusCode, String)> {
    let manager = app_manager(&state);
    let apps = manager.list_installed().map_err(map_manager_error)?;
    Ok(Json(AppsListResponse { apps }))
}

#[derive(Serialize)]
pub struct AppStatusResponse {
    pub name: String,
    pub state: AppLifecycleSnapshot,
}

pub async fn get_app_status(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
) -> Result<Json<AppStatusResponse>, (StatusCode, String)> {
    let manager = app_manager(&state);
    let apps = manager.list_installed().map_err(map_manager_error)?;
    if !apps.contains(&app_name) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("app '{app_name}' is not installed"),
        ));
    }

    let snapshot = state
        .app_state(&app_name)
        .map(|s| s.snapshot())
        .unwrap_or_else(|| AppLifecycleState::installed().snapshot());

    Ok(Json(AppStatusResponse {
        name: app_name,
        state: snapshot,
    }))
}

pub async fn uninstall_app(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let manager = app_manager(&state);
    manager
        .uninstall(&app_name)
        .await
        .map_err(map_manager_error)?;
    reload_catalog(&state);

    // Remove the lifecycle state so the app no longer appears ready.
    let mut new_states = (*state.app_states.load().clone()).clone();
    new_states.remove(&app_name);
    state.app_states.store(Arc::new(new_states));

    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_app_policy(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
    Json(patch): Json<PolicyPatch>,
) -> Result<Json<AppStatusResponse>, (StatusCode, String)> {
    let manager = app_manager(&state);
    manager
        .update_policy(&app_name, patch)
        .await
        .map_err(map_manager_error)?;
    reload_catalog(&state);

    let snapshot = state
        .app_state(&app_name)
        .map(|s| s.snapshot())
        .unwrap_or_else(|| AppLifecycleState::installed().snapshot());

    Ok(Json(AppStatusResponse {
        name: app_name,
        state: snapshot,
    }))
}

/// Default lifetime of a service token, kept short for one-off testing.
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);
const MIN_TOKEN_TTL: Duration = Duration::from_secs(60);
/// AI clients keep the token in their config, so allow up to a year.
const MAX_TOKEN_TTL: Duration = Duration::from_secs(366 * 24 * 60 * 60);
const MAX_TOKEN_LABEL_CHARS: usize = 100;

#[derive(Debug, Deserialize, Default)]
pub struct ServiceTokenRequest {
    pub org_id: Option<String>,
    /// Lifetime in seconds; 1 minute to 366 days. Defaults to 1 hour.
    pub ttl_seconds: Option<u64>,
    /// Where the token is used, e.g. "Cursor on laptop". Shown in the list.
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct ServiceTokenResponse {
    pub token: String,
    pub org_id: Option<String>,
    pub role: String,
    pub service: String,
    pub jti: String,
    pub label: String,
    /// Unix seconds.
    pub expires_at: u64,
}

/// Checks the requested lifetime and label before anything is signed.
fn token_options(body: &ServiceTokenRequest) -> Result<(Duration, String), (StatusCode, String)> {
    let ttl = body
        .ttl_seconds
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TOKEN_TTL);
    if !(MIN_TOKEN_TTL..=MAX_TOKEN_TTL).contains(&ttl) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "ttl_seconds must be between {} and {}",
                MIN_TOKEN_TTL.as_secs(),
                MAX_TOKEN_TTL.as_secs()
            ),
        ));
    }
    let label = body.label.as_deref().unwrap_or("").trim().to_string();
    if label.chars().count() > MAX_TOKEN_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("label must be at most {MAX_TOKEN_LABEL_CHARS} printable characters"),
        ));
    }
    Ok((ttl, label))
}

/// Mints a JWT whose only role is `mcp:<service>`. The service policy still
/// has to grant that role; this does not widen access by itself. The token is
/// returned once; only its metadata is recorded, for listing and revoking.
pub async fn issue_service_token(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
    Json(body): Json<ServiceTokenRequest>,
) -> Result<Json<ServiceTokenResponse>, (StatusCode, String)> {
    let manager = app_manager(&state);
    let service = manager.get_service(&app_name).map_err(map_manager_error)?;
    let (ttl, label) = token_options(&body)?;
    let org_id = token_org(&service, body.org_id)?;
    let role = scoped_role(&app_name);
    let (token, claims) = state
        .jwt_engine
        .issue_token_with_claims(&role, org_id.as_deref(), std::slice::from_ref(&role), ttl)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let expires_at = claims.exp as u64;
    state
        .issued_tokens
        .record(IssuedToken {
            jti: claims.jti.clone(),
            service: app_name.clone(),
            label: label.clone(),
            org_id: org_id.clone(),
            role: role.clone(),
            issued_at: now_secs(),
            expires_at,
        })
        .await;

    Ok(Json(ServiceTokenResponse {
        token,
        org_id,
        role,
        service: app_name,
        jti: claims.jti,
        label,
        expires_at,
    }))
}

/// Unexpired tokens issued for a service, metadata only.
pub async fn list_service_tokens(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
) -> Result<Json<Vec<IssuedToken>>, (StatusCode, String)> {
    app_manager(&state)
        .get_service(&app_name)
        .map_err(map_manager_error)?;
    Ok(Json(state.issued_tokens.list(&app_name).await))
}

/// Revokes one issued token by `jti`. The revocation is persisted, so the
/// token stays rejected until it would have expired anyway.
pub async fn revoke_service_token(
    State(state): State<Arc<AppState>>,
    Path((app_name, jti)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    let Some(store) = &state.revocation_store else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "revocation is not configured".into(),
        ));
    };
    if state.issued_tokens.remove(&app_name, &jti).await.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no token '{jti}' for service '{app_name}'"),
        ));
    }
    store.revoke(jti).await;
    Ok(StatusCode::NO_CONTENT)
}

fn token_org(
    service: &crate::config::schema::ServiceConfig,
    requested: Option<String>,
) -> Result<Option<String>, (StatusCode, String)> {
    if let Some(by_org) = &service.tenant_permissions {
        let org = requested.ok_or((
            StatusCode::BAD_REQUEST,
            "org_id is required for a multi-tenant service".into(),
        ))?;
        if !by_org.contains_key(&org) {
            return Err((
                StatusCode::FORBIDDEN,
                format!("org '{org}' is not on this service"),
            ));
        }
        return Ok(Some(org));
    }
    if let Some(tenant) = &service.tenant {
        if let Some(requested) = &requested {
            if requested != tenant {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("service is bound to tenant '{tenant}'"),
                ));
            }
        }
        return Ok(Some(tenant.clone()));
    }
    Ok(requested)
}

#[derive(Serialize)]
pub struct BuildResponse {
    pub image: String,
    pub log: Vec<String>,
}

#[derive(Serialize)]
pub struct BuildErrorResponse {
    pub error: String,
    pub log: Vec<String>,
}

/// Builds an image for `name` from a Git repository or a Dockerfile and
/// returns its tag, which the caller then installs as an image app. Bad input
/// is 400; a failed clone or build is 422 with the build log.
pub async fn build_app_image(
    Path(name): Path<String>,
    Json(request): Json<BuildRequest>,
) -> Result<Json<BuildResponse>, (StatusCode, Json<BuildErrorResponse>)> {
    let bad_request = |error: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(BuildErrorResponse {
                error,
                log: Vec::new(),
            }),
        )
    };
    validate_image_name(&name).map_err(bad_request)?;
    request.validate().map_err(bad_request)?;

    let registry = std::env::var("AMOEBA_REGISTRY").ok();
    match build_image(&name, &request, registry.as_deref()).await {
        Ok(outcome) => Ok(Json(BuildResponse {
            image: outcome.image,
            log: outcome.log,
        })),
        Err(failure) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(BuildErrorResponse {
                error: failure.message,
                log: failure.log,
            }),
        )),
    }
}

fn app_manager(state: &AppState) -> AppManager {
    AppManager::new(&state.catalog_path)
}

fn map_manager_error(e: AppManagerError) -> (StatusCode, String) {
    match e {
        AppManagerError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
        AppManagerError::Quota(msg) => (StatusCode::CONFLICT, msg),
        AppManagerError::Zip(_) | AppManagerError::Yaml(_) | AppManagerError::Json(_) => {
            (StatusCode::BAD_REQUEST, e.to_string())
        }
        AppManagerError::Io(msg) if msg.to_string().contains("not found") => {
            (StatusCode::NOT_FOUND, msg.to_string())
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Applies the catalog the manager just wrote. The write already succeeded,
/// so a failed re-read is logged rather than failing the request; the file
/// watcher will still pick the change up.
fn reload_catalog(state: &AppState) {
    if let Err(e) = state.reload_catalog() {
        tracing::warn!("catalog written but not reloaded: {e}");
    }
}
