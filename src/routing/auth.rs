//! Public authentication handlers: login (username/password -> JWT) and token
//! revocation (admin-only).

use crate::auth::Claims;
use crate::auth::users::UserStore;
use crate::state::AppState;
use axum::{
    extract::{Json, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

const DEFAULT_TOKEN_TTL_SECONDS: u64 = 3600;
const LOGIN_FAILURE_DELAY: Duration = Duration::from_millis(200);

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: usize,
}

/// Exchanges a username/password for a short-lived JWT.
/// Returns a generic 401 on any failure to avoid user enumeration.
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let result = async {
        let store = UserStore::load(&state.users_file).map_err(|e| {
            error!("failed to load users file: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let user = store
            .users
            .get(&payload.username)
            .ok_or(StatusCode::UNAUTHORIZED)?;

        if !crate::auth::users::verify_password(&payload.password, &user.password_hash) {
            return Err(StatusCode::UNAUTHORIZED);
        }

        let ttl = token_ttl();
        let token = state
            .jwt_engine
            .issue_token(&payload.username, Some(&user.org_id), &user.roles, ttl)
            .map_err(|e| {
                error!("failed to issue token: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        let exp = crate::auth::jwt::exp_from_now(ttl).map_err(|e| {
            error!("failed to compute expiry: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        Ok(Json(LoginResponse {
            token,
            expires_at: exp,
        }))
    }
    .await;

    if result.is_err() {
        tokio::time::sleep(LOGIN_FAILURE_DELAY).await;
    }

    result
}

/// Revokes the JWT presented in the request. Requires a valid token with the
/// "admin" role. The revocation is stored in memory only and is lost on restart.
pub async fn revoke(
    State(state): State<Arc<AppState>>,
    claims: axum::extract::Extension<Claims>,
) -> Result<StatusCode, StatusCode> {
    let is_admin = claims.roles.iter().any(|r| r == "admin");
    if !is_admin {
        return Err(StatusCode::FORBIDDEN);
    }

    if let Some(store) = &state.revocation_store {
        store.revoke(claims.jti.clone()).await;
        Ok(StatusCode::NO_CONTENT)
    } else {
        // Revocation is not configured; this should not happen in normal operation.
        error!("revocation store is not configured");
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

fn token_ttl() -> Duration {
    let seconds: u64 = std::env::var("AMOEBA_TOKEN_TTL_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TOKEN_TTL_SECONDS);
    Duration::from_secs(seconds)
}
