# Amoeba
The Amoeba Compute Orchestrator is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.


## System Architecture & Technical Specification

The **Amoeba Compute Orchestrator (`amoeba`)** is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.

---

## 1. System Architecture Overview

The system follows a two-tier architecture: **Edge Reverse Proxy Layer** (public-facing) and **Compute Orchestrator Layer** (internal control plane).

```
                                  [ PUBLIC INTERNET ]
                                           │
                                           │ Port 80 / 443
                                           ▼
                       ┌───────────────────────────────────────┐
                       │   EDGE PROXY (Caddy / Traefik / etc)  │
                       │   - Public SSL/TLS Termination    │
                       │   - Domain & Subdomain Handling       │
                       └───────────────────┬───────────────────┘
                                           │
                                           │ Loopback / Private Subnet (Port 8080)
                                           ▼
┌───────────────────────────────────────────────────────────────────────────────────────────┐
│                           Amoeba COMPUTE ENGINE (Axum Proxy)                            │
│                                                                                           │
│   ┌───────────────────────────┐  ┌──────────────────────────┐  ┌──────────────────────┐   │
│   │ Unified Auth & Claims     │  │  Capacity & Concurrency  │  │ Usage Metering &     │   │
│   │ (Local HMAC / Remote JWKS)│  │  Limiter (Semaphore)     │  │ Telemetry Extractor  │   │
│   └─────────────┬─────────────┘  └────────────┬─────────────┘  └──────────┬───────────┘   │
│                 │                             │                           │               │
│                 └─────────────────────────────┼───────────────────────────┘               │
│                                               ▼                                           │
│                            ┌─────────────────────────────────────┐                        │
│                            │   Container Lifecycle Manager       │                        │
│                            │   - Cold-boot trigger (<400ms)      │                        │
│                            │   - Active Connection Counter       │                        │
│                            │   - Lock-free ArcSwap Config Engine │                        │
│                            └──────────────────┬──────────────────┘                        │
└───────────────────────────────────────────────┼───────────────────────────────────────────┘
                                                │
                          ┌─────────────────────┴─────────────────────┐
                          │                                           │
                          ▼                                           ▼
          ┌───────────────────────────────┐           ┌───────────────────────────────┐
          │     Scale-to-Zero Workers     │           │    Warm Stateful Services     │
          │   (Gemma 4, Obscura, Docling) │           │     (Metabase, Postgres)      │
          │  - Dynamic start/stop         │           │  - Always running             │
          │  - Token injection/translation│           │  - Telemetry & Auth gated     │
          └───────────────────────────────┘           └───────────────────────────────┘

```

---

## 2. Dynamic Routing & Services Specification

Services are defined declaratively in `/etc/amoeba/services.json`. The orchestrator watches this file at runtime, executing **lock-free atomic memory pointer swaps (`ArcSwap`)** via `notify` kernel events without process restarts.

### 2.1 Configuration Schema (`services.json`)

```json
{
  "services": {
    "gemma4": {
      "image": "ollama/ollama:latest",
      "ip": "192.168.100.20",
      "port": 11434,
      "memory": "16g",
      "cooldown_seconds": 60,
      "operation_rules": [
        {
          "operation": "read",
          "match_type": "http_method",
          "values": ["GET", "HEAD"]
        },
        {
          "operation": "execute",
          "match_type": "http_method",
          "values": ["POST"]
        }
      ],
      "permissions": {
        "read": ["admin", "analyst", "viewer"],
        "execute": ["admin", "analyst"]
      },
      "upstream_auth": null
    },
    "docling": {
      "image": "ds4sd/docling-serve:latest",
      "ip": "192.168.100.40",
      "port": 5000,
      "memory": "4g",
      "cooldown_seconds": 120,
      "permissions": {
        "read": ["admin", "analyst"],
        "add": ["admin", "analyst"]
      },
      "upstream_auth": {
        "type": "bearer_static",
        "token_env_var": "INTERNAL_DOCLING_KEY"
      }
    }
  }
}

```

### 2.2 Subpath Mapping Protocol

Routing does not require unique DNS subdomains. Any service is reachable via path parameters:

$$\text{Target URL} = \texttt{https://<host>/v1/<service\_name>/<subpath>}$$

| Incoming Request Target | Extracted `service_name` | Extracted `subpath` | Upstream Proxy Destination |
| --- | --- | --- | --- |
| `POST /v1/gemma4/v1/chat/completions` | `gemma4` | `v1/chat/completions` | `[http://192.168.100.20:11434/v1/chat/completions](http://192.168.100.20:11434/v1/chat/completions)` |
| `POST /v1/docling/api/v1/parse` | `docling` | `api/v1/parse` | `[http://192.168.100.40:5000/api/v1/parse](http://192.168.100.40:5000/api/v1/parse)` |

---

## 3. Dual Auth Mode: Local vs. Amoeba Identity Provider

The orchestrator abstracts token verification into a single `JwtEngine`. The engine operates either in **Offline/Local Mode** or **Cloud/Amoeba OIDC Mode** based on `/etc/amoeba/config.toml`.

```toml
# /etc/amoeba/config.toml

[server]
bind_addr = "127.0.0.1:8080"
mode = "standalone"

[auth]
# Modes: "local_jwt" or "jwks"
mode = "jwks"
issuer = "https://auth.amoeba.com"
jwks_url = "https://auth.amoeba.com/.well-known/jwks.json"
audience = "amoeba-compute"

# Required only if mode = "local_jwt"
jwt_secret = "env:AMOEBA_LOCAL_JWT_SECRET"
users_file = "/etc/amoeba/users.json"

```

---

### 3.1 Authentication Sequence Comparison

```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│ MODE A: LOCAL JWT MODE (Standalone / Offline)                                          │
│                                                                                        │
│ [ Client ] ──( 1. POST /auth/login )──► [ Axum Engine ] ──( 2. Verifies Argon2 Hash ) │
│                                                │                                       │
│                                       3. Issues Local HMAC JWT                         │
│                                                │                                       │
│ [ Client ] ◄───────────────────────────────────┘                                       │
│                                                                                        │
│ [ Client ] ──( 4. GET /v1/gemma4 + Bearer JWT )──► [ Axum Engine ]                     │
│                                                         │                              │
│                                                5. Decodes with local secret            │
└────────────────────────────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────────────────────────────┐
│ MODE B: Amoeba JWKS MODE (Distributed / Multi-Tenant)                                │
│                                                                                        │
│ [ Background Task ] ──( Periodically Syncs )──► [ Amoeba IdP (/.well-known/jwks) ]   │
│         │                                                                              │
│         ▼                                                                              │
│ [ Local JWKS Cache ]                                                                   │
│         ▲                                                                              │
│         │                                                                              │
│ [ Client ] ──( 1. GET /v1/gemma4 + Amoeba Signed JWT )──► [ Axum Engine ]          │
│                                                                    │                   │
│                                                      2. Validates via cached public key│
└────────────────────────────────────────────────────────────────────────────────────────┘

```

---

### 3.2 Rust Unified Verification Engine Implementation

```rust
use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub org_id: Option<String>,
    pub roles: Vec<String>,
    pub exp: usize,
}

#[derive(Clone)]
pub enum AuthMode {
    LocalJwt {
        secret: Vec<u8>,
    },
    Jwks {
        jwks_url: String,
        cached_keys: Arc<RwLock<jsonwebtoken::jwk::JwkSet>>,
    },
}

pub struct JwtEngine {
    pub mode: AuthMode,
    pub validation: Validation,
}

impl JwtEngine {
    pub async fn verify_token(&self, token: &str) -> Result<Claims, String> {
        match &self.mode {
            AuthMode::LocalJwt { secret } => {
                let key = DecodingKey::from_secret(secret);
                decode::<Claims>(token, &key, &self.validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("Local HMAC failed: {}", e))
            }
            AuthMode::Jwks { cached_keys, .. } => {
                let header = decode_header(token).map_err(|e| e.to_string())?;
                let kid = header.kid.ok_or("Token missing 'kid' header")?;

                let keys = cached_keys.read().await;
                let jwk = keys.find(&kid).ok_or("Key ID not found in JWKS cache")?;

                let decoding_key = DecodingKey::from_jwk(jwk).map_err(|e| e.to_string())?;
                decode::<Claims>(token, &decoding_key, &self.validation)
                    .map(|d| d.claims)
                    .map_err(|e| format!("Amoeba JWKS validation failed: {}", e))
            }
        }
    }
}

// Unified Middleware Function
pub async fn unified_auth_middleware(
    State(engine): State<Arc<JwtEngine>>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if !auth_header.starts_with("Bearer ") {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let token = &auth_header[7..];

    match engine.verify_token(token).await {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            Ok(next.run(req).await)
        }
        Err(err) => {
            tracing::error!("Authentication failure: {}", err);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

```

---

## 4. Operation Mapping & Role Security

Requests are evaluated prior to waking or scaling containers.

```
Incoming Request Method / Body
              │
              ▼
    Resolve Operation Type ──► [ "read" | "add" | "update" | "delete" | "custom" ]
              │
              ▼
    Compare `claims.roles` against `service.permissions[operation]`
              │
      ┌───────┴───────┐
      ▼               ▼
 [ Matched ]    [ Unmatched ]
      │               │
      ▼               ▼
Proceed to Proxy    403 Forbidden (Container remains offline)

```

1. **REST Defaults:** Maps `GET` $\rightarrow$ `read`, `POST` $\rightarrow$ `add`, `PUT`/`PATCH` $\rightarrow$ `update`, `DELETE` $\rightarrow$ `delete`.
2. **Payload Overrides (Odoo / JSON-RPC):** Scans body for `params.method` (e.g., `search_read` is classified as `read` despite using `POST`).
3. **Upstream Token Injection:** If `upstream_auth` is set in `services.json`, the proxy strips the incoming caller token and attaches the microservice-specific static key or minted internal token before dispatching the request over internal loopback networks.

---

## 5. Deployment (Docker Compose)

The simplest way to run Amoeba alongside a containerized edge proxy is a single `docker-compose.yml` that puts Caddy and the orchestrator on a shared Docker bridge network. Caddy handles automatic HTTPS and domain routing; the orchestrator manages backend container lifecycles over the host's Docker socket.

### 5.1 Project Layout

Everything lives in the repo, using paths relative to it — clone it anywhere and `docker compose up` works, no fixed install path required:

```text
Amoeba/
├── docker-compose.yml
├── Dockerfile
├── Caddyfile
├── .env.example
└── config/
    ├── config.example.toml
    ├── services.example.json
    ├── services.docker.example.json
    └── users.example.json
```

### 5.2 Quickstart

```bash
# 1. Clone the repo
git clone https://github.com/hyperjump888/Amoeba.git && cd Amoeba

# 2. Set your own JWT signing secret
cp .env.example .env
# edit .env and replace the placeholder with a long random string

# 3. Provide a service catalog (container-name based, see 5.3 below)
cp config/services.docker.example.json config/services.json

# 4. Start the stack
docker compose up -d --build
```

This builds the orchestrator image, then starts both `amoeba-caddy` and `amoeba-orchestrator` on the shared `amoeba-net` bridge network. `docker-compose.yml` mounts `./config` to `/etc/amoeba` inside the orchestrator container and refuses to start if `AMOEBA_LOCAL_JWT_SECRET` isn't set, rather than silently falling back to an insecure default.

### 5.3 Routing to Workers on the Same Network

Any worker container joined to `amoeba-net` is reachable by its container/service name via Docker's built-in DNS — no static IPs needed. Point `services.json` at those names instead of IP addresses:

```json
{
  "services": {
    "gemma4": {
      "image": "ollama/ollama:latest",
      "ip": "amoeba-gemma4-worker",
      "port": 11434,
      "cooldown_seconds": 180,
      "permissions": { "read": ["admin", "analyst"] }
    }
  }
}
```

Add each worker as its own service in `docker-compose.yml`, give it a `container_name` matching what you put in `services.json`, and attach it to `amoeba-net`:

```yaml
services:
  gemma4:
    image: ollama/ollama:latest
    container_name: amoeba-gemma4-worker
    restart: unless-stopped
    networks:
      - amoeba-net
```

### 5.4 Hot Reload

Editing `./config/services.json` on the host is picked up immediately by the orchestrator's `notify`-based file watcher inside the container — no restart of the orchestrator or Caddy required.

### 5.5 Why This Is Easy to Operate

1. **Zero system dependencies** — only Docker and Docker Compose are required; no Rust toolchain, Caddy, or system libraries need to be installed on the host.
2. **Automatic TLS** — Caddy fetches and renews Let's Encrypt/ZeroSSL certificates on port 443 once `Caddyfile` points at a real domain with DNS pointing at the host.
3. **Small image** — the multi-stage `Dockerfile` produces a ~30MB Alpine-based runtime image with just the two release binaries (`amoeba`, `amoeba-admin`) and CA certificates.

---

## 6. User Access Management (Local JWT Mode)

When `auth.mode = "local_jwt"`, users live in `/etc/amoeba/users.json` — username, Argon2id password hash, roles, and org. There are two ways to manage that file, and they're deliberately gated differently.

### 6.1 `users.json` Schema

```json
{
  "users": {
    "dicky": {
      "password_hash": "$argon2id$v=19$m=65536,t=3,p=4$c29tZXNhbHRzdHJpbmc$9x2aX5q2K8mQp1vT6y8uZ1wA7sK4jL3mP0nO9rS8tU",
      "roles": ["admin", "analyst"],
      "org_id": "org_amoeba_hq"
    },
    "analyst_user": {
      "password_hash": "$argon2id$v=19$m=65536,t=3,p=4$YW5vdGhlcnNhbHQ$2y8uZ1wA7sK4jL3mP0nO9rS8tU9x2aX5q2K8mQp1vT6",
      "roles": ["analyst"],
      "org_id": "org_client_alpha"
    },
    "viewer_bot": {
      "password_hash": "$argon2id$v=19$m=65536,t=3,p=4$Ym90c2FsdHN0cmluZw$7sK4jL3mP0nO9rS8tU9x2aX5q2K8mQp1vT62y8uZ1wA",
      "roles": ["viewer"],
      "org_id": "org_client_alpha"
    }
  }
}
```

`password_hash` is never a plaintext password — it's generated by hashing with Argon2id and a random salt (see 6.2/6.3 below). Never hand-write this field.

### 6.2 Bootstrap: the `amoeba-admin` CLI

There's a bootstrapping problem with managing users purely over HTTP: you can't get an admin-scoped JWT before an admin user exists. `amoeba-admin` is the local, on-box tool that breaks that cycle — run it once to seed the first admin, then manage everyone else through the API (6.3).

```bash
# Create the first admin user
amoeba-admin add-user dicky hunter2 --roles admin,analyst --org org_amoeba_hq

# Change a password and/or roles
amoeba-admin update-user dicky --password new-password --roles admin

# Remove a user
amoeba-admin delete-user analyst_user

# Point at a non-default file (defaults to /etc/amoeba/users.json)
amoeba-admin add-user viewer_bot secret --roles viewer --org org_client_alpha --users-file ./config/users.json
```

Each command loads `users.json`, applies the change, and rewrites the file with `0600` permissions (owner read/write only) — the file holds password hashes, so it's never left world-readable. In the Docker Compose deployment, run this via `docker compose exec orchestrator amoeba-admin ...` against the mounted `/etc/amoeba/users.json`.

### 6.3 `/admin/users` HTTP API

Once at least one admin exists, everyone else can be managed over HTTP instead of shelling into the box. These routes require the **same JWT verification as every other endpoint, plus `"admin"` in the caller's `roles`** — there's no separate secret to provision or rotate.

| Method | Path | Body | Result |
| --- | --- | --- | --- |
| `POST` | `/admin/users` | `{"username", "password", "roles", "org_id"}` | `201 Created`, or `409 Conflict` if the username exists |
| `PATCH` | `/admin/users/:username` | any of `{"password", "roles", "org_id"}` | `200 OK`, `404` if unknown, `400` if body is empty |
| `DELETE` | `/admin/users/:username` | — | `204 No Content`, or `404` if unknown |

```bash
curl -X POST https://compute.yourdomain.com/admin/users \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -H "Content-Type: application/json" \
  -d '{"username": "new_analyst", "password": "hunter2", "roles": ["analyst"], "org_id": "org_client_alpha"}'
```

A request without a valid token gets `401`; a valid token without the `admin` role gets `403` — the same per-service pattern used for proxied requests (section 4), just with a fixed required role instead of a per-service permission map.

### 6.4 Choosing Between Them

- **CLI**: local/break-glass only. Whoever can shell into the host and write `/etc/amoeba/users.json` can run it — there's no additional app-level auth on top, so restrict host/SSH access accordingly.
- **API**: the ongoing, day-to-day path. Every action is tied to a real `sub` in an admin's JWT, giving you an audit trail the CLI doesn't. Consider binding `/admin/*` to an internal-only network or loopback port in front-line deployments, as defense in depth beyond the role check.
