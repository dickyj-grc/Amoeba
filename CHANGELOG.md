# Changelog

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
