# Amoeba
The Amoeba Compute Orchestrator is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.


## System Architecture & Technical Specification

The **Amoeba Compute Orchestrator (`amb-compute`)** is an edge-aware, scale-to-zero L7 application and compute gateway written in Rust (Axum). It manages the lifecycle of transient microservices (AI models, web scrapers, document parsers) and stateful application nodes, enforcing zero-trust authorization, usage metering, and capacity gating.

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

Services are defined declaratively in `/etc/Amoeba/services.json`. The orchestrator watches this file at runtime, executing **lock-free atomic memory pointer swaps (`ArcSwap`)** via `notify` kernel events without process restarts.

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

The orchestrator abstracts token verification into a single `JwtEngine`. The engine operates either in **Offline/Local Mode** or **Cloud/Amoeba OIDC Mode** based on `/etc/Amoeba/config.toml`.

```toml
# /etc/Amoeba/config.toml

[server]
bind_addr = "127.0.0.1:8080"
mode = "standalone"

[auth]
# Modes: "local_jwt" or "jwks"
mode = "jwks"
issuer = "https://auth.Amoeba.com"
jwks_url = "https://auth.Amoeba.com/.well-known/jwks.json"
audience = "Amoeba-compute"

# Required only if mode = "local_jwt"
jwt_secret = "env:Amoeba_LOCAL_JWT_SECRET"
users_file = "/etc/Amoeba/users.json"

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
