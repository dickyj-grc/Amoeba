//! HTTP handlers for `/admin/users`. Gated by `auth::middleware::require_admin_role`,
//! which must run after `unified_auth_middleware` on these routes.

use crate::auth::users::{UserStore, UserStoreError};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub roles: Vec<String>,
    pub org_id: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateUserRequest {
    pub password: Option<String>,
    pub roles: Option<Vec<String>>,
    pub org_id: Option<String>,
}

fn to_http_error(e: UserStoreError) -> (StatusCode, String) {
    match e {
        UserStoreError::AlreadyExists(_) => (StatusCode::CONFLICT, e.to_string()),
        UserStoreError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
        UserStoreError::Hash(_) | UserStoreError::Io(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

pub async fn create_user(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateUserRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut store = UserStore::load(&state.users_file).map_err(to_http_error)?;
    store
        .add_user(&payload.username, &payload.password, payload.roles, payload.org_id)
        .map_err(to_http_error)?;
    store.save(&state.users_file).map_err(to_http_error)?;

    Ok(StatusCode::CREATED)
}

pub async fn update_user(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
    Json(payload): Json<UpdateUserRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if payload.password.is_none() && payload.roles.is_none() && payload.org_id.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "nothing to update: provide password, roles, and/or org_id".to_string(),
        ));
    }

    let mut store = UserStore::load(&state.users_file).map_err(to_http_error)?;
    store
        .update_user(
            &username,
            payload.password.as_deref(),
            payload.roles,
            payload.org_id,
        )
        .map_err(to_http_error)?;
    store.save(&state.users_file).map_err(to_http_error)?;

    Ok(StatusCode::OK)
}

pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    Path(username): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut store = UserStore::load(&state.users_file).map_err(to_http_error)?;
    store.delete_user(&username).map_err(to_http_error)?;
    store.save(&state.users_file).map_err(to_http_error)?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_already_exists_to_conflict() {
        let (status, _) = to_http_error(UserStoreError::AlreadyExists("dicky".into()));
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[test]
    fn maps_not_found_to_404() {
        let (status, _) = to_http_error(UserStoreError::NotFound("ghost".into()));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn maps_hash_and_io_errors_to_500() {
        let (status, _) = to_http_error(UserStoreError::Hash("boom".into()));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

        let (status, _) = to_http_error(UserStoreError::Io("boom".into()));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
