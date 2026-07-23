//! Extracts service_name/subpath from the incoming path and dispatches upstream.

use super::operation::{classify_operation, has_permission};
use crate::auth::Claims;
use crate::config::schema::ServiceConfig;
use crate::lifecycle::container::ServiceRuntimeState;
use crate::metering::telemetry::emit_usage_telemetry;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header::AUTHORIZATION, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use std::sync::{atomic::Ordering, Arc};
use std::time::Instant;
use tracing::info;

/// Builds the upstream URL for a resolved service and request subpath.
pub fn build_target_url(ip: &str, port: u16, subpath: &str) -> String {
    format!("http://{ip}:{port}/{subpath}")
}

/// Applies configured upstream credential injection (token translation) to an outbound request.
pub fn apply_upstream_auth(
    mut outbound_req: reqwest::RequestBuilder,
    service_cfg: &ServiceConfig,
) -> reqwest::RequestBuilder {
    let Some(auth_cfg) = &service_cfg.upstream_auth else {
        return outbound_req;
    };

    match auth_cfg.r#type.as_str() {
        "bearer_static" => {
            if let Some(env_var) = &auth_cfg.token_env_var {
                let token = std::env::var(env_var).unwrap_or_default();
                outbound_req = outbound_req.header(AUTHORIZATION, format!("Bearer {token}"));
            }
        }
        "custom_header" => {
            if let (Some(hdr), Some(env_var)) = (&auth_cfg.header_name, &auth_cfg.token_env_var) {
                let val = std::env::var(env_var).unwrap_or_default();
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(hdr.as_bytes()),
                    HeaderValue::from_str(&val),
                ) {
                    outbound_req = outbound_req.header(name, value);
                }
            }
        }
        _ => {}
    }

    outbound_req
}

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

    // 2. Extract Claims from Auth Middleware (cloned to release the `req` borrow early)
    let claims = req
        .extensions()
        .get::<Claims>()
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // 3. Classify Operation (HTTP Method -> Operation Name)
    let operation = classify_operation(req.method().as_str());

    // 4. ACL Check: Evaluate Roles against Permissions Matrix
    let allowed_roles = service_cfg
        .permissions
        .get(operation)
        .cloned()
        .unwrap_or_default();

    if !has_permission(&claims, &allowed_roles) {
        info!(
            user = %claims.sub,
            service = %service_name,
            operation = %operation,
            "⛔ Access denied (403 Forbidden)"
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
    let target_url = build_target_url(&service_cfg.ip, service_cfg.port, &subpath);
    let mut outbound_req = state.http_client.request(req.method().clone(), &target_url);

    // 7. Handle Upstream Token Translation / Credential Injection
    outbound_req = apply_upstream_auth(outbound_req, service_cfg);

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
            emit_usage_telemetry(
                &claims.sub,
                claims.org_id.as_deref(),
                &service_name,
                operation,
                status.as_u16(),
                start_time.elapsed().as_millis(),
            );

            Ok(Response::builder()
                .status(status)
                .body(Body::from(res_bytes))
                .unwrap())
        }
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::UpstreamAuth;
    use std::collections::HashMap;

    fn base_service_cfg(upstream_auth: Option<UpstreamAuth>) -> ServiceConfig {
        ServiceConfig {
            image: "img".into(),
            ip: "127.0.0.1".into(),
            port: 8080,
            cooldown_seconds: None,
            operation_rules: None,
            permissions: HashMap::new(),
            upstream_auth,
        }
    }

    #[test]
    fn builds_upstream_target_url() {
        assert_eq!(
            build_target_url("192.168.100.20", 11434, "v1/chat/completions"),
            "http://192.168.100.20:11434/v1/chat/completions"
        );
    }

    #[test]
    fn leaves_request_untouched_without_upstream_auth() {
        let cfg = base_service_cfg(None);
        let client = reqwest::Client::new();
        let req = apply_upstream_auth(client.get("http://127.0.0.1:8080/x"), &cfg);
        let built = req.build().unwrap();
        assert!(built.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn injects_bearer_static_upstream_auth() {
        unsafe { std::env::set_var("AMOEBA_TEST_UPSTREAM_TOKEN", "secret-token") };
        let cfg = base_service_cfg(Some(UpstreamAuth {
            r#type: "bearer_static".into(),
            token_env_var: Some("AMOEBA_TEST_UPSTREAM_TOKEN".into()),
            header_name: None,
        }));

        let client = reqwest::Client::new();
        let req = apply_upstream_auth(client.get("http://127.0.0.1:8080/x"), &cfg);
        let built = req.build().unwrap();

        assert_eq!(built.headers().get(AUTHORIZATION).unwrap(), "Bearer secret-token");
    }

    #[test]
    fn injects_custom_header_upstream_auth() {
        unsafe { std::env::set_var("AMOEBA_TEST_UPSTREAM_HEADER_TOKEN", "internal-key") };
        let cfg = base_service_cfg(Some(UpstreamAuth {
            r#type: "custom_header".into(),
            token_env_var: Some("AMOEBA_TEST_UPSTREAM_HEADER_TOKEN".into()),
            header_name: Some("X-Internal-Key".into()),
        }));

        let client = reqwest::Client::new();
        let req = apply_upstream_auth(client.get("http://127.0.0.1:8080/x"), &cfg);
        let built = req.build().unwrap();

        assert_eq!(
            built.headers().get("X-Internal-Key").unwrap(),
            "internal-key"
        );
    }
}
