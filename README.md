<table border="0">
<tr>
<td width="90"><img src="images/amoeba%20square%20logo.png" alt="Amoeba logo" width="72" /></td>
<td>

# Amoeba
The Amoeba Compute Orchestrator is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.

</td>
</tr>
</table>


## System Architecture & Technical Specification

The **Amoeba Compute Orchestrator (`amoeba`)** is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.

---

## Why Amoeba?

### 1. Cut Energy and Infrastructure Costs
Amoeba's scale-to-zero lifecycle means workloads are only running when they are actually being used.

- **Local machines**: a home lab or workstation no longer has to keep every container — GPU models, scrapers, databases, dashboards — idling 24/7. Services cold-boot on demand and stop after a configurable cooldown, directly reducing power draw and fan noise.
- **Cloud VMs**: because containers are started only when requested, a single VM can host far more services than it could if everything were always-on. You pay for compute only when it is being used, and you can pack more workloads onto smaller instances.

### 2. Add Zero-Trust Auth to Any Open Source Project
Many excellent open-source tools ship with no authentication layer. Amoeba sits in front of them and enforces JWT-based access control without modifying the upstream application.

- Every service gets role-based permissions (`read`, `add`, `update`, `delete`) out of the box.
- Local HMAC mode lets you bootstrap users from a simple JSON file.
- JWKS mode lets you plug in a corporate identity provider for multi-tenant deployments.

### 3. Deploy New Apps Without Subdomain or SSL Plumbing
Adding a containerized app usually means DNS records, reverse-proxy rules, TLS certificates, and port management. Amoeba removes that toil:

- New services are reached via `/v1/<service_name>/<subpath>`; no custom subdomain is required.
- The edge proxy (Caddy, Traefik, etc.) handles SSL termination once for the whole gateway.
- Adding or removing an app is a single entry in `services.json`; Amoeba hot-reloads it without a restart.

### 4. Protect Machine Resources With Declared Capacity Budgets
Instead of guessing whether a machine can handle another workload, Amoeba enforces declared CPU, memory, and GPU-VRAM budgets before admitting a request that would cold-start a service.

- Services declare their footprint in `services.json`.
- Amoeba sums currently-occupying siblings and rejects the request with `503` if the new workload would exceed the budget.
- This lets you safely pack more services onto the same box without accidentally starving the GPU or RAM.

### 5. Built-In Usage Metering and Auditability
Every proxied request emits a telemetry log containing the caller, organization, service, operation, status code, and latency.

- No extra observability tooling is required to answer "who called what and when."
- The same logs form the basis for chargeback, debugging, and security auditing.

### 6. Secure Secrets and Upstream Credential Translation
Amoeba keeps sensitive material out of your compose files and config while bridging different auth schemes between callers and backends.

- `env_from_secret` injects secrets from a protected directory at container-start time.
- `upstream_auth` can replace the caller's JWT with a backend-specific static token or custom header before forwarding the request.

### 7. Minimal, Self-Hosted, and Portable
Amoeba is designed for environments where you want control without operational overhead.

- The release image is a small static binary; no Kubernetes or managed control plane is required.
- Works with Docker on Linux/Windows and Apple's native `container` CLI on macOS/Apple Silicon.
- Each machine runs its own instance with its own config, so scaling out is as simple as adding another box.

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

Every service declares **where** it's reached (`placement`) and **how Amoeba drives its container lifecycle** — exactly one of `container` (a single image Amoeba starts/stops directly) or `stack_spec` (a multi-container stack driven through the `docker compose` CLI):

```json
{
  "version": 1,
  "machines": {
    "local": { "type": "vm", "drivers": ["docker"], "resources": { "memory": "16Gi" } }
  },
  "services": {
    "gemma4": {
      "placement": { "type": "vm", "ip": "gemma4", "port": 11434, "cooldown_seconds": 180 },
      "container": {
        "image": "ollama/ollama:latest",
        "resources": { "limits": { "memory": "8Gi" } }
      },
      "operation_rules": [
        { "operation": "read", "match_type": "http_method", "values": ["GET", "HEAD"] },
        { "operation": "execute", "match_type": "http_method", "values": ["POST"] }
      ],
      "permissions": {
        "read": ["admin", "analyst", "viewer"],
        "execute": ["admin", "analyst"]
      },
      "upstream_auth": null
    },
    "docling": {
      "placement": { "type": "vm", "ip": "docling", "port": 5000, "cooldown_seconds": 120 },
      "container": { "image": "ds4sd/docling-serve:latest" },
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

`machines` and `placement.machine` can be omitted while there's only ever **one** machine — it's implied. They become required fields once a second machine exists (see 2.4).

**`public` (optional, defaults to `false`)** — set `"public": true` on a service to skip JWT verification and the permission check entirely for it. This is an explicit opt-in: a service with `permissions` omitted or empty is *not* public by default — it still requires a valid token, it just denies every role (fails closed with `403`) until you add roles to `permissions`. Only use `public: true` for endpoints that are genuinely meant to be reachable with no auth at all (e.g. a health check or webhook receiver).

### 2.2 Workload Types: `container` vs. `stack_spec`

A service is exactly one of two shapes — validated at load time (both startup and hot-reload), rejecting a service that sets both or neither:

**`container`** — a single image. Amoeba owns the full lifecycle directly via the Docker Engine API (create/start on cold boot, stop on cooldown). `placement.ip` is both the upstream host *and* the name Amoeba creates the container under, attached to the shared Docker network (`AMOEBA_DOCKER_NETWORK`, default `amoeba-net`) so it's reachable by name from Amoeba's own container.

**`stack_spec`** — a multi-container stack, driven through the `docker compose` CLI instead. `placement.primary_service` names which container in the stack the gateway routes to. Exactly one of:
- **`compose_file`** — a path to a docker-compose file a human already wrote (e.g. a vendored product stack). Amoeba never re-derives it; it just runs `docker compose -f <file> -p <project_name> {up -d|start|stop}` against it.
- **`services`** — an inline, Amoeba-authored stack (no file exists yet). Amoeba regenerates an equivalent compose file on disk before every start, then drives it through the identical CLI path. Every network a service references defaults to `external: true` (must already exist — e.g. a network shared with Amoeba's own container) unless `stack_spec.networks` explicitly marks it `{"external": false}` to have this stack create it fresh.

`project_name` defaults to the service's own key when omitted. A **cooldown on a `stack_spec` service always means `stop`, not `down`** — containers/networks stick around so the next activation is a fast `start` rather than a full `up -d` reconciliation.

```json
{
  "gorules": {
    "placement": { "type": "vm", "cooldown_seconds": 1800, "primary_service": "gorules", "port": 80 },
    "stack_spec": {
      "compose_file": "/etc/amoeba/stacks/gorules/docker-compose.yml",
      "project_name": "gorules"
    },
    "permissions": { "read": ["admin", "analyst"] },
    "env_from_secret": { "DB_PASSWORD": "gorules/db_password" },
    "upstream_auth": null
  },
  "brms": {
    "placement": { "type": "vm", "cooldown_seconds": 1800, "primary_service": "brms", "port": 3000 },
    "stack_spec": {
      "compose_version": "3.8",
      "env_vars": { "LOG_LEVEL": "info" },
      "services": {
        "brms": {
          "image": "gorules/brms:latest",
          "container_name": "gorules-brms",
          "ports": ["3000"],
          "depends_on": ["postgres"],
          "networks": ["gorules_network", "proxy-network"]
        },
        "postgres": {
          "image": "postgres:18.4",
          "container_name": "gorules-postgres",
          "networks": ["gorules_network"]
        }
      }
    },
    "permissions": { "read": ["admin", "analyst"] },
    "env_from_secret": { "DB_PASSWORD": "brms/db_password" },
    "upstream_auth": null
  }
}
```

See `config/services.local.example.json` for these two alongside a `container` service (`gemma4`) in one file — the three local-machine scenarios Amoeba supports today.

**`env_from_secret`** (optional) — `env var name -> "<service>/<key>"`, resolved fresh at every start from `<AMOEBA_SECRETS_DIR, default /etc/amoeba/secrets>/<service>/<key>` and injected directly into the container/compose-subprocess environment. Never written into Amoeba's own config or a generated compose file — a referenced compose file needs to consume it via normal Compose variable interpolation (`${DB_PASSWORD}`) or the `environment: [DB_PASSWORD]` passthrough shorthand.

### 2.3 A Second `container` Backend: `apple-container` (macOS/Apple Silicon)

A machine's `drivers` list picks which backend drives its `container`-workload services. `drivers: ["docker"]` (the default assumed throughout this doc) talks to the Docker Engine API. `drivers: ["apple-container"]` instead shells out to Apple's native `container` CLI — one lightweight Linux micro-VM per container via Containerization + Virtualization.framework, with sub-second boots and no Docker Desktop/OrbStack VM overhead. It accepts ordinary Docker image references (`ollama/ollama:latest` resolves to `docker.io/library/...` exactly like the Docker CLI) and requires `container system start` to have been run once beforehand.

```json
{
  "version": 1,
  "machines": {
    "local": { "type": "vm", "drivers": ["apple-container"], "resources": { "memory": "16Gi" } }
  },
  "services": {
    "gemma4": {
      "placement": { "type": "vm", "port": 11434, "cooldown_seconds": 180 },
      "container": {
        "image": "ollama/ollama:latest",
        "resources": { "limits": { "memory": "8Gi" } }
      },
      "permissions": { "read": ["admin", "analyst"] },
      "upstream_auth": null
    }
  }
}
```

See `config/services.applecontainer.example.json`. Note what's *missing*: **`placement.ip` is omitted, and validation requires it stay that way** for a `container` workload on an `apple-container` machine. Apple's `container` tool assigns each container a fresh IP on every start (confirmed by hand: stopping and restarting the same container moved it from `192.168.64.2` to `192.168.64.3`), and — unlike a Docker container on a shared bridge network — the bare macOS process running Amoeba can't resolve it by name; there's no DNS between the host and a `container`-managed VM. So the upstream host isn't a static config value here: Amoeba resolves it dynamically after every cold-boot (`container inspect`) and caches it in memory for warm requests to reuse, re-resolving again the next time the service cold-boots.

`stack_spec` (2.2) has no `apple-container` equivalent yet — Apple's CLI has no compose-like multi-container orchestration today, so multi-container stacks always go through `docker compose` regardless of a machine's `drivers`.

### 2.4 Machine Capacity Gating (optional)

Rather than have the orchestrator introspect the host's real CPU/memory (which doesn't work consistently across bare metal, Docker cgroup limits, and serverless sandboxes like RunPod/Modal — and can't see GPU/VRAM at all), operators **declare** a capacity budget per machine, and the orchestrator checks declared usage against it before admitting a request that would newly activate a service. No runtime introspection, no platform-specific behavior.

```json
{
  "version": 1,
  "machines": {
    "gpu-box-1": { "type": "vm", "drivers": ["docker"], "resources": { "memory": "32Gi", "gpu_vram": "24Gi" } }
  },
  "services": {
    "gemma4": {
      "placement": {
        "type": "vm", "ip": "gpu-box-1-gemma4", "port": 11434, "cooldown_seconds": 180, "machine": "gpu-box-1"
      },
      "container": {
        "image": "ollama/ollama:latest",
        "resources": { "limits": { "memory": "16Gi", "gpu_vram": "16Gi" } }
      },
      "permissions": { "read": ["admin", "analyst"] }
    }
  }
}
```

See `config/services.capacity.example.json` for a fuller example with multiple services sharing one machine.

- **`placement.machine`** — the entry in the top-level `machines` map this service's usage counts against. Optional only while `machines` has exactly one entry (implied); referencing an undefined machine, or omitting it once a second machine exists, fails config validation (both at startup and on hot-reload) rather than silently skipping the gate or picking one arbitrarily.
- **Quantities** (`memory`, `gpu_vram` on both a machine's `resources` and a container's `resources.limits`) accept Kubernetes-style sized strings (`"16Gi"`, `"512Mi"`) or a bare number (interpreted as MB). `cpu_cores` is always a bare count. **Every field is independently opt-in**: a field missing from a container's `resources.limits` means it requests zero of that dimension (never competes for or is blocked by that budget line); a field missing from a machine's budget means that dimension is completely unconstrained. Omitting `resources` entirely (or using a `stack_spec` workload, which never declares one) means the service requests nothing on any dimension — it's nominally "on" the machine but exempt from capacity accounting.
- **On exceeding budget**: the request is rejected with `503 Service Unavailable` before any upstream call is attempted. No queueing — retry later.
- **Resource usage is per-service, not per-request**: a container's `resources.limits` represents its footprint while running. N concurrent calls to an already-warm service still count as one instance of its declared resources, not N×. Once a service is warm (within its `cooldown_seconds` window, or always-on when `cooldown_seconds` is omitted), further requests to it aren't re-checked against the budget — only the request that newly activates a cold service is gated, and only that same moment triggers the driver's cold-boot (2.2) too.

### 2.5 Subpath Mapping Protocol

Routing does not require unique DNS subdomains. Any service is reachable via path parameters:

$$\text{Target URL} = \texttt{https://<host>/v1/<service\_name>/<subpath>}$$

| Incoming Request Target | Extracted `service_name` | Extracted `subpath` | Upstream Proxy Destination |
| --- | --- | --- | --- |
| `POST /v1/gemma4/v1/chat/completions` | `gemma4` | `v1/chat/completions` | `[http://gemma4:11434/v1/chat/completions](http://gemma4:11434/v1/chat/completions)` |
| `POST /v1/docling/api/v1/parse` | `docling` | `api/v1/parse` | `[http://docling:5000/api/v1/parse](http://docling:5000/api/v1/parse)` |

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
git clone https://github.com/dickyj-grc/Amoeba.git && cd Amoeba

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
  "version": 1,
  "machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } },
  "services": {
    "gemma4": {
      "placement": { "type": "vm", "ip": "amoeba-gemma4-worker", "port": 11434, "cooldown_seconds": 180 },
      "container": { "image": "ollama/ollama:latest" },
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

### 5.6 Running Across Multiple Machines

If your services are spread across more than one machine — say, a local box for a GPU model and a cloud VM for a lighter CPU-only service — run **one Amoeba instance per machine**, not one central instance trying to reach into the others.

Each instance gets its **own** `services.json`, listing only the services physically on that box, with a `machines` entry for itself:

```json
// on the local machine
{
  "version": 1,
  "machines": {
    "local-machine": { "type": "vm", "drivers": ["docker"], "resources": { "memory": "32Gi", "gpu_vram": "8Gi" } }
  },
  "services": {
    "gemma4": {
      "placement": { "type": "vm", "ip": "localhost", "port": 11434, "cooldown_seconds": 180 },
      "container": {
        "image": "ollama/ollama:latest",
        "resources": { "limits": { "memory": "16Gi", "gpu_vram": "8Gi" } }
      },
      "permissions": { "read": ["admin", "analyst"] }
    }
  }
}
```

See `config/services.local.example.json` and `config/services.cloud.example.json` for a full matching pair.

Why per-machine rather than one shared file:

- **`ip` only ever needs to be local** — `localhost`, a loopback address, or a Docker container name on that machine's own network. Nothing in a service's config ever needs to name another machine, since each instance never reaches outside its own host.
- **Container lifecycle stays local** — each instance only ever needs to talk to its *own* Docker socket (the same `/var/run/docker.sock` mount `docker-compose.yml` already sets up) to eventually start/stop its own containers. There's no remote Docker API, no per-machine control agent, no SSH to build or secure.
- **`machine`/`machines` naming stays unambiguous** — a service's `machine` field always refers to *this* instance's own machine, not a name that has to stay in sync across a shared file describing every box in the fleet.

**Routing — independent entry points, no shared edge layer.** Each instance is reachable at its own address; they don't talk to each other. A cloud VM's instance can sit behind its own public Caddy exactly as in 5.2–5.3. A local machine's instance typically has no public IP (home networks sit behind NAT), so it's reachable only over a private tunnel (Tailscale, WireGuard, Cloudflare Tunnel) between the two — never exposed directly to the internet. Whoever calls a service just needs to know which endpoint to hit for it; that knowledge lives with the caller, not inside Amoeba.

One thing this implies for auth: `AMOEBA_LOCAL_JWT_SECRET` and `users.json` are also per-instance. If you want the same caller/token to work against both machines, either configure the same JWT secret on both instances, or point both at the same `mode = "jwks"` identity provider (section 3) instead of two independent `local_jwt` stores.

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

### 6.4 Obtaining a Token: `POST /auth/login`

In local JWT mode, call the public login endpoint with a username and password from `users.json`:

```bash
curl -X POST https://compute.yourdomain.com/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username": "dicky", "password": "hunter2"}'
```

On success it returns a short-lived JWT:

```json
{
  "token": "eyJhbG...",
  "expires_at": 1722070800
}
```

Use the token as `Authorization: Bearer <token>` for subsequent API calls. Token lifetime defaults to 1 hour and can be changed with `AMOEBA_TOKEN_TTL_SECONDS`.

### 6.5 Revoking a Token: `POST /auth/revoke`

An admin can revoke their own current token (or any token they hold) before it naturally expires:

```bash
curl -X POST https://compute.yourdomain.com/auth/revoke \
  -H "Authorization: Bearer $TOKEN"
```

Revocation is stored in memory only and is lost when the process restarts. After restart, every caller must log in again.

### 6.6 Choosing Between User Management Paths

- **CLI**: local/break-glass only. Whoever can shell into the host and write `/etc/amoeba/users.json` can run it — there's no additional app-level auth on top, so restrict host/SSH access accordingly.
- **API**: the ongoing, day-to-day path. Every action is tied to a real `sub` in an admin's JWT, giving you an audit trail the CLI doesn't. Consider binding `/admin/*` to an internal-only network or loopback port in front-line deployments, as defense in depth beyond the role check.

---

## 7. Amoeba App Packages

Amoeba supports a self-contained app package format so any open-source project can ship a deployable bundle. A package is a zip file containing an `amoeba.yaml` manifest and a standard `compose.yaml` (or image reference). Install and uninstall are token-gated admin API calls; Amoeba hot-reloads `services.json` without a restart.

See `examples/app-packages/hello-world/` for a working example.

### 7.1 Package structure

```text
hello-world.amoeba.zip
├── amoeba.yaml      # metadata, routing, permissions, form schema
└── compose.yaml     # standard Docker Compose file
```

### 7.2 Install and uninstall via curl

```bash
# Install
curl -X POST http://localhost:8080/admin/apps \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -F "package=@hello-world.amoeba.zip" \
  -F 'values={"env":{"GREETING":"Hello!"}};type=application/json'

# List installed apps
curl http://localhost:8080/admin/apps \
  -H "Authorization: Bearer $ADMIN_JWT"

# Uninstall
curl -X DELETE http://localhost:8080/admin/apps/hello-world \
  -H "Authorization: Bearer $ADMIN_JWT"
```

### 7.3 Encrypted secrets with age

Packages can include age-encrypted secret files. The target Amoeba instance decrypts them at install time using `AMOEBA_AGE_SECRET_KEY`.

#### Install age-keygen

`age-keygen` is shipped with the `age` CLI tool.

- **macOS**: `brew install age`
- **Debian/Ubuntu**: `sudo apt install age`
- **Fedora**: `sudo dnf install age`
- **Arch**: `sudo pacman -S age`
- **Pre-built binaries**: [github.com/FiloSottile/age/releases](https://github.com/FiloSottile/age/releases)
- **Go**: `go install filippo.io/age/cmd/age-keygen@latest`
- **Rust alternative**: `cargo install rage` (uses `rage-keygen` instead of `age-keygen`)

#### Generate and configure the age key

On the Amoeba server:

```bash
age-keygen -o amoeba.age.key
export AMOEBA_AGE_SECRET_KEY=$(grep AGE-SECRET-KEY amoeba.age.key)
```

#### Declare and encrypt a package secret

In `amoeba.yaml`:

```yaml
schema:
  secrets:
    API_KEY:
      description: API key
      required: true
      file: secrets/api_key.age
```

Encrypt the secret to the server's public key:

```bash
age -r age1... -o secrets/api_key.age < api_key.txt
```

Include `secrets/api_key.age` in the zip. Amoeba decrypts it at install time and writes the plaintext to `/etc/amoeba/secrets/<app>/API_KEY`. User-provided secret values in the install payload still take precedence over encrypted files.

---

## 8. Nightly End-to-End Test on DigitalOcean

A Python orchestrator spins up a DigitalOcean droplet, installs Caddy and Amoeba, exercises the admin/proxy APIs, verifies scale-to-zero, and destroys the droplet. It is designed to run nightly in GitHub Actions.

### 8.1 Required GitHub Secrets

| Secret | Purpose |
|---|---|
| `DIGITALOCEAN_TOKEN` | DO API personal access token |
| `DO_SSH_PRIVATE_KEY` | Private SSH key for the droplet |
| `DO_SSH_PUBLIC_KEY` | Public SSH key registered in DO |
| `AMOEBA_LOCAL_JWT_SECRET` | Amoeba JWT signing secret |
| `AMOEBA_AGE_SECRET_KEY` (optional) | Age identity for encrypted app secrets |

### 8.2 Files

- `scripts/e2e-do.py` — orchestrates the droplet lifecycle and tests
- `scripts/e2e-cloud-init.sh` — cloud-init user-data that provisions the droplet
- `.github/workflows/nightly-e2e.yml` — GitHub Actions schedule

### 8.3 Run locally

```bash
export DIGITALOCEAN_TOKEN="dop_v1_..."
export DO_SSH_PRIVATE_KEY="/path/to/id_ed25519"
export DO_SSH_PUBLIC_KEY="ssh-ed25519 AAAAC3NzaC..."
export AMOEBA_LOCAL_JWT_SECRET="..."

python3 -m pip install requests paramiko
python scripts/e2e-do.py
```

Add `--keep` to leave the droplet alive for debugging.

### 8.4 What the test verifies

1. Droplet creation and SSH readiness.
2. Amoeba + Caddy come up via Docker Compose.
3. Admin login returns a JWT.
4. `POST /admin/apps` installs the `hello-world` app package.
5. `GET /admin/apps` lists the installed app.
6. Admin creates `analyst` and `viewer` users with different roles.
7. Role-based access control:
   - `analyst` can read and add to `hello-world`.
   - `viewer` can read but not add to `hello-world`.
   - Non-admin users get `403` on `/admin/apps`.
8. `GET /v1/hello-world/` returns the expected response and wakes the container.
9. Error cases fail closed:
   - Missing/invalid token on a private service → `401`
   - Wrong password or unknown user login → `401`
   - Unknown service → `404`
   - Duplicate user creation → `409`
   - Malformed or missing app package → `400`
   - Non-admin user creation attempt → `403`
   - Revoked token → `401`
10. After the cooldown, the container is stopped (scale-to-zero).
11. Droplet is destroyed.
