# Changelog

## Unreleased

### Console

- **Build progress.** Deploying from Git or a Dockerfile now shows elapsed time, the command running and the live build log, updated every 2 seconds, with a "Stop build" button. Before, the button just said "Building image..." until the build ended.
- **Real service status.** The table showed every service as "Idle (Scale-to-Zero)", because it read fields the gateway never sent. It now shows Running, Preparing, Idle or Error, with the error message and the service's port.
- **Less flicker.** The 3-second refresh no longer shows a loading state, skips re-rendering when nothing changed, and pauses while the tab is hidden. Services stay in a fixed order; before, they could reshuffle on every refresh.

### Gateway

- **`GET` and `DELETE /admin/apps/<name>/build`** report and cancel the latest build. A second build for the same name while one runs is `409`. Cancelling kills the build's whole process group.
- **`AMOEBA_BUILD_TIMEOUT_SECS`** overrides the 15-minute build timeout, for large projects.
- **`GET /admin/apps`** returns names sorted.
- **`GET /admin/apps/<name>`** adds a `runtime` block: `running`, `active_requests`, `last_request_at`, `port`, `public`.
- **Readiness errors name the port.** "nothing answered on port 8080", plus the port the image suggests when it differs, e.g. 9222 for `lightpanda/browser`, read from its `EXPOSE` or `--port` in its command. A 503 for an app that failed or is still preparing now carries the reason in its body.
- **The reaper stops an idle service once.** It used to stop an already-stopped service again on every 10-second sweep.

## 0.1.4 — 2026-11-11

### Fixes

- **Version numbers.** The 0.1.3 release was tagged before its version bump merged, so its binaries and console still reported 0.1.2. 0.1.4 reports its own version everywhere.

### Security

- **Console test runner upgraded to Vitest 4.1.12.** Clears all six Dependabot alerts (three critical) in `vitest`, `@vitest/mocker` and `tinypool`. They were dev-only: none of these ship in the images or the built console.

## 0.1.3 — 2026-11-11

### Service tokens for AI apps

- **Choose how long a token lives.** `POST /admin/apps/<name>/token` takes `ttl_seconds` (1 minute to 366 days; default still 1 hour) and a `label`. The console defaults to 30 days, since AI apps keep the token in their MCP settings.
- **List and revoke tokens.** `GET /admin/apps/<name>/tokens` lists unexpired tokens (label, org, expiry; never the token). `DELETE /admin/apps/<name>/tokens/<jti>` revokes one. Metadata lives in `issued_tokens.json` next to the catalog.
- **Console token dialog.** Shows the token once with a warning, copies it even on plain-http consoles, downloads a ready-made MCP client config, and lists active tokens with a Revoke button. It now sends `org_id`, which the server expects; the old role picker and org field had no effect.

### Gateway

- **`local_upstream`** in `amoeba.yaml` (console: "Call as a local client") makes the gateway drop the browser's `Origin` and send `Host: localhost:<port>`. Apps that only accept local callers, such as Lightpanda's MCP server, otherwise answer `403 Origin not allowed`.

## 0.1.2 — 2026-11-11

### Upgrade notes

- **New image: `ghcr.io/dickyj-grc/amoeba-db`.** The database service ships as its own image. GHCR creates new packages as private, so make it public, as `amoeba` is, before installing the `finance-db` example elsewhere.
- **Console dev server ports changed.** `npm run dev` in `web/` now serves on `3064` and proxies to the API on `8064`, matching `scripts/dev.sh`. Before, it used `3000` and `8080`.
- **Installs check the whole catalog.** An app install that would leave `services.json` invalid now fails with `400`. Before, the file was written, and the gateway silently kept serving the old catalog.

### Database service (new)

- `amoeba-db` holds one PostgreSQL connection and serves only approved definitions: tables opened for specific operations, and named queries. Callers send names and values, never SQL (#18).
- Per-role column policy (`show`, `mask`, `hide`). Masking happens in the generated SQL, so a masked value never leaves the database. Unlisted columns stay closed.
- Guardrails:
  - Reads run in `READ ONLY` transactions, on a separate read connection if one is configured.
  - Writes can use their own account (`DATABASE_URL_WRITE`).
  - Every statement has a timeout.
  - A write that touches more rows than its limit is rolled back.
  - Rows are scoped to the caller's org (`app.org`, `org_column`).
  - Every call writes one audit record.
- Every definition the caller's roles allow is also an MCP tool at `/mcp`.
- Definitions hot-reload from `DB_DEFINITIONS_FILE`. An invalid edit is logged, and the previous definitions stay in force.
- Example package `examples/app-packages/finance-db`, with demo data in `demo.sql`.

### Gateway

- App manifests can declare `operation_rules`, so a package can name its own operations (for example, one per table and query) and grant them separately (#18).
- `AMOEBA_SERVICES_FILE` overrides the catalog path (default `/etc/amoeba/services.json`) (#17).
- Install, uninstall and policy changes apply to the running gateway immediately, instead of waiting for the file watcher (#17).

### Apple `container` driver (macOS)

- Image apps install correctly on `apple-container` machines. Before, the install was silently ignored (#17).
- New apps no longer get stuck in `pulling`: the readiness probe uses the container's resolved IP and pulls with `container image pull` (#17).
- Plain env vars from the install now reach the container. Before, only secrets did (#17).

### Local development

- `scripts/dev.sh` runs Amoeba and the web console in one command, with the API on `localhost:8064` and the console on `localhost:3064`. On macOS with Apple's `container` CLI it runs natively, with no Docker Desktop VM; elsewhere it uses Docker Compose (#17).
- The README has a new Run Locally section, an intro built around Amoeba's two jobs, and a roadmap (#19).

### CI

- Nightly e2e builds and tests the image for its own commit, never `:latest`. A stale `:latest` had made a passing fix look broken (#16).
- A new CI job runs the database service's tests against a real Postgres. Releases also publish the `amoeba-db` image (#18).

## 0.1.1 — 2026-11-11

### Upgrade notes

- **Obscura package needs a token.** Obscura 0.2.3+ refuses to start its MCP server without `OBSCURA_MCP_TOKEN`. The example package now pins `h4ckf0r0day/obscura:0.2.6` and requires that secret (32+ characters) at install. Reinstall Obscura with `values.secrets.OBSCURA_MCP_TOKEN` set.
- **Upstream auth fails closed.** A service with `upstream_auth` whose token can't be resolved now gets `503` with `x-amoeba-rejection-reason: secret-unavailable`. Before, the request was forwarded with an empty `Bearer` token.
- **New header for upstreams.** Upstreams now receive `x-amoeba-roles`. Amoeba strips any copy the client sends.

### Gateway

- Path-based `operation_rules`: map paths like `loans/{id}/approve` to named operations, checked against the service's role map. The first matching rule wins; requests that match no rule fall back to the HTTP method. Rules are validated when the config loads (#11).
- Upstreams receive the caller's roles in `x-amoeba-roles`, next to `x-amoeba-user` and `x-amoeba-org` (#11).
- `upstream_auth.token_secret` reads the upstream token from an installed secret file on each request (#14).
- Token revocation is persisted, so a revoked token stays revoked across restarts.

### Apps and secrets

- App manifests can declare `upstream_auth: {type, secret}`. The same secret is passed to the container and sent by the gateway as its credential (#14).
- Secret names are checked so they can't escape the app's secrets directory. Install rejects secret values the manifest doesn't declare (#12).
- MCP `deploy_service`: agents can deploy services, with single-tenant or per-org permission maps (#4).

### Web console

- New admin console: services table, stats, issuing scoped tokens, and testing endpoints.
- Deploying from the console now builds a real app package, so rows marked as secret reach the service as secrets (#12).
- The Git repo and Dockerfile tabs now work: Amoeba builds the image on its host (`POST /admin/apps/:name/build`), shows the build log, and then deploys the image. Git URLs must be `https://`, and all input is checked before any `git` or `docker` command runs (#13).
- `npm run build` works again: Tailwind is pinned to v3. A new `Web` CI workflow runs tests and the build (#12).

### Fixes and maintenance

- Builds again after the zip 9 and jsonwebtoken 10 upgrades. JWT signing no longer panics; that panic made every login return 502.
- Nightly e2e: retry droplet deletion, sweep leftover droplets, and run a branch's own orchestrator image on manual runs (#14).
- New example config for Unlimited-OCR.
- Dependency bumps: vite, postcss-selector-parser, jsonwebtoken, cargo group (#5–#8).
