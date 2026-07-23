//! Per-request usage extraction and emission.

use tracing::info;

/// Emits a structured usage telemetry record for a completed proxy request.
pub fn emit_usage_telemetry(
    user_id: &str,
    org_id: Option<&str>,
    service: &str,
    operation: &str,
    status: u16,
    latency_ms: u128,
) {
    info!(
        target: "amoeba_usage_telemetry",
        user_id = %user_id,
        org_id = %org_id.unwrap_or("none"),
        service = %service,
        operation = %operation,
        status = %status,
        latency_ms = %latency_ms,
        "API execution processed"
    );
}
