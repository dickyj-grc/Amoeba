//! Extracts service_name/subpath from the incoming path and dispatches upstream.

use super::operation::{classify_operation, has_permission};
use crate::apps::state::AppLifecycleState;
use crate::auth::Claims;
use crate::auth::middleware::verify_request_token;
use crate::capacity::limiter::{fits_within_budget, sum_resource_usage};
use crate::config::schema::ServiceConfig;
use crate::lifecycle::container::{
    ServiceRuntimeState, is_occupying_capacity, now_unix, wait_until_ready,
};
use crate::metering::telemetry::emit_usage_telemetry;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderName, HeaderValue, StatusCode, header::AUTHORIZATION, header::HOST},
    response::Response,
};
use futures_util::StreamExt;
use std::sync::{Arc, atomic::Ordering};
use std::time::Instant;
use tracing::{error, info};

/// Builds the upstream URL for a resolved service and request subpath.
pub fn build_target_url(host: &str, port: u16, subpath: &str) -> String {
    format!("http://{host}:{port}/{subpath}")
}

/// Header carrying *why* a request was rejected. `503` alone is ambiguous
/// between distinct fresh-activation failure modes (machine over capacity vs.
/// the driver failing to start the service vs. it never becoming ready) that
/// operators — and this module's own tests — need to tell apart.
const REJECTION_REASON_HEADER: &str = "x-amoeba-rejection-reason";

fn rejection(status: StatusCode, reason: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header(REJECTION_REASON_HEADER, reason)
        .body(Body::empty())
        .unwrap()
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

/// RAII guard that increments `active_connections` on creation and decrements
/// on drop. Moved into the response body stream so the connection is counted
/// until the client finishes reading (or disconnects), not just until response
/// headers arrive. This makes scale-to-zero safe for streamed/SSE responses.
struct ConnGuard(Arc<ServiceRuntimeState>);

impl ConnGuard {
    fn acquire(runtime: Arc<ServiceRuntimeState>) -> Self {
        runtime.active_connections.fetch_add(1, Ordering::Relaxed);
        Self(runtime)
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Hop-by-hop headers that must not be blindly forwarded between client and
/// upstream. Dropping these prevents connection-management confusion (e.g.
/// `Transfer-Encoding: chunked` from upstream being misinterpreted by the
/// gateway) and avoids leaking proxy internals.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    Path((service_name, subpath)): Path<(String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    let start_time = Instant::now();

    // 1. Resolve target service configuration. Cloned out of the ArcSwap guard
    // immediately: `arc_swap::Guard` must not be held across `.await` points
    // (its own docs warn this can stall a concurrent writer), and this handler
    // awaits several times further down (auth, driver boot, the proxied call
    // itself).
    let service_cfg: ServiceConfig = {
        let catalog = state.catalog.load();
        catalog
            .services
            .get(&service_name)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let service_cfg = &service_cfg;

    // 1.5. Fail fast if the app is known but not yet ready. This prevents the
    // first request from blocking on a slow or failing image pull.
    if let Some(app_state) = state.app_state(&service_name) {
        if !app_state.is_ready() {
            let reason = if app_state.is_error() {
                "app-error"
            } else {
                "app-not-ready"
            };
            return Ok(rejection(StatusCode::SERVICE_UNAVAILABLE, reason));
        }
    } else {
        // Services that pre-date the lifecycle state map are treated as ready
        // so existing config-file services keep working without an explicit
        // install step. They will get a lifecycle entry on the next catalog
        // reload and be probed then.
        state.set_app_state(&service_name, Arc::new(AppLifecycleState::ready()));
    }

    // 2. Authenticate, unless this service opted out via "public": true
    let claims: Option<Claims> = if service_cfg.public {
        None
    } else {
        Some(verify_request_token(&state.jwt_engine, req.headers()).await?)
    };

    // 3. Classify Operation (HTTP Method -> Operation Name)
    let operation = classify_operation(req.method().as_str());

    // 4. ACL Check: Evaluate Roles against Permissions Matrix (skipped for public services)
    if let Some(claims) = &claims {
        let allowed_roles = service_cfg
            .permissions
            .get(operation)
            .cloned()
            .unwrap_or_default();

        if !has_permission(claims, &allowed_roles) {
            info!(
                user = %claims.sub,
                service = %service_name,
                operation = %operation,
                "⛔ Access denied (403 Forbidden)"
            );
            return Err(StatusCode::FORBIDDEN);
        }
    }

    // 5. Track Runtime Activity & Connection Counter. `runtime` is an owned
    // `Arc` clone (independent of any guard), but resolving it and computing
    // sibling usage below still needs a short-lived, synchronous-only borrow
    // of `runtime_states`/`catalog` — scoped tightly so neither guard is ever
    // held across the `.await` points further down.
    let runtime = {
        let runtimes = state.runtime_states.load();
        runtimes
            .get(&service_name)
            .cloned()
            .unwrap_or_else(|| Arc::new(ServiceRuntimeState::new()))
    };

    let now = now_unix();
    let previous_active = runtime.active_connections.load(Ordering::Relaxed);
    let previous_last_accessed = runtime.last_accessed_unix.load(Ordering::Relaxed);
    let previous_has_activated = runtime.has_activated.load(Ordering::Relaxed);

    runtime.touch();
    // Acquire the connection guard first; every early-return path below simply
    // drops it, which decrements the counter automatically.
    let guard = ConnGuard::acquire(runtime.clone());

    let was_already_occupying = is_occupying_capacity(
        service_cfg.placement.cooldown_seconds,
        previous_has_activated,
        previous_active,
        previous_last_accessed,
        now,
    );

    // A fresh activation: 5.5 gate against the machine's declared capacity
    // budget, then (Container Boot Check) make sure the driver has actually
    // started the service and wait for it to accept connections, before ever
    // attempting to proxy to it.
    if !was_already_occupying {
        let capacity_rejected = {
            let catalog = state.catalog.load();
            let runtimes = state.runtime_states.load();

            match service_cfg.machine_name(&catalog).and_then(|machine_name| {
                catalog
                    .machines
                    .get(machine_name)
                    .map(|m| (machine_name, m))
            }) {
                Some((machine_name, machine)) => {
                    let want = service_cfg.resource_footprint();

                    let sibling_specs: Vec<_> = catalog
                        .services
                        .iter()
                        .filter(|(other_name, other_cfg)| {
                            *other_name != &service_name
                                && other_cfg.machine_name(&catalog) == Some(machine_name)
                        })
                        .filter_map(|(other_name, other_cfg)| {
                            let other_runtime = runtimes.get(other_name)?;
                            let occupying = is_occupying_capacity(
                                other_cfg.placement.cooldown_seconds,
                                other_runtime.has_activated.load(Ordering::Relaxed),
                                other_runtime.active_connections.load(Ordering::Relaxed),
                                other_runtime.last_accessed_unix.load(Ordering::Relaxed),
                                now,
                            );
                            occupying.then(|| other_cfg.resource_footprint())
                        })
                        .collect();

                    let used = sum_resource_usage(sibling_specs.iter());

                    if !fits_within_budget(&machine.budget(), &used, &want) {
                        info!(
                            service = %service_name,
                            machine = %machine_name,
                            "🚫 Rejecting request: machine at capacity"
                        );
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };

        if capacity_rejected {
            // The guard drops here, rolling back the connection counter.
            return Ok(rejection(
                StatusCode::SERVICE_UNAVAILABLE,
                "machine-capacity",
            ));
        }

        let driver = state.drivers.load().get(&service_name).cloned();
        if let Some(driver) = driver {
            match driver.ensure_started().await {
                // Only apple-container reports a host here (a fresh IP on every
                // start, no stable DNS name) -- cache it for this and future
                // (warm) requests. Docker/Compose return `None`: their static
                // `placement.ip`/`primary_service` is already correct.
                Ok(Some(host)) => runtime.set_resolved_host(host),
                Ok(None) => {}
                Err(e) => {
                    error!(service = %service_name, "failed to start service: {e}");
                    return Ok(rejection(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "driver-start-failed",
                    ));
                }
            }
        }

        let host = runtime
            .resolved_host()
            .unwrap_or_else(|| service_cfg.upstream_host().to_string());
        if !wait_until_ready(&host, service_cfg.placement.port).await {
            error!(service = %service_name, "service did not become ready in time");
            return Ok(rejection(
                StatusCode::SERVICE_UNAVAILABLE,
                "driver-not-ready",
            ));
        }
    }

    // 6. Build Upstream Request URL. `resolved_host` is populated whenever a
    // prior cold-boot (this request's or an earlier one, for an
    // already-warm service) resolved a dynamic host; otherwise falls back to
    // the static `placement.ip`/`primary_service` config value.
    let host = runtime
        .resolved_host()
        .unwrap_or_else(|| service_cfg.upstream_host().to_string());
    let target_url = build_target_url(&host, service_cfg.placement.port, &subpath);
    let mut outbound_req = state.http_client.request(req.method().clone(), &target_url);

    // 6.5. Forward client headers before consuming the body. Skip hop-by-hop
    // headers, HOST (reqwest sets the upstream host), and AUTHORIZATION (handled
    // by upstream_auth injection below). SSE/MCP clients need Accept,
    // Mcp-Session-Id, Last-Event-ID, etc. to reach the upstream.
    for (name, value) in req.headers() {
        if !is_hop_by_hop(name) && name != HOST && name != AUTHORIZATION {
            outbound_req = outbound_req.header(name, value);
        }
    }

    // 7. Handle Upstream Token Translation / Credential Injection
    outbound_req = apply_upstream_auth(outbound_req, service_cfg);

    // Forward Request Payload Body
    let body_bytes = axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    outbound_req = outbound_req.body(body_bytes);

    // 8. Execute Forward Proxy Request
    let response = outbound_req.send().await;

    match response {
        Ok(res) => {
            let status = res.status();

            // 9. Emit Usage Telemetry Log (at response-headers time; body may
            // stream for minutes for SSE).
            emit_usage_telemetry(
                claims
                    .as_ref()
                    .map(|c| c.sub.as_str())
                    .unwrap_or("anonymous"),
                claims.as_ref().and_then(|c| c.org_id.as_deref()),
                &service_name,
                operation,
                status.as_u16(),
                start_time.elapsed().as_millis(),
            );

            // Copy response headers before consuming the body.
            let response_headers: Vec<_> = res
                .headers()
                .iter()
                .filter(|(name, _)| !is_hop_by_hop(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();

            // 10. Stream the response body back to the client. The connection
            // guard is moved into the stream closure so `active_connections`
            // stays incremented until the client finishes reading or disconnects.
            // `touch()` on each chunk advances the idle clock, so the reaper only
            // scales down silent streams, not active ones.
            let rt = runtime.clone();
            let stream = res.bytes_stream().inspect(move |_| {
                let _ = &guard; // hold the guard for the lifetime of the stream
                rt.touch();
            });
            let body = Body::from_stream(stream);

            let mut builder = Response::builder().status(status);
            for (name, value) in response_headers {
                builder = builder.header(name, value);
            }
            builder
                .body(body)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{ContainerSpec, Placement, UpstreamAuth};
    use std::collections::HashMap;

    fn base_service_cfg(upstream_auth: Option<UpstreamAuth>) -> ServiceConfig {
        ServiceConfig {
            placement: Placement {
                r#type: "vm".into(),
                ip: Some("127.0.0.1".into()),
                primary_service: None,
                port: 8080,
                cooldown_seconds: None,
                machine: None,
            },
            container: Some(ContainerSpec {
                image: "img".into(),
                resources: None,
                env: HashMap::new(),
            }),
            stack_spec: None,
            operation_rules: None,
            permissions: HashMap::new(),
            upstream_auth,
            public: false,
            env_from_secret: HashMap::new(),
        }
    }

    #[test]
    fn builds_upstream_target_url() {
        assert_eq!(
            build_target_url("gemma4", 11434, "v1/chat/completions"),
            "http://gemma4:11434/v1/chat/completions"
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

        assert_eq!(
            built.headers().get(AUTHORIZATION).unwrap(),
            "Bearer secret-token"
        );
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
