---
title: amoeba.yaml manifest reference
description: The app package manifest that tells Amoeba how to run an app and how callers use it. Field reference, validation rules and examples.
---

# `amoeba.yaml` manifest reference

`amoeba.yaml` describes one app for [Amoeba](../README.md): which image or compose stack to run, the port it listens on, who may call it, which settings and secrets it needs, and how callers use its API. Amoeba reads it when the app is installed, from an uploaded package, from the console's App library, or (planned) from the root of a GitHub repository.

- **Schema for editors and AI tools:** [`amoeba.schema.json`](amoeba.schema.json) (JSON Schema 2020-12). Add `# yaml-language-server: $schema=https://raw.githubusercontent.com/dickyj-grc/Amoeba/main/docs/amoeba.schema.json` as the first line of a manifest for completion and checks in VS Code.
- **Examples:** [`library/`](../library) and [`examples/app-packages/`](../examples/app-packages).
- **Package format:** a `.zip` with `amoeba.yaml` at its root, plus the compose file for compose apps and any age-encrypted secret files. Upload it with `POST /admin/apps` (multipart field `package`, optional `values` JSON).

## Minimal example

```yaml
api_version: v1
name: my-api
app:
  type: image
  image: ghcr.io/me/my-api:1.0
placement:
  port: 8080
permissions:
  read: [admin, analyst]
```

The app is then reachable at `https://<amoeba>/v1/my-api/...`. Callers send an Amoeba token; `GET` needs a role listed under `read`.

## Complete example: an MCP server from a published image

```yaml
api_version: v1
name: lightpanda
version: 1.0.0
description: Lightpanda headless browser for AI agents, over MCP.
app:
  type: image
  image: lightpanda/browser:latest
  # Runs the image's MCP mode instead of its default command. No build needed.
  # ${PORT} is placement.port, so the port is written once.
  command: ["/bin/lightpanda", "mcp", "--host", "0.0.0.0", "--port", "${PORT}"]
placement:
  port: 9222
  cooldown_seconds: 300
local_upstream: true
permissions:
  read: [admin, mcp:lightpanda]
  add: [admin, mcp:lightpanda]
  delete: [admin, mcp:lightpanda]
resources:
  memory: 1Gi
interface:
  type: mcp
  path: /mcp
  summary: MCP over HTTP at /mcp. Tools include goto, markdown, click, fill and extract.
  docs: https://lightpanda.io/docs/open-source/guides/mcp-server
  try:
    method: POST
    path: /mcp
    body: {jsonrpc: "2.0", id: 1, method: tools/list}
```

## Top-level fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `api_version` | string | yes | Always `v1`. |
| `name` | string | yes | Service name and URL segment: `/v1/<name>/`. Must not be empty or contain `/` or `\`. Use lowercase letters, digits, `.`, `_` and `-`: the console and the App library require that. Both can rename the app at install. |
| `version` | string | no | The app's version, shown in the console. |
| `description` | string | no | One or two sentences on what the app does. |
| `author` | string | no | Who maintains the package. |
| `app` | object | yes | What to run. See [`app`](#app). |
| `placement` | object | yes | Port and lifecycle. See [`placement`](#placement). |
| `permissions` | map | no | Operation to the roles allowed to perform it. See [Permissions](#permissions). Missing or empty grants nobody, unless `public` is true. |
| `operation_rules` | list | no | Names operations by path and method. See [`operation_rules`](#operation_rules). |
| `tenant` | string | no | Only this org may call; uses `permissions`. Not with `tenant_permissions` or `public`. |
| `tenant_permissions` | map | no | Org to its own permissions map. Replaces `permissions`; not with `tenant` or `public`. |
| `public` | bool | no | `true` skips authentication entirely. Default `false`. |
| `upstream_auth` | object | no | A credential Amoeba sends to the app on every request. See [`upstream_auth`](#upstream_auth). |
| `local_upstream` | bool | no | Call the app as a local client: drop the browser's `Origin`, send `Host: localhost:<port>`. For apps that refuse remote or browser callers, such as many MCP servers. Default `false`. |
| `resources` | object | no | Footprint while running, for capacity checks. See [`resources`](#resources). |
| `schema` | object | no | Settings and secrets the installer provides. See [`schema`](#schema). |
| `interface` | object | no | How callers use the app, shown by the console. See [`interface`](#interface). |

## `app`

Exactly one form, chosen by `type`.

**Single image**

| Field | Type | Meaning |
|---|---|---|
| `type` | `image` | |
| `image` | string | Image reference, e.g. `lightpanda/browser:latest` or `ghcr.io/org/app@sha256:...`. |
| `command` | list of strings | Replaces the image's `CMD`; its `ENTRYPOINT` stays, as with the arguments after the image in `docker run`. Use it to run a published image in another mode instead of building a new one. Each entry must be non-empty. Write `${PORT}` where the app needs its port (`["--port", "${PORT}"]` or `["--port=${PORT}"]`): it becomes `placement.port`, so the number appears once. No other `${...}` is allowed. |

The container also gets the env var `PORT` set to `placement.port`, so apps that read `PORT` need no command at all. Because of that, an image app's `schema` must not declare `PORT`.

**Compose stack**

| Field | Type | Meaning |
|---|---|---|
| `type` | `compose` | |
| `compose_file` | string | Path of the compose file inside the package, e.g. `compose.yaml`. Its services must join the external network `amoeba-net`. |
| `primary_service` | string | The compose service Amoeba routes to. Defaults to `name`. |

Environment values from `schema.env` are available to the compose file as `${KEY}`.

## `placement`

| Field | Type | Meaning |
|---|---|---|
| `port` | integer | **Required.** The port the app listens on inside its container, and the value of `${PORT}` and the `PORT` env var. If nothing answers there, the install fails with "nothing answered on port N", plus the port the image suggests when it differs. |
| `cooldown_seconds` | integer | Idle time before Amoeba stops the app (scale to zero). Omit to keep it running. The next request starts it again. |
| `machine` | string | Machine to place it on, when the gateway manages several. |
| `runtime` | string | OCI runtime for the Docker driver, e.g. `runsc` (gVisor). Must be installed on the host. |

## Permissions

Every request maps to one operation name; `permissions` lists the roles allowed to perform it. Without `operation_rules`, the operation comes from the HTTP method:

| Method | Operation |
|---|---|
| `GET`, `HEAD` | `read` |
| `POST` | `add` |
| `PUT`, `PATCH` | `update` |
| `DELETE` | `delete` |

```yaml
permissions:
  read: [admin, analyst]
  add: [admin]
```

Tokens issued from the console's Token dialog carry the role `mcp:<name>`. List it to let those tokens call the app. MCP over HTTP posts every call, so MCP servers need `add` (and `read` for the event stream, `delete` to end a session).

## `operation_rules`

Ordered rules that name operations, so `permissions` can grant them separately. The first match wins; no match falls back to the method mapping above.

| Field | Type | Meaning |
|---|---|---|
| `operation` | string | The name to use in `permissions`, e.g. `loans.read`. |
| `match_type` | `path` or `http_method` | Default `http_method`. |
| `path` | string | For `path` rules: the subpath after `/v1/<name>/`. `{id}` and `*` match one segment; a trailing `**` matches the rest. |
| `values` | list | For `path` rules, the methods it applies to (empty means any). For `http_method` rules, the methods. |

```yaml
operation_rules:
  - operation: loans.read
    match_type: path
    path: tables/loans/**
    values: [GET]
permissions:
  loans.read: [analyst]
```

## `upstream_auth`

For apps that require a credential on every request. Callers never see it: they authenticate to Amoeba with their own token, and Amoeba adds this one.

| Field | Type | Meaning |
|---|---|---|
| `type` | `bearer_static` or `custom_header` | `bearer_static` sends `Authorization: Bearer <secret>`. |
| `secret` | string | A key declared in `schema.secrets`. |
| `header_name` | string | Required for `custom_header`. |

## `resources`

| Field | Type | Meaning |
|---|---|---|
| `memory` | string | e.g. `512Mi`, `1Gi`. |
| `cpu_cores` | integer | |
| `gpu_vram` | string | e.g. `8Gi`. |

## `schema`

The inputs an installer fills in. The console's deploy form shows exactly these.

**`schema.env`**: non-secret settings, passed to the app as environment variables. Declare every variable the app reads that an installer may need to set; the install refuses keys it does not declare.

| Field | Type | Meaning |
|---|---|---|
| `description` | string | Shown next to the input. |
| `type` | `string`, `number`, `integer` or `boolean` | Default `string`. Values are checked against it; `boolean` means `true` or `false`. |
| `required` | bool | Default `false`. When `true` and there is no `default`, the install fails unless the installer gives a value. |
| `default` | string | Used when the installer leaves it empty. Must itself match `type` and `options`. |
| `options` | list | Allowed values; the console shows a dropdown. Must not be empty. |

An empty value counts as not given: the default applies, or the variable is left unset.

```yaml
schema:
  env:
    REGION:
      description: Region to serve
      required: true              # no default: the installer must choose
      options: [eu, us, ap]
    LOG_LEVEL:
      default: info
      options: [debug, info, warn]
    WORKERS:
      type: integer
      default: "4"
```

**`schema.secrets`**: sensitive values, stored in Amoeba's secrets directory, never in the catalog or package.

| Field | Type | Meaning |
|---|---|---|
| `description` | string | Shown next to the input. |
| `required` | bool | Default `true`. |
| `validation_pattern` | string | Regex the value must match, e.g. `^postgres(ql)?://`. |
| `inject` | `env` or `proxy` | `env` (default) sets it in the container environment. `proxy` keeps it on the gateway and sends it as `X-Amoeba-Secret-<KEY>` per request. |
| `file` | string | Path in the package of an age-encrypted file holding the value, decrypted at install. |

Secret keys become file names and header suffixes: letters, digits, `_`, `-` and `.`, starting with a letter, digit or `_`.

## `interface`

How callers use the app. The console shows it after deploy, starts its Test dialog with `try`, and uses `path` in the MCP client config it generates with a token.

| Field | Type | Meaning |
|---|---|---|
| `type` | `mcp`, `openapi`, `http` or `cdp` | The protocol. `cdp` (browser automation over WebSocket) is not relayed by the gateway yet. |
| `path` | string | Where the API lives under `/v1/<name>`, starting with `/`, e.g. `/mcp`. |
| `summary` | string | One or two sentences on how to call it. |
| `docs` | URL | Documentation for the API. |
| `try` | object | A sample request: `method` (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`), `path` (starts with `/`) and optional JSON `body`. |

## Validation

An install is refused, with a `400` naming the problem, when:

- `api_version` is not `v1`, `name` is empty or contains `/`, or `placement.port` is missing or `0`;
- `public` is combined with `tenant` or `tenant_permissions`, or `tenant` with `tenant_permissions`, or `tenant_permissions` with `permissions`;
- a secret key is unsafe, or `upstream_auth.secret` is not declared in `schema.secrets`, or `custom_header` has no `header_name`;
- a resource quantity cannot be parsed;
- an env field has an unknown `type`, empty `options`, or a `default` that does not match its `type` or `options`;
- an `interface` path does not start with `/`, `try.method` is not an HTTP method, or `docs` is not an http(s) URL;
- an `app.command` entry is empty or has a `${...}` other than `${PORT}`, or an image app's `schema` declares `PORT`;
- an `operation_rules` path pattern is invalid (for example `**` not at the end);
- a required secret is missing, a secret fails its `validation_pattern`, a required env value without a default is missing, an env value does not match its `type` or `options`, or the installer sends an env value or secret the manifest does not declare.

## Adding an app to the App library

The console's App library lists ready-made packages for known images, so users can deploy them without knowing their port, command or API. To add one:

1. Create `library/<id>/amoeba.yaml` with an `interface` block.
2. Add an entry to [`library/index.json`](../library/index.json): `id`, `title`, `summary`, `tags`, the `images` (without tags) and GitHub `repos` that should find it, and `package`.
3. Add the package's files to `PACKAGES` in `src/apps/library.rs`.
4. Run `cargo test`: it checks every entry is bundled, valid, matches its listed image and has an `interface`.
