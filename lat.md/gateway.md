# Gateway

The proxy at `/v1/<service>/...` authenticates the caller, names the operation, checks it against the service's role map, and forwards with gateway-owned identity and credentials.

Request flow lives in [[src/routing/proxy.rs#proxy_handler]]. The authorization model is in [[src/routing/operation.rs#authorize]].

## Operation resolution

Each request maps to one operation name, which is the key looked up in `permissions` or `tenant_permissions`. Rules let a service name operations like `loans.create`.

[[src/routing/operation.rs#resolve_operation]] walks the service's `operation_rules` in order and takes the first match. With no match it falls back to [[src/routing/operation.rs#classify_operation]] (`read`/`add`/`update`/`delete`).

Two rule kinds:

- `http_method`: `values` lists methods (case-insensitive). Must not be empty.
- `path`: `path` is a pattern over the subpath after `/v1/<service>/`; `values` optionally narrows methods, empty means any.

Pattern syntax ([[src/routing/operation.rs#path_matches]]): `{name}` and `*` match exactly one segment, a final `**` matches zero or more segments, and slashes at either end are ignored. `**` anywhere but last is rejected.

Rules are validated at load by [[src/routing/operation.rs#validate_operation_rules]], called from [[src/config/watcher.rs#validate_service_access]], so a typo fails the reload rather than silently never matching. Order matters: put specific rules before broad ones.

## Caller identity

The gateway forwards who is calling as headers derived from the verified token, never from the client. Client-supplied copies are stripped.

[[src/routing/proxy.rs#apply_caller_identity]] sets `x-amoeba-user` (token `sub`), `x-amoeba-org` (when the token has an org), and `x-amoeba-roles` (roles joined with `,`; sent even when empty). [[src/routing/proxy.rs#is_gateway_identity_header]] lists these plus every `x-amoeba-secret-*` header, and the proxy drops matching inbound headers before forwarding.

Upstream services such as the database service rely on `x-amoeba-roles` for column policy and tool filtering, so it must only ever come from the token.

## Upstream credentials

`upstream_auth` injects a credential the caller never sees: `bearer_static` sets `Authorization: Bearer <token>`, `custom_header` sets `header_name: <token>`. The token is read from `token_env_var`.

Implemented by [[src/routing/proxy.rs#apply_upstream_auth]]. Per-request secret headers from `header_from_secret` are applied right after it.

### Fails closed

A configured credential that cannot be produced rejects the request with `503` and `x-amoeba-rejection-reason: secret-unavailable` instead of forwarding.

This covers an unset or empty env var, a missing `token_env_var` or `header_name`, an unknown `type`, or a value that is not a valid header. The old behaviour sent `Bearer ` with an empty token.
