//! amoeba-mcp — MCP server that lets AI agents (Claude, ChatGPT, Gemini)
//! deploy and manage microservices on an Amoeba gateway.
//!
//! ## Protocol
//!
//! Implements the Model Context Protocol (MCP) over stdio (JSON-RPC 2.0).
//! The server is launched by the AI client as a subprocess and communicates
//! via stdin/stdout.
//!
//! ## Tools
//!
//! | Tool | Description |
//! |------|-------------|
//! | `deploy_service` | Deploy a service from source, an image, or a Dockerfile |
//! | `get_build_logs` | Fetch build/pull logs for a deploy in progress |
//! | `get_runtime_logs` | Fetch recent container stdout/stderr |
//! | `get_service_status` | Health, readiness, and resource usage for a service |
//! | `list_services` | List all deployed services |
//! | `delete_service` | Remove a service and its resources |
//! | `update_policy` | Update auth permissions or public flag for a service |
//! | `get_amoeba_help` | Get help about Amoeba concepts (scale-to-zero, auth, etc.) |

use amoeba::apps::deploy::{
    BootObservation, LifecycleView, advise_boot, count_diff_lines, image_reference, nixpacks_args,
    prepare_deploy_access, read_lifecycle,
};
use amoeba::apps::schema::{AppManifest, SecretInject};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ============================================================================
// JSON-RPC 2.0 types
// ============================================================================

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

// ============================================================================
// MCP Protocol types
// ============================================================================

#[derive(Debug, Serialize)]
struct InitializeResult {
    protocolVersion: String,
    capabilities: ServerCapabilities,
    serverInfo: ServerInfo,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<ResourcesCapability>,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Debug, Serialize)]
struct ResourcesCapability {
    subscribe: bool,
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct ListToolsResult {
    tools: Vec<ToolDef>,
}

#[derive(Debug, Serialize, Clone)]
struct ToolDef {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct CallToolResult {
    content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ToolContent {
    r#type: String, // "text" or "resource"
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<EmbeddedResource>,
}

#[derive(Debug, Serialize)]
struct EmbeddedResource {
    uri: String,
    mime_type: Option<String>,
    text: Option<String>,
}

// ============================================================================
// Configuration
// ============================================================================

struct McpConfig {
    amoeba_url: String,
    amoeba_token: String,
    registry: Option<String>,
    build_dir: PathBuf,
    /// Org used when a deploy does not name one.
    default_org: String,
    /// OCI runtime (`runsc`, `kata-runtime`, …). Unset keeps the daemon default.
    container_runtime: Option<String>,
}

impl McpConfig {
    fn from_env() -> Self {
        Self {
            amoeba_url: std::env::var("AMOEBA_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            amoeba_token: std::env::var("AMOEBA_TOKEN").unwrap_or_default(),
            registry: std::env::var("AMOEBA_REGISTRY")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            build_dir: std::env::var("AMOEBA_BUILD_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("amoeba-builds")),
            default_org: std::env::var("AMOEBA_ORG").unwrap_or_else(|_| "mcp".into()),
            container_runtime: std::env::var("AMOEBA_CONTAINER_RUNTIME")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        }
    }

    fn admin_url(&self, path: &str) -> String {
        format!("{}/admin{}", self.amoeba_url, path)
    }

    fn proxy_url(&self, service: &str) -> String {
        format!("{}/v1/{}/", self.amoeba_url, service)
    }
}

// ============================================================================
// Build log tracking
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct BuildLogEntry {
    timestamp: String,
    stream: String,
    line: String,
}

struct BuildJob {
    service_name: String,
    logs: Vec<BuildLogEntry>,
    finished: bool,
    success: bool,
    image_ref: Option<String>,
}

// ============================================================================
// Server state
// ============================================================================

struct McpState {
    config: McpConfig,
    client: reqwest::Client,
    builds: RwLock<HashMap<String, BuildJob>>,
}

// ============================================================================
// Tool definitions
// ============================================================================

fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "deploy_service".into(),
            description: "Deploy a microservice to Amoeba. Provide a name, port, and either a Dockerfile (as a string), a pre-built image reference (e.g. 'python:3.12-slim'), or auto-detection source (base64-encoded tarball of project). Returns the service URL and status. The service will scale-to-zero when idle.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name for the new service. Must be a valid DNS-like name with no path separators."
                    },
                    "source_kind": {
                        "type": "string",
                        "enum": ["dockerfile", "image", "auto"],
                        "description": "How to build the service: 'dockerfile' for a Dockerfile string, 'image' for a pre-built image reference, 'auto' for auto-detection via Nixpacks."
                    },
                    "source": {
                        "type": "string",
                        "description": "The source content: Dockerfile text, image reference string, or base64-encoded tarball for auto-detection."
                    },
                    "port": {
                        "type": "integer",
                        "description": "Port the service listens on inside the container."
                    },
                    "cooldown_seconds": {
                        "type": "integer",
                        "description": "Idle seconds before scale-to-zero. Omit for always-on."
                    },
                    "env": {
                        "type": "object",
                        "description": "Environment variables to inject into the container.",
                        "additionalProperties": { "type": "string" }
                    },
                    "secrets": {
                        "type": "object",
                        "description": "Secret values stored securely (never visible in logs or config).",
                        "additionalProperties": { "type": "string" }
                    },
                    "permissions": {
                        "type": "object",
                        "description": "Single-policy map of operation to roles. Use with tenant for one org, or alone for a tenant-unaware service. Omit both this and tenant_permissions to bind the service to AMOEBA_ORG and a scoped role.",
                        "additionalProperties": { "type": "array", "items": { "type": "string" } }
                    },
                    "tenant": {
                        "type": "string",
                        "description": "Single-tenant form: only this org may call, using permissions."
                    },
                    "tenant_permissions": {
                        "type": "object",
                        "description": "Multi-tenant form: org id to a complete operation-to-roles map. Omit tenant and permissions when using this.",
                        "additionalProperties": {
                            "type": "object",
                            "additionalProperties": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "context_b64": {
                        "type": "string",
                        "description": "Optional base64 tarball extracted as the docker build context before the Dockerfile is written. Required when the Dockerfile COPYs project files."
                    },
                    "public": {
                        "type": "boolean",
                        "description": "If true, no JWT is required to reach this service."
                    },
                    "memory": {
                        "type": "string",
                        "description": "Memory limit (e.g. '512Mi', '2Gi')."
                    },
                    "cpu_cores": {
                        "type": "integer",
                        "description": "CPU core limit."
                    },
                    "readiness_timeout_secs": {
                        "type": "integer",
                        "description": "How long to wait for the service to become ready (default 120)."
                    }
                },
                "required": ["name", "source_kind", "source", "port"]
            }),
        },
        ToolDef {
            name: "get_build_logs".into(),
            description: "Get build or pull logs for a service deployment. Useful when a deploy is in progress or failed — the model can read the error, fix the code, and deploy again.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Service name whose build logs to fetch."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "get_runtime_logs".into(),
            description: "Get recent stdout/stderr logs from a running service container. Use this to debug runtime errors, check application output, or verify the service is working correctly.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Service name to fetch logs for."
                    },
                    "tail": {
                        "type": "integer",
                        "description": "Number of tail lines (default 50)."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "get_service_status".into(),
            description: "Get the current status of a deployed service: lifecycle state (installed/pulling/ready/error), port, public flag, and whether it's responding to requests.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Service name to check."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "list_services".into(),
            description: "List all services currently deployed on the Amoeba gateway with their status.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDef {
            name: "delete_service".into(),
            description: "Delete a deployed service. This removes the service from the catalog, stops its containers, and cleans up its secrets and stack files.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Service name to delete."
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "update_policy".into(),
            description: "Update a service's access policy: set which roles can perform which operations, or toggle the public flag. Changes take effect immediately via hot-reload.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Service name to update."
                    },
                    "permissions": {
                        "type": "object",
                        "description": "Replace the entire permissions map (operation -> roles).",
                        "additionalProperties": { "type": "array", "items": { "type": "string" } }
                    },
                    "tenant": {
                        "type": "string",
                        "description": "Bind the service to one org and use permissions as that org's policy."
                    },
                    "clear_tenant": {
                        "type": "boolean",
                        "description": "Remove the single-tenant binding."
                    },
                    "tenant_permissions": {
                        "type": "object",
                        "description": "Replace the policy with per-org maps. This clears tenant and permissions.",
                        "additionalProperties": {
                            "type": "object",
                            "additionalProperties": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "clear_tenant_permissions": {
                        "type": "boolean",
                        "description": "Remove per-org maps."
                    },
                    "public": {
                        "type": "boolean",
                        "description": "Toggle public access (no JWT required)."
                    },
                    "grant": {
                        "type": "object",
                        "description": "Grant a role access to an operation.",
                        "properties": {
                            "org": { "type": "string" },
                            "operation": { "type": "string" },
                            "role": { "type": "string" }
                        },
                        "required": ["operation", "role"]
                    },
                    "revoke": {
                        "type": "object",
                        "description": "Revoke a role's access to an operation.",
                        "properties": {
                            "org": { "type": "string" },
                            "operation": { "type": "string" },
                            "role": { "type": "string" }
                        },
                        "required": ["operation", "role"]
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDef {
            name: "get_amoeba_help".into(),
            description: "Get help and best practices for deploying services on Amoeba. Covers scale-to-zero, auth model, permission system, resource limits, and common patterns. Use this when the user asks 'how do I...' about Amoeba concepts.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "enum": ["scale-to-zero", "auth", "permissions", "secrets", "resources", "dockerfile-tips", "overview"],
                        "description": "Which Amoeba concept to explain."
                    }
                }
            }),
        },
    ]
}

// ============================================================================
// Help topics
// ============================================================================

fn help_topic(topic: &str) -> &'static str {
    match topic {
        "scale-to-zero" => "\
# Amoeba Scale-to-Zero

Services on Amoeba can scale to zero when idle, saving compute resources.

## How it works
- Each service has a `cooldown_seconds` config (e.g. 300 = 5 minutes).
- When a service has no active connections for the cooldown period, Amoeba stops its container.
- The next request cold-boots it automatically (typically <400ms overhead for simple containers).
- Services with no cooldown set are 'always-on' and never scale down.

## Best practices for AI-deployed services
- Set cooldown_seconds to 300 (5 min) for most tools and APIs.
- For services with heavy model-loading (LLMs), use longer cooldowns (1800s = 30 min).
- Ensure your container starts fast: avoid heavy init scripts, use multi-stage Docker builds.
- Don't write to local disk — containers may be stopped and recreated.
- Use Amoeba secrets for any credentials, never hardcode them.",

        "auth" => "\
# Amoeba Authentication

Amoeba uses JWT-based authentication with two modes:

## Local JWT mode (default)
- Users are managed via `/admin/users` API (create/update/delete).
- Tokens are obtained via `POST /auth/login` with username/password.
- Tokens include roles and optional org_id for multi-tenancy.

## JWKS mode
- Amoeba validates tokens against a remote JWKS endpoint.
- Useful for integrating with corporate identity providers (Okta, Auth0, etc.).

## Token format
- All requests to non-public services need `Authorization: Bearer <token>`.
- The proxy validates the token, checks permissions, and forwards the request.
- Upstream services never see the JWT (unless upstream_auth is configured).",

        "permissions" => "\
# Amoeba Permission System

Every service has a permission matrix mapping operations to allowed roles.

## Operations
- `read` — GET, HEAD requests
- `add` — POST requests
- `update` — PUT, PATCH requests
- `delete` — DELETE requests

## Configuring permissions
```yaml
permissions:
  read: [admin, viewer, analyst]
  add: [admin]
  update: [admin]
  delete: [admin]
```

## Public services
Set `public: true` to skip authentication entirely. The service is open to anyone who can reach the gateway. Use with caution.

## Best practices
- Default-deny: if an operation isn't listed, no role can perform it.
- Create dedicated roles for AI-deployed services (e.g. 'mcp-user').
- Use the `update_policy` tool to adjust permissions after deployment.",

        "secrets" => "\
# Amoeba Secrets

Secrets are stored encrypted on disk and injected into containers at startup.

## How to use
1. Pass secrets in the `secrets` field of `deploy_service`.
2. Amoeba writes them to `/etc/amoeba/secrets/<service>/<key>`.
3. At container start, secrets are injected as environment variables via `env_from_secret`.

## Security properties
- Secrets never appear in services.json or compose files.
- Secret files have 0600 permissions.
- Secrets are resolved fresh on every cold-boot.
- The container sees them as regular env vars, but the gateway never logs them.

## Best practices
- Never put secrets in Dockerfiles or source code.
- Use different secrets per service, even if they have the same value.
- Rotate secrets by re-deploying with new values.",

        "resources" => "\
# Amoeba Resource Limits

Amoeba enforces declared capacity budgets to prevent resource starvation.

## Declaring limits
- `memory`: Kubernetes-style quantity (e.g. '512Mi', '2Gi', '1Ti').
- `cpu_cores`: Integer count of CPU cores.
- `gpu_vram`: GPU memory (e.g. '8Gi').

## Capacity gating
- Each machine declares its total budget in services.json.
- When a cold-boot is triggered, Amoeba sums currently-occupying siblings.
- If the new service would exceed the budget, the request gets 503 'machine-capacity'.

## Best practices
- Set realistic memory limits: Python services typically need 256Mi-512Mi.
- GPU services should always declare gpu_vram to avoid overcommitting.
- For CPU-bound services, declare cpu_cores to ensure fair scheduling.",

        "dockerfile-tips" => "\
# Dockerfile Tips for AI-Deployed Services on Amoeba

Since Amoeba cold-boots services on demand, fast startup is critical.

## Do
- Use multi-stage builds to keep images small.
- Use slim/alpine base images when possible.
- Copy only what's needed (use .dockerignore).
- HEALTHCHECK for complex services (Amoeba probes TCP port).
- EXPOSE the correct port.

## Don't
- Don't install build tools in the final image.
- Don't run database migrations at container start (do it at deploy time).
- Don't write to local disk — use external volumes or object storage.
- Don't use 'latest' tag for production dependencies.

## Example: Fast Python service
```dockerfile
FROM python:3.12-slim
WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY . .
EXPOSE 8080
CMD [\"uvicorn\", \"main:app\", \"--host\", \"0.0.0.0\", \"--port\", \"8080\"]
```

## Cold-boot profiling
After deploying, check startup time with `get_runtime_logs`. If it's >10s, consider:
- Using a smaller base image.
- Pre-downloading model weights in the image.
- Removing unnecessary dependencies.",

        "overview" => "\
# Amoeba Overview for AI Agents

Amoeba is a scale-to-zero compute gateway. Think of it as 'serverless containers' that you control.

## Key concepts
- **Services** are containers (or compose stacks) that handle HTTP requests.
- **Scale-to-zero** means containers stop when idle and start on demand.
- **Zero-trust auth** means every request is authenticated and authorized.
- **Capacity gating** prevents overcommitting machine resources.

## Service URL pattern
```
http://<gateway>/v1/<service_name>/<path>
```

## Typical workflow for AI-deployed services
1. `deploy_service` — ship the code/image.
2. `get_build_logs` — check the build succeeded.
3. `get_service_status` — verify it's ready.
4. `get_runtime_logs` — debug any runtime issues.
5. `update_policy` — set who can access it.
6. `delete_service` — remove when no longer needed.

## The 'one-shot' loop
If deployment fails, the model can read the error via `get_build_logs` or `get_runtime_logs`, fix the code, and deploy again — all in one conversation. This is the core value prop of AI + Amoeba.",

        _ => "Unknown topic. Try: scale-to-zero, auth, permissions, secrets, resources, dockerfile-tips, overview.",
    }
}

// ============================================================================
// Build helpers
// ============================================================================

async fn append_build_log(state: &McpState, name: &str, stream: &str, line: &str) {
    let mut builds = state.builds.write().await;
    if let Some(job) = builds.get_mut(name) {
        job.logs.push(BuildLogEntry {
            timestamp: timestamp_now(),
            stream: stream.to_string(),
            line: line.to_string(),
        });
    }
}

async fn finish_build(state: &McpState, name: &str, success: bool, image_ref: Option<String>) {
    let mut builds = state.builds.write().await;
    if let Some(job) = builds.get_mut(name) {
        job.finished = true;
        job.success = success;
        job.image_ref = image_ref;
    }
}

fn timestamp_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    format!("T{hours:02}:{minutes:02}:{seconds:02}Z")
}

async fn extract_context(
    state: &McpState,
    service_name: &str,
    source_b64: &str,
) -> Result<(), String> {
    let build_ctx = state.config.build_dir.join(service_name);
    tokio::fs::create_dir_all(&build_ctx)
        .await
        .map_err(|e| format!("failed to create build dir: {e}"))?;

    let source_bytes = base64::engine::general_purpose::STANDARD
        .decode(source_b64)
        .map_err(|e| format!("failed to decode base64 source: {e}"))?;

    let tar_path = build_ctx.join("source.tar");
    tokio::fs::write(&tar_path, &source_bytes)
        .await
        .map_err(|e| format!("failed to write source archive: {e}"))?;

    let extract = Command::new("tar")
        .args(["-xf", "source.tar"])
        .current_dir(&build_ctx)
        .output()
        .await
        .map_err(|e| format!("tar extract failed: {e}"))?;

    if !extract.status.success() {
        return Err(format!(
            "failed to extract source: {}",
            String::from_utf8_lossy(&extract.stderr)
        ));
    }
    let _ = tokio::fs::remove_file(&tar_path).await;
    Ok(())
}

async fn build_dockerfile(
    state: &McpState,
    service_name: &str,
    dockerfile: &str,
    context_b64: Option<&str>,
) -> Result<String, String> {
    let build_ctx = state.config.build_dir.join(service_name);
    tokio::fs::create_dir_all(&build_ctx)
        .await
        .map_err(|e| format!("failed to create build dir: {e}"))?;

    if let Some(context_b64) = context_b64.filter(|value| !value.is_empty()) {
        extract_context(state, service_name, context_b64).await?;
    }

    tokio::fs::write(build_ctx.join("Dockerfile"), dockerfile)
        .await
        .map_err(|e| format!("failed to write Dockerfile: {e}"))?;

    let tag = image_reference(state.config.registry.as_deref(), service_name);

    append_build_log(
        state,
        service_name,
        "stdout",
        &format!("Building image {tag}..."),
    )
    .await;

    let output = Command::new("docker")
        .args(["build", "-t", &tag, "."])
        .current_dir(&build_ctx)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("docker build failed to start: {e}"))?;

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        append_build_log(state, service_name, "stdout", line).await;
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        if !line.is_empty() {
            append_build_log(state, service_name, "stderr", line).await;
        }
    }

    if !output.status.success() {
        finish_build(state, service_name, false, None).await;
        return Err(format!(
            "docker build failed with exit code {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    append_build_log(
        state,
        service_name,
        "stdout",
        &format!("Build succeeded: {tag}"),
    )
    .await;

    // Push if registry is configured.
    if state.config.registry.is_some() {
        append_build_log(state, service_name, "stdout", &format!("Pushing {tag}...")).await;
        let push = Command::new("docker")
            .args(["push", &tag])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("docker push failed to start: {e}"))?;

        for line in String::from_utf8_lossy(&push.stdout).lines() {
            append_build_log(state, service_name, "stdout", line).await;
        }
        for line in String::from_utf8_lossy(&push.stderr).lines() {
            if !line.is_empty() {
                append_build_log(state, service_name, "stderr", line).await;
            }
        }

        if !push.status.success() {
            finish_build(state, service_name, false, None).await;
            return Err(format!(
                "docker push failed: {}",
                String::from_utf8_lossy(&push.stderr)
            ));
        }
    }

    finish_build(state, service_name, true, Some(tag.clone())).await;
    Ok(tag)
}

async fn build_auto(
    state: &McpState,
    service_name: &str,
    source_b64: &str,
) -> Result<String, String> {
    let build_ctx = state.config.build_dir.join(service_name);
    tokio::fs::create_dir_all(&build_ctx)
        .await
        .map_err(|e| format!("failed to create build dir: {e}"))?;

    extract_context(state, service_name, source_b64).await?;

    let tag = image_reference(state.config.registry.as_deref(), service_name);

    append_build_log(state, service_name, "stdout", "Running nixpacks build...").await;

    let output = Command::new("nixpacks")
        .args(nixpacks_args(&tag))
        .current_dir(&build_ctx)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match output {
        Ok(output) => {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                append_build_log(state, service_name, "stdout", line).await;
            }
            for line in String::from_utf8_lossy(&output.stderr).lines() {
                if !line.is_empty() {
                    append_build_log(state, service_name, "stderr", line).await;
                }
            }

            if !output.status.success() {
                // Fallback to Dockerfile if present.
                if build_ctx.join("Dockerfile").exists() {
                    append_build_log(
                        state,
                        service_name,
                        "stdout",
                        "nixpacks failed, falling back to Dockerfile...",
                    )
                    .await;
                    let dockerfile = tokio::fs::read_to_string(build_ctx.join("Dockerfile"))
                        .await
                        .map_err(|e| format!("failed to read Dockerfile: {e}"))?;
                    return build_dockerfile(state, service_name, &dockerfile, None).await;
                }

                finish_build(state, service_name, false, None).await;
                return Err(format!(
                    "nixpacks build failed (and no Dockerfile fallback): {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        Err(e) => {
            // nixpacks not installed — fall back to Dockerfile if present.
            if build_ctx.join("Dockerfile").exists() {
                append_build_log(
                    state,
                    service_name,
                    "stdout",
                    "nixpacks not found, falling back to Dockerfile...",
                )
                .await;
                let dockerfile = tokio::fs::read_to_string(build_ctx.join("Dockerfile"))
                    .await
                    .map_err(|e| format!("failed to read Dockerfile: {e}"))?;
                return build_dockerfile(state, service_name, &dockerfile, None).await;
            }
            finish_build(state, service_name, false, None).await;
            return Err(format!(
                "nixpacks not available and no Dockerfile found: {e}"
            ));
        }
    }

    if state.config.registry.is_some() {
        append_build_log(state, service_name, "stdout", &format!("Pushing {tag}...")).await;
        let push = Command::new("docker")
            .args(["push", &tag])
            .output()
            .await
            .map_err(|e| format!("docker push failed: {e}"))?;

        if !push.status.success() {
            finish_build(state, service_name, false, None).await;
            return Err(format!(
                "docker push failed: {}",
                String::from_utf8_lossy(&push.stderr)
            ));
        }
    }

    finish_build(state, service_name, true, Some(tag.clone())).await;
    Ok(tag)
}

// ============================================================================
// Amoeba API wrappers
// ============================================================================

async fn install_app_on_amoeba(
    state: &McpState,
    name: &str,
    image: &str,
    port: u16,
    cooldown_seconds: Option<u64>,
    env: &HashMap<String, String>,
    secrets: &HashMap<String, String>,
    access: &amoeba::apps::deploy::DeployAccess,
    public: bool,
    memory: Option<&str>,
    cpu_cores: Option<u64>,
) -> Result<(u16, String), String> {
    let manifest = AppManifest {
        api_version: "v1".into(),
        name: name.to_string(),
        version: "1.0.0".into(),
        description: format!("AI-deployed service: {name}"),
        author: "amoeba-mcp".into(),
        app: amoeba::apps::schema::AppSpec::Image {
            image: image.to_string(),
        },
        placement: amoeba::apps::schema::PlacementSpec {
            port,
            cooldown_seconds,
            machine: None,
            runtime: state.config.container_runtime.clone(),
        },
        permissions: access.permissions.clone(),
        tenant: access.tenant.clone(),
        tenant_permissions: access.tenant_permissions.clone(),
        public,
        upstream_auth: None,
        resources: if memory.is_some() || cpu_cores.is_some() {
            Some(amoeba::apps::schema::ResourceSpec {
                memory: memory.map(|m| m.to_string()),
                cpu_cores,
                gpu_vram: None,
            })
        } else {
            None
        },
        schema: amoeba::apps::schema::AppSchema {
            env: env
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        amoeba::apps::schema::EnvField {
                            description: String::new(),
                            r#type: "string".into(),
                            default: Some(v.clone()),
                            options: None,
                        },
                    )
                })
                .collect(),
            secrets: secrets
                .iter()
                .map(|(k, _)| {
                    (
                        k.clone(),
                        amoeba::apps::schema::SecretField {
                            description: String::new(),
                            required: true,
                            validation_pattern: None,
                            file: None,
                            inject: SecretInject::Proxy,
                        },
                    )
                })
                .collect(),
        },
    };

    let manifest_yaml = serde_yaml::to_string(&manifest)
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;

    // Build a zip in memory.
    let mut zip_buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
        zip.start_file("amoeba.yaml", options)
            .map_err(|e| format!("zip error: {e}"))?;
        std::io::Write::write_all(&mut zip, manifest_yaml.as_bytes())
            .map_err(|e| format!("zip write error: {e}"))?;
        zip.finish().map_err(|e| format!("zip finish error: {e}"))?;
    }

    let values = amoeba::apps::schema::InstallValues {
        env: env.clone(),
        secrets: secrets.clone(),
    };
    let values_json =
        serde_json::to_string(&values).map_err(|e| format!("failed to serialize values: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .part(
            "package",
            reqwest::multipart::Part::bytes(zip_buf)
                .file_name(format!("{name}.zip"))
                .mime_str("application/zip")
                .map_err(|e| format!("mime error: {e}"))?,
        )
        .part(
            "values",
            reqwest::multipart::Part::text(values_json)
                .mime_str("application/json")
                .map_err(|e| format!("mime error: {e}"))?,
        );

    let resp = state
        .client
        .post(state.config.admin_url("/apps"))
        .header(
            "Authorization",
            format!("Bearer {}", state.config.amoeba_token),
        )
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();

    Ok((status, body))
}

async fn fetch_service_info(
    state: &McpState,
    name: &str,
) -> Result<Option<serde_json::Value>, String> {
    let resp = state
        .client
        .get(state.config.admin_url(&format!("/apps/{name}")))
        .header(
            "Authorization",
            format!("Bearer {}", state.config.amoeba_token),
        )
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Amoeba API error (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse response: {e}"))?;

    Ok(Some(body))
}

fn parse_role_map(value: &serde_json::Value) -> HashMap<String, Vec<String>> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, roles)| {
                    let roles = roles
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|role| role.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (key.clone(), roles)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_role_map_of_orgs(
    value: &serde_json::Value,
) -> HashMap<String, HashMap<String, Vec<String>>> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(org, map)| (org.clone(), parse_role_map(map)))
                .collect()
        })
        .unwrap_or_default()
}

async fn mint_scoped_token(
    state: &McpState,
    name: &str,
    org: Option<&str>,
) -> Result<String, String> {
    let mut body = serde_json::Map::new();
    if let Some(org) = org {
        body.insert("org_id".into(), serde_json::Value::String(org.to_string()));
    }
    let resp = state
        .client
        .post(state.config.admin_url(&format!("/apps/{name}/token")))
        .header(
            "Authorization",
            format!("Bearer {}", state.config.amoeba_token),
        )
        .json(&serde_json::Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse token response: {e}"))?;
    if !status.is_success() {
        return Err(format!("token request failed (HTTP {status}): {payload}"));
    }
    payload["token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "token response did not include a token".into())
}

async fn observe_boot(name: &str, boot_ms: u64) -> BootObservation {
    let local_writes = match Command::new("docker").args(["diff", name]).output().await {
        Ok(output) if output.status.success() => {
            Some(count_diff_lines(&String::from_utf8_lossy(&output.stdout)))
        }
        _ => None,
    };

    let survived_restart = match Command::new("docker")
        .args(["restart", name])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            match Command::new("docker")
                .args(["inspect", "-f", "{{.State.Running}}", name])
                .output()
                .await
            {
                Ok(inspect) if inspect.status.success() => {
                    Some(String::from_utf8_lossy(&inspect.stdout).trim() == "true")
                }
                _ => None,
            }
        }
        _ => None,
    };

    BootObservation {
        boot_ms,
        local_writes,
        survived_restart,
    }
}

async fn list_installed_apps(state: &McpState) -> Result<Vec<String>, String> {
    let resp = state
        .client
        .get(state.config.admin_url("/apps"))
        .header(
            "Authorization",
            format!("Bearer {}", state.config.amoeba_token),
        )
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Amoeba API error: {body}"));
    }

    #[derive(Deserialize)]
    struct ListBody {
        apps: Vec<String>,
    }

    let body: ListBody = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse response: {e}"))?;

    Ok(body.apps)
}

async fn delete_app_on_amoeba(state: &McpState, name: &str) -> Result<(u16, String), String> {
    let resp = state
        .client
        .delete(state.config.admin_url(&format!("/apps/{name}")))
        .header(
            "Authorization",
            format!("Bearer {}", state.config.amoeba_token),
        )
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body))
}

// ============================================================================
// Tool dispatch
// ============================================================================

async fn handle_tool_call(
    state: &McpState,
    tool_name: &str,
    arguments: Option<serde_json::Value>,
) -> Result<CallToolResult, String> {
    let args = arguments.unwrap_or(serde_json::Value::Object(Default::default()));

    match tool_name {
        "deploy_service" => {
            let name = args["name"].as_str().unwrap_or("").to_string();
            let source_kind = args["source_kind"].as_str().unwrap_or("image").to_string();
            let source = args["source"].as_str().unwrap_or("").to_string();
            let port = args["port"].as_u64().unwrap_or(8080) as u16;
            let cooldown_seconds = args["cooldown_seconds"].as_u64();
            let public = args["public"].as_bool().unwrap_or(false);
            let memory = args["memory"].as_str().map(|s| s.to_string());
            let cpu_cores = args["cpu_cores"].as_u64();
            let readiness_timeout = args["readiness_timeout_secs"].as_u64().unwrap_or(120);

            let env: HashMap<String, String> = args["env"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let secrets: HashMap<String, String> = args["secrets"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let permissions = parse_role_map(&args["permissions"]);
            let tenant = args["tenant"].as_str().map(|s| s.to_string());
            let tenant_permissions = args
                .get("tenant_permissions")
                .filter(|value| !value.is_null())
                .map(parse_role_map_of_orgs);
            let context_b64 = args["context_b64"].as_str().map(|s| s.to_string());

            if name.is_empty() || name.contains('/') || name.contains('\\') {
                return Err("name must be non-empty and not contain path separators".into());
            }

            // Register build job.
            {
                let mut builds = state.builds.write().await;
                builds.insert(
                    name.clone(),
                    BuildJob {
                        service_name: name.clone(),
                        logs: Vec::new(),
                        finished: false,
                        success: false,
                        image_ref: None,
                    },
                );
            }

            // Build image.
            let image_ref = match source_kind.as_str() {
                "image" => {
                    let img = source.trim().to_string();
                    finish_build(state, &name, true, Some(img.clone())).await;
                    img
                }
                "dockerfile" => {
                    build_dockerfile(state, &name, &source, context_b64.as_deref()).await?
                }
                "auto" => build_auto(state, &name, &source).await?,
                _ => return Err(format!("unknown source_kind: {source_kind}")),
            };

            if public && (tenant.is_some() || tenant_permissions.is_some()) {
                return Err("a public service cannot set tenant or tenant_permissions".into());
            }

            let access = if public {
                amoeba::apps::deploy::DeployAccess {
                    tenant: None,
                    permissions,
                    tenant_permissions: None,
                    token_org: None,
                    token_role: String::new(),
                }
            } else {
                prepare_deploy_access(
                    &name,
                    tenant,
                    permissions,
                    tenant_permissions,
                    &state.config.default_org,
                )?
            };

            // Install on Amoeba.
            let (status, body) = install_app_on_amoeba(
                state,
                &name,
                &image_ref,
                port,
                cooldown_seconds,
                &env,
                &secrets,
                &access,
                public,
                memory.as_deref(),
                cpu_cores,
            )
            .await?;

            if status >= 400 {
                finish_build(state, &name, false, None).await;
                return Err(format!("Amoeba install failed (HTTP {status}): {body}"));
            }

            let started = std::time::Instant::now();
            let deadline = started + std::time::Duration::from_secs(readiness_timeout);
            let mut lifecycle = LifecycleView::Pending;
            while std::time::Instant::now() < deadline {
                lifecycle = match fetch_service_info(state, &name).await? {
                    Some(body) => read_lifecycle(&body),
                    None => LifecycleView::Missing,
                };
                if !matches!(lifecycle, LifecycleView::Pending) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }

            let url = state.config.proxy_url(&name);
            let boot_ms = started.elapsed().as_millis() as u64;
            let (token_line, token_err) = if public {
                (String::new(), None)
            } else {
                match mint_scoped_token(state, &name, access.token_org.as_deref()).await {
                    Ok(token) => (
                        format!(
                            "\nToken: {token}\nRole: {}\nOrg: {}\n",
                            access.token_role,
                            access.token_org.as_deref().unwrap_or("(none)")
                        ),
                        None,
                    ),
                    Err(err) => (String::new(), Some(err)),
                }
            };

            let text = match &lifecycle {
                LifecycleView::Ready => {
                    let profile = observe_boot(&name, boot_ms).await;
                    let advice = advise_boot(&profile);
                    let advice_text = if advice.scale_to_zero {
                        "Boot profile: scale-to-zero is appropriate.".to_string()
                    } else {
                        format!(
                            "Boot profile: keep this service always-on (omit cooldown_seconds).\n{}",
                            advice.notes.join("\n")
                        )
                    };
                    let token_problem = token_err
                        .as_ref()
                        .map(|err| format!("\nScoped token was not issued: {err}"))
                        .unwrap_or_default();
                    format!(
                        "Service '{name}' is deployed and ready.\n\nURL: {url}\nImage: {image_ref}\nPort: {port}\nPublic: {public}{token_line}\n{advice_text}{token_problem}"
                    )
                }
                LifecycleView::Error { message } => format!(
                    "Service '{name}' installed but failed to become ready.\n\nURL: {url}\nImage: {image_ref}\nError: {message}"
                ),
                LifecycleView::Pending | LifecycleView::Missing => format!(
                    "Service '{name}' is installed but did not report ready within {readiness_timeout}s.\n\nURL: {url}\nImage: {image_ref}\n\nCall get_service_status. The lifecycle error text is in the message field."
                ),
            };

            let failed = !matches!(lifecycle, LifecycleView::Ready) || token_err.is_some();
            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(text),
                    resource: None,
                }],
                is_error: if failed { Some(true) } else { None },
            })
        }

        "get_build_logs" => {
            let name = args["name"].as_str().unwrap_or("").to_string();
            let builds = state.builds.read().await;
            let job = builds
                .get(&name)
                .ok_or_else(|| format!("no build found for service '{name}'"))?;

            let logs_text = job
                .logs
                .iter()
                .map(|e| format!("[{} {}] {}", e.timestamp, e.stream, e.line))
                .collect::<Vec<_>>()
                .join("\n");

            let status_line = if job.finished {
                if job.success {
                    format!(
                        "Build succeeded. Image: {}",
                        job.image_ref.as_deref().unwrap_or("unknown")
                    )
                } else {
                    "Build failed.".to_string()
                }
            } else {
                "Build still in progress...".to_string()
            };

            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(format!("{status_line}\n\n{logs_text}")),
                    resource: None,
                }],
                is_error: if job.finished && !job.success {
                    Some(true)
                } else {
                    None
                },
            })
        }

        "get_runtime_logs" => {
            let name = args["name"].as_str().unwrap_or("").to_string();
            let tail = args["tail"].as_u64().unwrap_or(50).to_string();

            let output = Command::new("docker")
                .args(["logs", "--tail", &tail, &name])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| format!("docker logs failed: {e}"))?;

            let mut logs = String::new();
            if !output.stdout.is_empty() {
                logs.push_str(&String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                if !logs.is_empty() {
                    logs.push('\n');
                }
                logs.push_str("--- stderr ---\n");
                logs.push_str(&String::from_utf8_lossy(&output.stderr));
            }

            if logs.is_empty() {
                logs = "(no logs found — container may be stopped or never produced output)".into();
            }

            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(logs),
                    resource: None,
                }],
                is_error: None,
            })
        }

        "get_service_status" => {
            let name = args["name"].as_str().unwrap_or("").to_string();

            let info = fetch_service_info(state, &name).await?;
            match info {
                Some(body) => {
                    let view = read_lifecycle(&body);
                    let (state_str, message, failed) = match view {
                        LifecycleView::Ready => ("ready", String::new(), false),
                        LifecycleView::Pending => ("pending", String::new(), false),
                        LifecycleView::Missing => ("missing", String::new(), false),
                        LifecycleView::Error { message } => ("error", message, true),
                    };

                    let mut text = format!("Service: {name}\nState: {state_str}\n");
                    if !message.is_empty() {
                        text.push_str(&format!("Message: {message}\n"));
                    }
                    text.push_str(&format!("URL: {}\n", state.config.proxy_url(&name)));

                    Ok(CallToolResult {
                        content: vec![ToolContent {
                            r#type: "text".into(),
                            text: Some(text),
                            resource: None,
                        }],
                        is_error: if failed { Some(true) } else { None },
                    })
                }
                None => Ok(CallToolResult {
                    content: vec![ToolContent {
                        r#type: "text".into(),
                        text: Some(format!("Service '{name}' is not installed.")),
                        resource: None,
                    }],
                    is_error: None,
                }),
            }
        }

        "list_services" => {
            let apps = list_installed_apps(state).await?;

            if apps.is_empty() {
                return Ok(CallToolResult {
                    content: vec![ToolContent {
                        r#type: "text".into(),
                        text: Some("No services are currently deployed on Amoeba.".into()),
                        resource: None,
                    }],
                    is_error: None,
                });
            }

            let mut lines = vec![format!("{} service(s) deployed:\n", apps.len())];
            for name in &apps {
                let info = fetch_service_info(state, name).await.ok().flatten();
                let state_str = info
                    .as_ref()
                    .and_then(|b| b["state"]["state"].as_str())
                    .unwrap_or("unknown");
                lines.push(format!("  - {name} ({state_str})"));
            }

            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(lines.join("\n")),
                    resource: None,
                }],
                is_error: None,
            })
        }

        "delete_service" => {
            let name = args["name"].as_str().unwrap_or("").to_string();

            let (status, body) = delete_app_on_amoeba(state, &name).await?;

            if status == 404 {
                return Ok(CallToolResult {
                    content: vec![ToolContent {
                        r#type: "text".into(),
                        text: Some(format!("Service '{name}' was not found.")),
                        resource: None,
                    }],
                    is_error: None,
                });
            }

            if status >= 400 {
                return Err(format!("Delete failed (HTTP {status}): {body}"));
            }

            // Clean up build job.
            {
                let mut builds = state.builds.write().await;
                builds.remove(&name);
            }

            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(format!("Service '{name}' deleted successfully.")),
                    resource: None,
                }],
                is_error: None,
            })
        }

        "update_policy" => {
            let name = args["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                return Err("name is required".into());
            }

            let mut patch = serde_json::Map::new();
            if args
                .get("permissions")
                .is_some_and(|value| value.is_object())
            {
                patch.insert("permissions".into(), args["permissions"].clone());
            }
            if args
                .get("tenant_permissions")
                .is_some_and(|value| value.is_object())
            {
                patch.insert(
                    "tenant_permissions".into(),
                    args["tenant_permissions"].clone(),
                );
            }
            if let Some(tenant) = args["tenant"].as_str() {
                patch.insert("tenant".into(), serde_json::Value::String(tenant.into()));
            }
            if let Some(clear_tenant) = args["clear_tenant"].as_bool() {
                patch.insert("clear_tenant".into(), serde_json::Value::Bool(clear_tenant));
            }
            if let Some(clear) = args["clear_tenant_permissions"].as_bool() {
                patch.insert(
                    "clear_tenant_permissions".into(),
                    serde_json::Value::Bool(clear),
                );
            }
            if let Some(public) = args["public"].as_bool() {
                patch.insert("public".into(), serde_json::Value::Bool(public));
            }
            if args.get("grant").is_some_and(|value| value.is_object()) {
                patch.insert("grant".into(), args["grant"].clone());
            }
            if args.get("revoke").is_some_and(|value| value.is_object()) {
                patch.insert("revoke".into(), args["revoke"].clone());
            }

            let resp = state
                .client
                .patch(state.config.admin_url(&format!("/apps/{name}")))
                .header(
                    "Authorization",
                    format!("Bearer {}", state.config.amoeba_token),
                )
                .json(&serde_json::Value::Object(patch))
                .send()
                .await
                .map_err(|e| format!("HTTP request failed: {e}"))?;

            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            if status >= 400 {
                return Err(format!("Policy update failed (HTTP {status}): {body}"));
            }

            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(format!(
                        "Policy updated for '{name}'. The catalog watcher hot-reloads the change.\n\n{body}"
                    )),
                    resource: None,
                }],
                is_error: None,
            })
        }

        "get_amoeba_help" => {
            let topic = args["topic"].as_str().unwrap_or("overview");
            Ok(CallToolResult {
                content: vec![ToolContent {
                    r#type: "text".into(),
                    text: Some(help_topic(topic).to_string()),
                    resource: None,
                }],
                is_error: None,
            })
        }

        _ => Err(format!("unknown tool: {tool_name}")),
    }
}

// ============================================================================
// JSON-RPC handler
// ============================================================================

fn make_response(id: Option<serde_json::Value>, result: serde_json::Value) -> String {
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    })
    .unwrap()
}

fn make_error(
    id: Option<serde_json::Value>,
    code: i32,
    message: String,
    data: Option<serde_json::Value>,
) -> String {
    serde_json::to_string(&JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data,
        }),
    })
    .unwrap()
}

fn make_notification(method: &str, params: Option<serde_json::Value>) -> String {
    serde_json::to_string(&JsonRpcNotification {
        jsonrpc: "2.0",
        method: method.to_string(),
        params,
    })
    .unwrap()
}

async fn handle_request(state: &McpState, req: JsonRpcRequest) -> Option<String> {
    match req.method.as_str() {
        "initialize" => {
            let result = InitializeResult {
                protocolVersion: "2024-11-05".into(),
                capabilities: ServerCapabilities {
                    tools: ToolsCapability {
                        list_changed: false,
                    },
                    resources: Some(ResourcesCapability {
                        subscribe: false,
                        list_changed: false,
                    }),
                },
                serverInfo: ServerInfo {
                    name: "amoeba-mcp".into(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            };
            Some(make_response(req.id, serde_json::to_value(result).unwrap()))
        }

        "tools/list" => {
            let tools = tool_definitions();
            let result = ListToolsResult { tools };
            Some(make_response(req.id, serde_json::to_value(result).unwrap()))
        }

        "tools/call" => {
            let tool_name = req
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");

            let arguments = req
                .params
                .as_ref()
                .and_then(|p| p.get("arguments").cloned());

            match handle_tool_call(state, tool_name, arguments).await {
                Ok(result) => Some(make_response(req.id, serde_json::to_value(result).unwrap())),
                Err(e) => {
                    // Return tool errors as successful JSON-RPC responses with
                    // is_error: true, per MCP spec.
                    let result = CallToolResult {
                        content: vec![ToolContent {
                            r#type: "text".into(),
                            text: Some(e),
                            resource: None,
                        }],
                        is_error: Some(true),
                    };
                    Some(make_response(req.id, serde_json::to_value(result).unwrap()))
                }
            }
        }

        "resources/list" => {
            // We don't expose resources yet, return empty list.
            let result = serde_json::json!({"resources": []});
            Some(make_response(req.id, result))
        }

        "notifications/initialized" => {
            // No response needed for notifications.
            None
        }

        "ping" => Some(make_response(req.id, serde_json::json!({}))),

        _ => Some(make_error(
            req.id,
            -32601,
            format!("Method not found: {}", req.method),
            None,
        )),
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("AMOEBA_MCP_LOG").unwrap_or_else(|_| "error".into()))
        .with_writer(std::io::stderr) // Log to stderr so stdout stays clean for JSON-RPC.
        .init();

    let config = McpConfig::from_env();

    if config.amoeba_token.is_empty() {
        warn!("AMOEBA_TOKEN is not set — requests to Amoeba will fail with 401");
    }

    info!(
        amoeba_url = %config.amoeba_url,
        registry = ?config.registry,
        "amoeba-mcp starting"
    );

    let state = Arc::new(McpState {
        config,
        client: reqwest::Client::new(),
        builds: RwLock::new(HashMap::new()),
    });

    // Read JSON-RPC one message at a time, but handle each request on its own
    // task so `get_build_logs` can run while `deploy_service` is still building.
    // Responses carry the request id, so they may arrive out of order.
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            _ => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let err = make_error(None, -32700, format!("Parse error: {e}"), None);
                let mut out = stdout.lock().await;
                let _ = out.write_all(err.as_bytes()).await;
                let _ = out.write_all(b"\n").await;
                let _ = out.flush().await;
                continue;
            }
        };

        let state = Arc::clone(&state);
        let stdout = Arc::clone(&stdout);
        tokio::spawn(async move {
            if let Some(response) = handle_request(&state, req).await {
                let mut out = stdout.lock().await;
                let _ = out.write_all(response.as_bytes()).await;
                let _ = out.write_all(b"\n").await;
                let _ = out.flush().await;
            }
        });
    }
}
