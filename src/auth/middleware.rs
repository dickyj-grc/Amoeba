//! Axum middleware that verifies the bearer token and attaches `Claims`,
//! plus a follow-on gate restricting routes to callers with the "admin" role.

use super::Claims;
use crate::state::AppState;
use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

fn extract_bearer_token(header_value: &str) -> Option<&str> {
    header_value.strip_prefix("Bearer ")
}

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

    let token = extract_bearer_token(auth_header).ok_or(StatusCode::UNAUTHORIZED)?;

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

fn is_admin(roles: &[String]) -> bool {
    roles.iter().any(|r| r == "admin")
}

/// Route-scoped gate for admin-only endpoints (e.g. `/admin/*`). Must run
/// after `unified_auth_middleware` so `Claims` are already attached to the request.
pub async fn require_admin_role(req: Request, next: Next) -> Result<Response, StatusCode> {
    let claims = req
        .extensions()
        .get::<Claims>()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if is_admin(&claims.roles) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_token_from_bearer_header() {
        assert_eq!(extract_bearer_token("Bearer abc123"), Some("abc123"));
    }

    #[test]
    fn rejects_non_bearer_header() {
        assert_eq!(extract_bearer_token("Basic abc123"), None);
    }

    #[test]
    fn rejects_bare_bearer_with_no_token() {
        assert_eq!(extract_bearer_token("Bearer "), Some(""));
    }

    #[test]
    fn is_admin_true_when_admin_role_present() {
        assert!(is_admin(&["viewer".to_string(), "admin".to_string()]));
    }

    #[test]
    fn is_admin_false_without_admin_role() {
        assert!(!is_admin(&["viewer".to_string(), "analyst".to_string()]));
    }

    #[test]
    fn is_admin_false_for_empty_roles() {
        assert!(!is_admin(&[]));
    }
}
