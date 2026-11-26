//! OAuth Protected Resource Metadata (RFC 9728), which the MCP authorization
//! spec requires: a client that gets 401 from a service learns from the
//! `WWW-Authenticate` header where the metadata is, and from the metadata
//! which authorization servers issue tokens for it. Each service under
//! `/v1/<name>/` is a protected resource, described at
//! `/.well-known/oauth-protected-resource/v1/<name>/<path>`.

use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;

pub const WELL_KNOWN_PREFIX: &str = "/.well-known/oauth-protected-resource";

/// `scheme://host` as the caller sees the gateway, honoring the
/// `X-Forwarded-Proto` / `X-Forwarded-Host` a TLS-terminating proxy sets.
pub fn public_origin(headers: &HeaderMap) -> String {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(',').next().unwrap_or(v).trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let scheme = get("x-forwarded-proto").unwrap_or_else(|| "http".to_string());
    let host = get("x-forwarded-host")
        .or_else(|| get(header::HOST.as_str()))
        .unwrap_or_else(|| "localhost".to_string());
    format!("{scheme}://{host}")
}

/// The resource a request addresses: `<origin>/v1/<service>/<subpath>`.
fn resource(origin: &str, service: &str, subpath: &str) -> String {
    format!("{origin}/v1/{service}/{subpath}")
}

/// The 401 a service answers without a valid token, pointing at its metadata.
pub fn unauthorized(headers: &HeaderMap, service: &str, subpath: &str) -> Response {
    let origin = public_origin(headers);
    let metadata = format!("{origin}{WELL_KNOWN_PREFIX}/v1/{service}/{subpath}");
    let challenge = format!("Bearer resource_metadata=\"{metadata}\"");
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

pub async fn metadata_root(
    state: State<Arc<AppState>>,
    Path(service): Path<String>,
    headers: HeaderMap,
) -> Response {
    metadata(state, Path((service, String::new())), headers).await
}

/// `GET /.well-known/oauth-protected-resource/v1/<service>/<path>`.
pub async fn metadata(
    State(state): State<Arc<AppState>>,
    Path((service, subpath)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(config) = state.catalog.load().services.get(&service).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let origin = public_origin(&headers);
    let mut body = json!({
        "resource": resource(&origin, &service, &subpath),
        "bearer_methods_supported": ["header"],
        "resource_name": service,
    });
    let servers = state
        .authorization_servers
        .get()
        .cloned()
        .unwrap_or_default();
    if !servers.is_empty() {
        body["authorization_servers"] = json!(servers);
    }
    if let Some(docs) = config.interface.as_ref().and_then(|i| i.docs.clone()) {
        body["resource_documentation"] = json!(docs);
    }
    Json(body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_follows_the_forwarding_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "10.0.0.5:8080".parse().unwrap());
        assert_eq!(public_origin(&headers), "http://10.0.0.5:8080");
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "gw.example.com".parse().unwrap());
        assert_eq!(public_origin(&headers), "https://gw.example.com");
    }
}
