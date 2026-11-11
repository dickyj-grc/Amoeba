# Deploy

How a service gets from the web console to the catalog: optionally build an image from source, then install it as an image app package.

Installing the package and handling its secrets is covered in [[secrets#Console deploy]].

## Source builds

The console's Git and Dockerfile tabs have no image yet, so Amoeba builds one on its own host and the deploy then runs that tag.

`POST /admin/apps/:name/build` (admin only) is [[src/routing/apps_admin.rs#build_app_image]]; the work is [[src/apps/build.rs#build_image]]. It returns `{image, log}`, or `422` with `{error, log}` when the clone or build fails. The log keeps the last 500 lines.

- `{"source":"git","url","ref"?}`: shallow-clone the repo and build with its root `Dockerfile`, or with nixpacks when there is none and nixpacks is installed ([[src/apps/build.rs#choose_builder]]).
- `{"source":"dockerfile","dockerfile","context_url"?,"context_ref"?}`: build the pasted Dockerfile. If `context_url` is set, the repo is cloned as the build context and the pasted file replaces its Dockerfile.

The tag comes from [[src/apps/deploy.rs#image_reference]]: `amoeba/<name>:latest`, or `<AMOEBA_REGISTRY>/<name>:latest` with a push when that variable is set. Each build runs in a fresh temp directory. Clone times out after 2 minutes, build after 15. The request blocks until the build finishes.

The MCP server's `deploy_service` tool has its own build code in `src/bin/amoeba-mcp.rs`; it does not call this endpoint.

### Input checks

Everything a caller sends is checked before any command runs, because the values become `git` and `docker` arguments.

- Service name: lowercase `[a-z0-9][a-z0-9._-]*`, since it becomes the image tag ([[src/apps/build.rs#validate_image_name]]).
- Git URLs: `https://` only, which rules out `file://`, `ext::` and option-looking values ([[src/apps/build.rs#validate_git_url]]). The URL is passed after `--`.
- Refs: no leading `-`, no `..`, only `[A-Za-z0-9._/-]` ([[src/apps/build.rs#validate_git_ref]]).
- Dockerfile: non-empty, at most 64 KiB.

Git runs with `GIT_TERMINAL_PROMPT=0` so a private repo fails instead of hanging.

### Console flow

The deploy form builds first for the Git and Dockerfile tabs, then installs the returned tag exactly like the Docker image tab.

[[web/src/lib/deployPackage.ts#buildRequestFor]] maps the tab to a build request and [[web/src/lib/deployPackage.ts#requestBuild]] posts it. On failure the modal shows the error and the last 40 log lines. The button reads "Building image..." and then "Deploying...".

## Service tokens

The console's token dialog issues a scoped JWT (role `mcp:<service>`) for one service. The token is shown once; the user keeps it in their AI app's MCP settings, like an API key.

[[web/src/lib/serviceToken.ts#mcpClientConfig]] builds an `mcpServers` entry pointing at `<console origin>/v1/<service>/<mcp path>` with the token as a bearer header. Copy falls back to a selected textarea where the Clipboard API is unavailable (plain-http consoles reached by IP).

### Lifetime, list and revoke

`POST /admin/apps/<name>/token` takes `ttl_seconds` (1 minute to 366 days, default 1 hour), a `label` and `org_id` ([[src/routing/apps_admin.rs#issue_service_token]]). The console defaults to 30 days because AI clients keep the token in their config.

`GET /admin/apps/<name>/tokens` lists unexpired tokens for the service, metadata only. `DELETE /admin/apps/<name>/tokens/<jti>` adds the `jti` to the persisted revocation list, so the token is rejected from then on ([[src/routing/apps_admin.rs#revoke_service_token]]).

### Registry

[[src/auth/issued.rs#IssuedTokenStore]] records each issued token's `jti`, label, org, role and expiry in `issued_tokens.json` next to the catalog, never the token, so the file cannot be replayed. Expired entries are dropped on the next write.
