//! HTTP handlers for `/admin/apps` — install, list, and uninstall Amoeba App
//! Packages. These routes are gated by the same admin role middleware as
//! `/admin/users`.

use crate::apps::manager::{AppManager, AppManagerError, spawn_readiness_probe};
use crate::apps::schema::{InstallValues, PolicyPatch};
use crate::apps::state::{AppLifecycleSnapshot, AppLifecycleState};
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

    let snapshot = state
        .app_state(&app_name)
        .map(|s| s.snapshot())
        .unwrap_or_else(|| AppLifecycleState::installed().snapshot());

    Ok(Json(AppStatusResponse {
        name: app_name,
        state: snapshot,
    }))
}

#[derive(Debug, Deserialize, Default)]
pub struct ServiceTokenRequest {
    pub org_id: Option<String>,
}

#[derive(Serialize)]
pub struct ServiceTokenResponse {
    pub token: String,
    pub org_id: Option<String>,
    pub role: String,
}

/// Mints a JWT whose only role is `mcp:<service>`. The service policy still
/// has to grant that role; this does not widen access by itself.
pub async fn issue_service_token(
    State(state): State<Arc<AppState>>,
    Path(app_name): Path<String>,
    Json(body): Json<ServiceTokenRequest>,
) -> Result<Json<ServiceTokenResponse>, (StatusCode, String)> {
    let manager = app_manager(&state);
    let service = manager.get_service(&app_name).map_err(map_manager_error)?;
    let org_id = token_org(&service, body.org_id)?;
    let role = scoped_role(&app_name);
    let token = state
        .jwt_engine
        .issue_token(
            &role,
            org_id.as_deref(),
            &[role.clone()],
            Duration::from_secs(60 * 60),
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    Ok(Json(ServiceTokenResponse {
        token,
        org_id,
        role,
    }))
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
