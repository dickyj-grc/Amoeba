//! Axum middleware that verifies the bearer token and attaches `Claims`.

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
}
