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

### From an app package

An app manifest can declare `operation_rules` with the same shape. Install copies them into the service's catalog entry and validates them, so a bad pattern fails the install.

This is how the database service package gives each table and query its own operation name for `permissions`. See [[database#Gateway setup]].

## Caller identity

The gateway forwards who is calling as headers derived from the verified token, never from the client. Client-supplied copies are stripped.

[[src/routing/proxy.rs#apply_caller_identity]] sets `x-amoeba-user` (token `sub`), `x-amoeba-org` (when the token has an org), and `x-amoeba-roles` (roles joined with `,`; sent even when empty). [[src/routing/proxy.rs#is_gateway_identity_header]] lists these plus every `x-amoeba-secret-*` header, and the proxy drops matching inbound headers before forwarding.

Upstream services such as the database service rely on `x-amoeba-roles` for column policy and tool filtering, so it must only ever come from the token.

## Local upstreams

A service with `local_upstream: true` is called as a local client: the browser's `Origin` is dropped and `Host` is `localhost:<port>`.

Some upstreams refuse anything that looks like a browser or a remote name, for example Lightpanda's MCP server answers `403 Origin not allowed` to any request carrying `Origin`, as DNS-rebinding defense. Behind the gateway that check is moot, because the gateway is the public endpoint and authenticates the caller.

[[src/routing/proxy.rs#forward_client_headers]] applies it. It is off by default because apps that answer CORS themselves need the browser's `Origin`.

### From an app package

`local_upstream: true` in `amoeba.yaml` is copied to the catalog service. The console's deploy form sets it with "Call as a local client".

## Upstream credentials

`upstream_auth` injects a credential the caller never sees: `bearer_static` sets `Authorization: Bearer <token>`, `custom_header` sets `header_name: <token>`.

The token comes from `token_secret` (a `"<service>/<key>"` file in the secrets directory, read per request via [[src/lifecycle/driver.rs#resolve_secret_ref]]) when set, else from the `token_env_var` env var on the Amoeba process. App packages use `token_secret`; see [[secrets#Gateway credential from a secret]].

Implemented by [[src/routing/proxy.rs#apply_upstream_auth]]. Per-request secret headers from `header_from_secret` are applied right after it.

### Fails closed

A configured credential that cannot be produced rejects the request with `503` and `x-amoeba-rejection-reason: secret-unavailable` instead of forwarding.

This covers an unset or empty env var, a missing or empty `token_secret` file, a missing token source or `header_name`, an unknown `type`, or a value that is not a valid header. The old behaviour sent `Bearer ` with an empty token.

## Service status

`GET /admin/apps/<name>` reports the install lifecycle (`installed`, `pulling`, `ready`, `error` with a message), a `runtime` block (`running`, `active_requests`, `last_request_at`, `port`, `public`) and the package's `interface`.

[[src/routing/apps_admin.rs#app_status]] builds it. The console's table maps it with [[web/src/lib/services.ts#toServiceItem]] to Running, Preparing, Idle or Error, and shows the error message and port.

### Run state

The gateway records whether it last started or stopped each workload; `running` is `null` until it has done either, e.g. after a gateway restart.

[[src/lifecycle/container.rs#should_stop]] keeps the reaper from stopping a workload it already stopped. Before this, an idle service was "scaled to zero" again on every 10-second sweep.

### Not ready names the port

When nothing answers on the service's port, the error says which port and, if the image suggests another, which one to use instead.

[[src/apps/manager.rs#ports_from_image_config]] reads `EXPOSE`d ports and `--port N`/`--port=N`/`-p N` from the image's entrypoint and command, since many images (Lightpanda's among them) skip `EXPOSE`. [[src/apps/manager.rs#not_ready_message]] words the error; [[src/apps/manager.rs#spawn_readiness_probe]] sets it on the app after inspecting the image with `docker image inspect` or, on Apple containers, `container image inspect`. The proxy returns these reasons as the 503 body, not just the `x-amoeba-rejection-reason` header, so the console's Test dialog and curl show them.
