//! Extracts service_name/subpath from the incoming path and dispatches upstream.

use super::operation::{classify_operation, has_permission};
use crate::auth::middleware::verify_request_token;
use crate::auth::Claims;
use crate::capacity::limiter::{fits_within_budget, sum_resource_usage};
use crate::config::schema::ServiceConfig;
use crate::lifecycle::container::{is_occupying_capacity, now_unix, ServiceRuntimeState};
use crate::metering::telemetry::emit_usage_telemetry;
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header::AUTHORIZATION, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::time::timeout;
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

/// Bounded poll-connect against the upstream host:port, used as the
/// dependency-free readiness check after a driver reports a service started
/// (works uniformly across the container/compose-file/inline-stack_spec
/// scenarios, since all of them expose a plain TCP port). The overall budget
/// is configurable via `AMOEBA_READINESS_TIMEOUT_MS` (default 30s) so tests
/// exercising an intentionally-unreachable backend aren't stuck waiting.
async fn wait_until_ready(host: &str, port: u16) -> bool {
    let budget_ms: u64 = std::env::var("AMOEBA_READINESS_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    let per_attempt = Duration::from_millis(250);

    loop {
        if let Ok(Ok(_)) = timeout(per_attempt, TcpStream::connect((host, port))).await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
    runtime.active_connections.fetch_add(1, Ordering::Relaxed);

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
                catalog.machines.get(machine_name).map(|m| (machine_name, m))
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
            // Roll back the increment above: this request never actually
            // proceeds, so it must not permanently inflate the counter.
            runtime.active_connections.fetch_sub(1, Ordering::Relaxed);
            return Ok(rejection(StatusCode::SERVICE_UNAVAILABLE, "machine-capacity"));
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
                    runtime.active_connections.fetch_sub(1, Ordering::Relaxed);
                    error!(service = %service_name, "failed to start service: {e}");
                    return Ok(rejection(StatusCode::SERVICE_UNAVAILABLE, "driver-start-failed"));
                }
            }
        }

        let host = runtime.resolved_host().unwrap_or_else(|| service_cfg.upstream_host().to_string());
        if !wait_until_ready(&host, service_cfg.placement.port).await {
            runtime.active_connections.fetch_sub(1, Ordering::Relaxed);
            error!(service = %service_name, "service did not become ready in time");
            return Ok(rejection(StatusCode::SERVICE_UNAVAILABLE, "driver-not-ready"));
        }
    }

    // 6. Build Upstream Request URL. `resolved_host` is populated whenever a
    // prior cold-boot (this request's or an earlier one, for an
    // already-warm service) resolved a dynamic host; otherwise falls back to
    // the static `placement.ip`/`primary_service` config value.
    let host = runtime.resolved_host().unwrap_or_else(|| service_cfg.upstream_host().to_string());
    let target_url = build_target_url(&host, service_cfg.placement.port, &subpath);
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
                claims.as_ref().map(|c| c.sub.as_str()).unwrap_or("anonymous"),
                claims.as_ref().and_then(|c| c.org_id.as_deref()),
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
            container: Some(ContainerSpec { image: "img".into(), resources: None }),
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
