# Feature: Per-User / Per-Org License Usage Metering and Enforcement

## Summary

Amoeba already emits structured per-request telemetry (`amoeba_usage_telemetry`) containing `user_id`, `org_id`, `service`, `operation`, `status`, and `latency_ms`. This issue proposes building a license/usage analysis layer on top of those events so operators can answer questions like:

- How many API calls has `org_alpha` made this month?
- Has `user_dicky` exceeded their paid GPU-hour allowance?
- Which services consume the most resources across all tenants?
- Should the next request be rejected because the license limit is reached?

## Motivation / Use Cases

1. **SaaS billing** — Charge customers based on actual compute consumption rather than flat seat licenses.
2. **License compliance** — Enforce hard or soft limits per user/org and surface overage reports.
3. **Capacity planning** — Understand which services and users drive load before upgrading hardware.
4. **Fair sharing** — Prevent a single tenant from monopolizing a shared GPU box.

## Current State

`src/metering/telemetry.rs` emits one log line per proxied request:

```json
{
  "target": "amoeba_usage_telemetry",
  "user_id": "dicky",
  "org_id": "org_hq",
  "service": "gemma4",
  "operation": "read",
  "status": 200,
  "latency_ms": 45
}
```

There is no aggregation, no license entitlement store, and no enforcement hook.

## Proposed Scope

### Phase 1: Usage Aggregation (read-only)

- Define a `License` / `Entitlement` schema (e.g., requests per month, GPU-VRAM-seconds per month, tokens per month).
- Add a new config file or extend `users.json` / `services.json` to store entitlements per user or org.
- Provide an endpoint (e.g., `GET /admin/usage?org=org_hq&period=2026-07`) that returns aggregated usage for a given period.
- Keep aggregation lightweight; for a first pass, aggregate in memory from telemetry logs or a simple SQLite file.

### Phase 2: Enforcement

- Before proxying a request, check whether the caller's org has remaining quota for the requested service/operation.
- Return `429 Too Many Requests` or `402 Payment Required` when a limit is exceeded.
- Allow configurable behavior per license tier: hard reject vs. allow-with-warning.

### Phase 3: Reporting and Export

- Add a `/admin/usage/export` endpoint or CLI command for CSV/JSON export.
- Optionally stream events to an external billing system (Stripe, Metronome, etc.).

## Open Questions

1. Should metering be per-user, per-org, or both?
2. What dimensions matter most for billing: request count, wall-clock time, GPU/VRAM time, input/output tokens, or a combination?
3. Should usage data be persisted to SQLite, a time-series database, or kept in memory with periodic flush?
4. How should overages be handled: reject immediately, queue for later, or allow with an alert?

## Acceptance Criteria

- [ ] A schema exists for license entitlements.
- [ ] Amoeba can aggregate `amoeba_usage_telemetry` events by user/org/service/operation over a time window.
- [ ] An admin endpoint returns current usage vs. entitlement.
- [ ] (Optional for Phase 1) A configurable enforcement hook rejects requests that would exceed a license limit.
- [ ] Documentation is updated to explain how to configure licenses and read usage reports.

## Related Code

- `src/metering/telemetry.rs` — existing telemetry emission.
- `src/routing/proxy.rs` — where enforcement would be wired.
- `src/auth/mod.rs` / `src/auth/users.rs` — where user/org identity lives.

---

Labels: `enhancement`, `billing/metering`, `help wanted`
