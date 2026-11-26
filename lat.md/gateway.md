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

## Query strings reach the app

The caller's query string is passed to the app unchanged; before, the proxy sent only the path, so `?status=overdue` filters and URL tokens never arrived.

[[src/routing/proxy.rs#with_query]] appends it to the upstream URL, for plain requests and WebSocket handshakes alike.

## Signed identity for apps

Every authenticated request also carries `x-amoeba-identity`, an EdDSA token with the caller's user, org and roles for that one service, so an app can verify who is calling instead of trusting plain headers.

The plain `x-amoeba-user`/`-org`/`-roles` headers prove nothing to an app another app can reach directly: anyone can send them. [[src/auth/identity.rs#issue]] signs the token with an Ed25519 key kept in `/etc/amoeba/identity_key` (`AMOEBA_IDENTITY_KEY_FILE`), `aud` = the service, `iss` = `amoeba`, five minutes long. Every app container gets the public key as `AMOEBA_IDENTITY_PUBLIC_KEY` and its name as `AMOEBA_SERVICE_NAME` ([[src/lifecycle/sandbox.rs#identity_env]]). The database service requires the token once it has the key (see [[database#Gateway setup#Signed identity]]). A copy a caller sends is dropped.

### Apps verify the gateway's signature

A token verifies only with the gateway's public key and only for the service it names; one for another service, signed by another key, or made up with HMAC is refused.

### Every request carries it

Through the gateway an app receives a token naming the caller, not the forged copy the caller sent.

## Gateway is the only way in

Each sandboxed app gets a Docker network of its own, shared only with the gateway, so one app cannot reach another's port and skip the gateway's checks.

Before this, every app shared `amoeba-net`, so an AI-built app could call the database service directly with made-up identity headers. [[src/lifecycle/docker_container.rs#DockerContainerDriver]] creates `amoeba-app-<name>`, always internal, and attaches the gateway's container to it (`AMOEBA_SELF_CONTAINER`, else its hostname). An app with egress also joins the shared `amoeba-egress` network, whose bridge has inter-container traffic off; the gateway never joins it. An earlier draft made egress apps' own networks non-internal, and Docker then picked one as the gateway's default route, so removing it cut the gateway's connections. Without a gateway container to attach (native mode), it falls back to the shared network with a warning. Apple containers get a host-only network per app instead. Services without a sandbox (written by hand, or installed before this) stay on the shared network until reinstalled. Uninstalling removes the network. Each network takes an address range from Docker's pools, which by default allow about 30 bridge networks per host.

### Compose apps get their own network

At install, a compose app's external `amoeba-net` network is renamed to the app's own ([[src/lifecycle/sandbox.rs#isolate_compose_network]]); the compose driver creates it and attaches the gateway before `up`.

## Sandbox

Apps installed from a package run confined unless their manifest relaxes it: the settings suit code an AI just wrote and nobody has reviewed.

[[src/config/schema.rs#Sandbox]] holds them; both container drivers apply them. Apple containers get the same flags (`--read-only`, `--tmpfs`, `--cap-drop`, `--ulimit nproc`, `--user`) and a host-only network.

### Defaults for new installs

An install gets a read-only root, no outbound network, no root user and 256 processes unless the manifest's `sandbox` says otherwise; unsafe values (adding `ALL` or `SYS_ADMIN`, odd limits, unknown keys) are refused.

### What the container gets

A sandboxed Docker container has no capabilities beyond those listed, no privilege escalation, a read-only root and limits on processes and memory; a service without a sandbox gets none of this.

Docker drops all capabilities, adds back only those listed, sets `no-new-privileges`, a read-only root with tmpfs `/tmp`, `/run` and `/var/tmp`, a process limit, a 1 GiB memory limit when none is declared, and user 65534 for images that run as root ([[src/lifecycle/sandbox.rs#run_as_user]]).

## MCP sign-in discovery

A 401 from a service carries `WWW-Authenticate: Bearer resource_metadata="…"`, and the gateway serves OAuth Protected Resource Metadata (RFC 9728) per service, as the MCP authorization spec requires.

[[src/routing/resource_metadata.rs#metadata]] answers `/.well-known/oauth-protected-resource/v1/<service>/<path>` with the `resource`, `bearer_methods_supported` and `authorization_servers` (config `auth.authorization_servers`, else `auth.issuer`). The origin follows `X-Forwarded-Proto`/`-Host` ([[src/routing/resource_metadata.rs#public_origin]]). Amoeba is not an authorization server itself; in local mode no servers are listed and clients use issued tokens.

## MCP tools

The gateway reads JSON-RPC `tools/call` requests to an MCP app, so permissions can differ per tool and a tool that changes after approval is blocked.

[[src/apps/tools.rs]] does both. The body is read early only for services with `tool:` permissions or pinned tools, and forwarded unchanged.

### Per-tool permissions

A call to tool `x` also needs the roles under `tool:x` (else `tool:*`), on top of the request's own operation, in one request or a batch; so analysts may search invoices while only admins delete them.

### Changed tools are blocked

The gateway fingerprints an MCP app's tools when it is first ready and reads them again after each start and every five minutes; a reworded or new tool is refused (403 `tool-not-approved`) until an admin approves the current list.

Fingerprints are SHA-256 of each tool's JSON ([[src/apps/tools.rs#fingerprint]]), kept in the catalog as [[src/config/schema.rs#ToolPins]]. `GET /admin/apps/<name>/tools` shows them and `POST /admin/apps/<name>/tools/approve` approves; the console shows changed tools with an Approve button. Reinstalling an app re-pins its tools, since a reinstall is itself an admin's decision.

## Rate limits

Failed sign-ins are limited per account and per client address, and requests per caller per service, in the gateway's memory.

[[src/routing/ratelimit.rs#Limits]] keeps both. The client address is the peer, or the first `X-Forwarded-For` entry when the peer is a local or private-network proxy ([[src/routing/ratelimit.rs#client_address]]).

### Per caller per service

Each signed-in user (or client address, for public services) may make 600 requests a minute to a service by default (`AMOEBA_RATE_LIMIT_PER_MINUTE`, or the service's `rate_limit_per_minute`; 0 is unlimited), then gets 429 with `Retry-After`.

### Failed sign-ins

After five failed sign-ins for an account (or twenty from one address) in fifteen minutes, `POST /auth/login` answers 429 with `Retry-After`, even for the right password; a success clears the account's count.

## Calls for the caller

An app that declares `calls` gets `x-amoeba-caller-token` on each request: the caller's identity, usable only on those services, so a downstream service checks the original caller's roles.

The use is an app such as a Playwright or Stagehand scraper driving a hosted browser through the gateway: the browser should admit whoever may use it, not the scraper. [[src/auth/delegation.rs#issue]] signs the token with a key the gateway generates at startup (it cannot sign with an identity provider's key), carrying the caller's `sub`, org, roles and `jti`, `svc` (the declared services) and `via` (the app). It lasts at most an hour and never past the caller's token. [[src/auth/mod.rs#JwtEngine#verify_token]] picks that key by the token's `kid`, and [[src/routing/proxy.rs#proxy_handler]] refuses it outside `svc` ([[src/auth/delegation.rs#allows]]) and logs the caller as `alice via scraper`.

### Downstream checks the caller

Through the app, a service sees the caller's user and roles, and refuses a caller whose roles it does not grant, exactly as it would directly.

### Only the declared services

The token is refused (403) on a service the app did not list in `calls`, even one the caller may use directly.

### Only for apps that declare calls

Apps without `calls` never receive the header, and a copy sent by the caller is dropped, so an app cannot be handed a forged token.

### Revoked with the caller

The token carries the caller's `jti`, so revoking the caller's token makes it fail with 401 too.

## WebSockets

A WebSocket handshake gets the same checks as any request, then the connection is handed to the app and passed through byte for byte, so browser automation (CDP), notebooks and terminals work behind Amoeba.

[[src/routing/websocket.rs#is_websocket_upgrade]] spots the handshake in [[src/routing/proxy.rs#proxy_handler]] after auth, the role check (a `GET`, so `read` unless an operation rule says otherwise), waking the app and header rewriting. [[src/routing/websocket.rs#tunnel]] sends the handshake to the app over TCP; on `101` it answers the caller and joins the two connections until either closes. Frames are not parsed, so any subprotocol works. The proxy's usual header rewriting applies: no caller token, verified `x-amoeba-*` identity, injected credentials.

### Messages pass both ways

Text and binary messages reach the app and come back unchanged, the app sees the path and query, and the open session counts as an active connection until it closes, so the reaper never stops the app under a caller.

### Same checks as any request

A handshake without a token gets 401 and one without a permitted role gets 403, and neither reaches the app.

### Local-only apps

With `local_upstream`, the app sees `Host: localhost:<port>` and no `Origin`, which Chrome's and Lightpanda's CDP servers require.

### Refused upgrades

When the app answers the handshake with anything but `101`, the caller gets that answer and the session slot is released.

## Service status

`GET /admin/apps/<name>` reports the install lifecycle (`installed`, `pulling`, `ready`, `error` with a message), a `runtime` block (`running`, `active_requests`, `last_request_at`, `port`, `public`) and the package's `interface`.

It also reports the app's `source`, current `settings` and `overrides` (see [[deploy#App library#Name conflicts and settings]]).

[[src/routing/apps_admin.rs#app_status]] builds it. The console's table maps it with [[web/src/lib/services.ts#toServiceItem]] to Running, Preparing, Idle or Error, and shows the error message and port. Each row's Settings button opens [[web/src/components/ServiceSettingsModal.tsx#ServiceSettingsModal]].

### Run state

The gateway records whether it last started or stopped each workload; `running` is `null` until it has done either, e.g. after a gateway restart.

[[src/lifecycle/container.rs#should_stop]] keeps the reaper from stopping a workload it already stopped. Before this, an idle service was "scaled to zero" again on every 10-second sweep.

The proxy starts a workload on a fresh activation, and also whenever the gateway knows it stopped it. Without the second case an always-on service (no idle stop) counts as active forever, so after a stop, such as a restart for a settings change, its requests got 502 instead of starting it.

### Reloads keep app states

An install's lifecycle state must survive the catalog reloads it triggers, or the app reports "installed" forever while its readiness probe updates a state no one reads.

Each install reloads twice: the handler's reload, then the file watcher's. Both, and `set_app_state`, used to copy the state map, change it and store it, so the later writer dropped the other's change; a nightly e2e saw `streaming-echo` stuck that way. [[src/state.rs#AppState#apply_catalog]], `set_app_state` and `remove_app_state` now update with `ArcSwap::rcu`, which retries on a concurrent write.

### Not ready names the port

When nothing answers on the service's port, the error says which port and, if the image suggests another, which one to use instead.

[[src/apps/manager.rs#ports_from_image_config]] reads `EXPOSE`d ports and `--port N`/`--port=N`/`-p N` from the image's entrypoint and command, since many images (Lightpanda's among them) skip `EXPOSE`. [[src/apps/manager.rs#not_ready_message]] words the error; [[src/apps/manager.rs#spawn_readiness_probe]] sets it on the app after inspecting the image with `docker image inspect` or, on Apple containers, `container image inspect`. The proxy returns these reasons as the 503 body, not just the `x-amoeba-rejection-reason` header, so the console's Test dialog and curl show them.
