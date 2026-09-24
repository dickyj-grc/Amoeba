# Amoeba MCP Server

The `amoeba-mcp` server lets AI agents (Claude, ChatGPT, Gemini) deploy and manage microservices on an Amoeba gateway through the [Model Context Protocol](https://modelcontextprotocol.io/) (MCP).

## How it works

```
┌──────────────┐     stdio (JSON-RPC)     ┌──────────────┐     HTTP      ┌──────────────┐
│  AI Client   │ ◄──────────────────────► │  amoeba-mcp  │ ◄───────────► │   Amoeba     │
│  (Claude)    │                          │  (this)      │  admin API    │  Gateway     │
└──────────────┘                          └──────┬───────┘               └──────┬───────┘
                                                 │                              │
                                                 │ docker build/logs            │ proxy
                                                 ▼                              ▼
                                          ┌──────────┐                ┌────────────────┐
                                          │  Docker   │                │  Your Service  │
                                          │  Daemon   │                │  Containers    │
                                          └──────────┘                └────────────────┘
```

The MCP server is launched by the AI client as a subprocess. It communicates via stdin/stdout using JSON-RPC 2.0, per the MCP specification. All mutations go through Amoeba's admin API (`/admin/*`) — the MCP server never writes `services.json` directly (except for `update_policy`, which modifies the catalog file to trigger hot-reload for fine-grained policy changes).

## Quick start

### 1. Build

```bash
cargo build --release --bin amoeba-mcp
```

The binary is at `target/release/amoeba-mcp`.

### 2. Configure environment

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `AMOEBA_URL` | No | `http://localhost:8080` | Base URL of the Amoeba gateway |
| `AMOEBA_TOKEN` | **Yes** | — | Admin JWT for authenticating to Amoeba |
| `AMOEBA_REGISTRY` | No | — | Registry prefix for pushed images (e.g. `registry.example.com/amoeba`) |
| `AMOEBA_BUILD_DIR` | No | `$TMPDIR/amoeba-builds` | Temp directory for build contexts |
| `AMOEBA_CATALOG_PATH` | No | `/etc/amoeba/services.json` | Path to services.json for policy updates |
| `AMOEBA_MCP_LOG` | No | `error` | Log level (uses `tracing-subscriber` env filter) |

### 3. Get an admin token

```bash
# If using Amoeba's local JWT mode:
curl -X POST http://localhost:8080/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"your-password"}'
```

### 4. Configure your AI client

#### Claude Desktop

Add to `~/.claude/claude_desktop_config.json` or the project's `.mcp.json`:

```json
{
  "mcpServers": {
    "amoeba": {
      "command": "/path/to/target/release/amoeba-mcp",
      "env": {
        "AMOEBA_URL": "http://localhost:8080",
        "AMOEBA_TOKEN": "eyJhbGciOiJIUzI1NiIs...",
        "AMOEBA_REGISTRY": "registry.example.com/amoeba",
        "AMOEBA_MCP_LOG": "error"
      }
    }
  }
}
```

#### With Docker

```json
{
  "mcpServers": {
    "amoeba": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "/var/run/docker.sock:/var/run/docker.sock",
        "-e", "AMOEBA_URL=http://host.docker.internal:8080",
        "-e", "AMOEBA_TOKEN=eyJhbGciOiJIUzI1NiIs...",
        "amoeba-mcp"
      ]
    }
  }
}
```

## Tools reference

### `deploy_service`

Deploy a microservice to Amoeba.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name (no path separators) |
| `source_kind` | enum | yes | `dockerfile`, `image`, or `auto` |
| `source` | string | yes | Dockerfile text, image ref, or base64 tarball |
| `port` | integer | yes | Container port |
| `cooldown_seconds` | integer | no | Idle seconds before scale-to-zero |
| `env` | object | no | Environment variables |
| `secrets` | object | no | Secret values (stored encrypted) |
| `permissions` | object | no | Operation → roles map |
| `public` | boolean | no | Skip JWT auth if true |
| `memory` | string | no | Memory limit (e.g. `512Mi`) |
| `cpu_cores` | integer | no | CPU core limit |
| `readiness_timeout_secs` | integer | no | Max wait for readiness (default 120) |

**Example — Deploy a Python FastAPI service from a Dockerfile:**

```
deploy_service {
  name: "pdf-parser",
  source_kind: "dockerfile",
  source: "FROM python:3.12-slim\nWORKDIR /app\nCOPY requirements.txt .\nRUN pip install -r requirements.txt\nCOPY . .\nEXPOSE 8080\nCMD [\"uvicorn\", \"main:app\", \"--host\", \"0.0.0.0\", \"--port\", \"8080\"]",
  port: 8080,
  cooldown_seconds: 300,
  permissions: { read: ["admin", "viewer"] }
}
```

**Example — Deploy from a pre-built image:**

```
deploy_service {
  name: "redis-cache",
  source_kind: "image",
  source: "redis:7-alpine",
  port: 6379,
  cooldown_seconds: 600
}
```

### `get_build_logs`

Fetch build/pull logs for a deploy in progress or recently completed.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name |

### `get_runtime_logs`

Fetch recent stdout/stderr from a running service container.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name |
| `tail` | integer | no | Number of lines (default 50) |

### `get_service_status`

Check a service's health and lifecycle state.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name |

### `list_services`

List all deployed services with their lifecycle states.

No parameters.

### `delete_service`

Remove a service and clean up its resources.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name |

### `update_policy`

Update a service's access policy. Changes hot-reload immediately.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | yes | Service name |
| `permissions` | object | no | Replace entire permissions map |
| `public` | boolean | no | Toggle public access |
| `grant` | object | no | Grant `{operation, role}` |
| `revoke` | object | no | Revoke `{operation, role}` |

### `get_amoeba_help`

Get help and best practices for Amoeba concepts.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `topic` | enum | no | `overview`, `scale-to-zero`, `auth`, `permissions`, `secrets`, `resources`, or `dockerfile-tips` |

## The "one-shot" loop

This is the core value proposition of AI + Amoeba. When a deployment fails:

1. The model calls `deploy_service` → it fails
2. The model calls `get_build_logs` → sees the build error
3. The model fixes the Dockerfile/code
4. The model calls `deploy_service` again → it succeeds
5. The model calls `get_runtime_logs` → verifies the app works

All in one conversation. No human intervention needed.

## Architecture decisions

### Why not use Amoeba's Rust crate directly?

The MCP server talks to Amoeba over HTTP rather than linking against it as a library. This means:

- The MCP server can run on a different machine from Amoeba.
- Amoeba's hot-reload and catalog serialization remain the single source of truth.
- The MCP server can be written in any language (this one is Rust, but a Python/Node version would be equally valid).
- Build isolation: the MCP server shells out to `docker build`, so build artifacts never touch Amoeba's process.

### Why modify services.json directly for `update_policy`?

The admin API has `POST /admin/apps` (install) and `DELETE /admin/apps/:name` (uninstall), but no `PATCH` for fine-grained field updates. For policy changes, the MCP server reads `services.json`, modifies the service entry, and writes it back — Amoeba's file watcher picks up the change and hot-reloads within milliseconds. This is safe because Amoeba uses `ArcSwap` for atomic catalog swaps, so readers never see a partially-written file.

A future Amoeba release should add `PATCH /admin/apps/:name` to make this cleaner.

### Build step

The MCP server includes a build step because most AI chat clients can't run `docker build` themselves. The flow:

1. For `source_kind: dockerfile`: write Dockerfile → `docker build` → `docker push` (if registry configured)
2. For `source_kind: image`: use the image reference directly (no build)
3. For `source_kind: auto`: extract tarball → `nixpacks build` (fallback to Dockerfile if present)

The resulting image reference is passed to Amoeba's `POST /admin/apps` as a single-image App Package.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

#### Triggering a Docker build from the MCP

When the AI model calls `deploy_service` with `source_kind: "dockerfile"`, the MCP server:

1. Creates a temp directory at `$AMOEBA_BUILD_DIR/<service_name>/`
2. Writes the Dockerfile content from the `source` parameter to that directory
3. Runs `docker build -t amoeba/<service_name>:latest .` in that directory
4. Captures stdout/stderr line-by-line into the build log
5. If `AMOEBA_REGISTRY` is set, runs `docker push` to push the image
6. On success, passes the image reference to Amoeba's `POST /admin/apps`

The AI model can then call `get_build_logs` to see the build output, spot errors, fix the Dockerfile, and re-deploy — all in one conversation.

**Example conversation flow:**
```
User: Deploy a PDF-to-text converter on port 8080
AI:   [calls deploy_service with a Dockerfile]
      → Build failed: "python:3.12-slmi" not found

AI:   [calls get_build_logs]
      → Sees the typo in the image name

AI:   [calls deploy_service again with fixed Dockerfile]
      → Build succeeds, service is ready at /v1/pdf-converter/

AI:   [calls get_runtime_logs]
      → Verifies the app started correctly
```

#### Without a registry (local Docker)

When `AMOEBA_REGISTRY` is not set, the MCP server tags images as `amoeba/<service_name>:latest` locally. This works when:
- The MCP server and Amoeba run on the same Docker host
- Amoeba is configured to pull from the local Docker image store

For production or multi-host setups, set `AMOEBA_REGISTRY` to push images to a registry that Amoeba's Docker daemon can pull from.

## Security

- **Authentication**: The MCP server authenticates to Amoeba using `AMOEBA_TOKEN`. Without this token, all admin API calls fail with 401.
- **Secrets**: Secrets passed to `deploy_service` are written to Amoeba's secrets directory with 0600 permissions and injected as env vars at container start. They never appear in `services.json` or build logs.
- **Build isolation**: Builds run in `AMOEBA_BUILD_DIR`, a temp directory. Each service gets its own subdirectory.
- **Docker socket access**: The MCP server needs access to the Docker socket for builds and log retrieval. In production, consider running it in a container with the socket mounted read-only.
- **No network exposure**: The MCP server communicates exclusively over stdio. It has no listening ports.

## Limitations

- **No compose support yet**: `deploy_service` only creates single-container (image) services. Multi-service compose stacks must be deployed manually or via a future update.
- **No build caching**: Each `deploy_service` does a fresh `docker build`. For production use, consider a registry with layer caching.
- **No log streaming**: `get_build_logs` returns the full log buffer. For long builds, poll it periodically.
- **Single Amoeba target**: The server connects to exactly one Amoeba gateway (configured via `AMOEBA_URL`).
