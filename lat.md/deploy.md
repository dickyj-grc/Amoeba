# Deploy

How a service gets from the web console to the catalog: optionally build an image from source, then install it as an image app package.

Installing the package and handling its secrets is covered in [[secrets#Console deploy]].

## Source builds

The console's Git and Dockerfile tabs have no image yet, so Amoeba builds one on its own host and the deploy then runs that tag.

`POST /admin/apps/:name/build` (admin only) is [[src/routing/apps_admin.rs#build_app_image]]; the work is [[src/apps/build.rs#build_image]]. It returns `{image, log}`, or `422` with `{error, log}` when the clone or build fails. The log keeps the last 500 lines.

- `{"source":"git","url","ref"?}`: shallow-clone the repo and build with its root `Dockerfile`, or with nixpacks when there is none and nixpacks is installed ([[src/apps/build.rs#choose_builder]]).
- `{"source":"dockerfile","dockerfile","context_url"?,"context_ref"?}`: build the pasted Dockerfile. If `context_url` is set, the repo is cloned as the build context and the pasted file replaces its Dockerfile.

The tag comes from [[src/apps/deploy.rs#image_reference]]: `amoeba/<name>:latest`, or `<AMOEBA_REGISTRY>/<name>:latest` with a push when that variable is set. Each build runs in a fresh temp directory. Clone times out after 2 minutes, build after 15 ([[src/apps/build.rs#build_timeout]]: `AMOEBA_BUILD_TIMEOUT_SECS` overrides it, for large projects). The request blocks until the build finishes; [[deploy#Source builds#Live progress]] shows what it is doing meanwhile.

The MCP server's `deploy_service` tool has its own build code in `src/bin/amoeba-mcp.rs`; it does not call this endpoint.

### Live progress

A build can take many minutes, so its output is visible while it runs, and it can be stopped.

[[src/apps/build.rs#BuildProgress]] collects each command's output line by line as it arrives (BuildKit in plain mode), keeping the last 500 lines. The gateway keeps the latest build per app name in memory.

- `GET /admin/apps/<name>/build` ([[src/routing/apps_admin.rs#build_status]]): `{state, step, elapsed_seconds, log}`, where `state` is `running`, `succeeded`, `failed` or `cancelled`, and `log` is the newest 200 lines. 404 when there has been no build since the gateway started.
- `DELETE /admin/apps/<name>/build` ([[src/routing/apps_admin.rs#cancel_build]]): stops the running build. Each command runs in its own process group and the whole group is killed, so `docker` helpers stop too. The pending build request answers 422 `cancelled`.
- A second build for the same name while one runs is 409.

The console's deploy dialog polls the status every 2 seconds ([[web/src/lib/deployPackage.ts#fetchBuildStatus]]) while its build request is pending, showing elapsed time, the current command, the log tail and a "Stop build" button.

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

## Console refresh

The service list refreshes every 3 seconds without visible churn: background refreshes do not show a loading state, and an unchanged result keeps the old list so nothing re-renders.

The list is sorted by name on the server ([[src/apps/manager.rs#AppManager#list_installed]]) and in the console. The catalog is a map, so before sorting the rows changed order on every refresh. [[web/src/lib/services.ts#sameServices]] decides whether a refresh changed anything. Refreshes pause while the browser tab is hidden.

## App library

Ready-made app packages for known images, so a user deploys one without knowing its port, start command or API. The console's first deploy tab searches it.

`library/index.json` lists each app's title, summary, tags, the image names and GitHub repos that find it, and its package folder. [[src/apps/library.rs]] compiles the index and every package's files into the gateway, so it works offline and ships with each release; the Dockerfile copies `library/` and `examples/app-packages/` for that.

- `GET /admin/library` ([[src/routing/apps_admin.rs#list_library]]) lists entries with what the console needs: image, command, port, interface, and the env and secret inputs from the manifest's `schema`.
- `GET /admin/library/<id>` returns one entry.
- `POST /admin/library/<id>/install` ([[src/routing/apps_admin.rs#install_from_library]]) takes `{name?, public?, env, secrets}`.

### Install from the library

The install builds the package zip from the bundled files and installs it exactly like an upload, through the same readiness probe.

[[src/apps/library.rs#package_zip]] applies the installer's name and `public` choice to the manifest. A rename also renames the console token role `mcp:<old>` in its permissions, so tokens issued for the new name work. Names follow the image-name rule (lowercase), as in the deploy form.

### Start command

`app.command` in a manifest replaces the image's `CMD` (the entrypoint stays), so a published image can run in another mode without a build, e.g. Lightpanda's MCP server instead of its CDP server.

`${PORT}` in it becomes `placement.port` ([[src/apps/schema.rs#expand_command]]), so the port is written once; any other `${...}` fails the install. Image apps also get `PORT` in their env, so their `schema` must not declare it.

It is stored, expanded, as `container.command` in the catalog. The Docker driver sets it as the container's `Cmd`; the Apple driver appends it after the image in `container run` ([[src/lifecycle/apple_container.rs#AppleContainerDriver#run_args]]).

### Changed specs recreate the container

Reinstalling an app (e.g. Lightpanda from the Docker Image tab, then from the library) must run the new spec, not restart the old container under the same name.

Both drivers label each container `amoeba.spec` with a fingerprint ([[src/lifecycle/driver.rs#spec_fingerprint]]) of its image, command, env (secrets included), memory, CPU, runtime and network. On start, a container whose label differs or is missing is deleted and recreated. Anything kept only in the container's own filesystem is lost then, as it is on any reinstall.

### Declared settings

`schema.env` lists every setting an installer may set; install refuses undeclared keys, a missing `required` value without a default, and values that miss their `type` or `options`.

Types are `string`, `number`, `integer` and `boolean` ([[src/apps/schema.rs#EnvField#check]]). An empty value counts as not given, so the default applies ([[src/apps/manager.rs#resolved_env]]); compose apps get the same values in their `.env`. Manifests are checked too: unknown types, empty `options` and defaults that fail their own rules are refused. The library tab shows `options` (and booleans) as a dropdown and marks required settings.

### Bundled packages

A test keeps the library honest: every index entry is bundled, its manifest validates, declares an `interface`, lists its image, and each bundled file equals the file on disk.

Every folder under `library/` must be in the index. Example packages are reused from `examples/app-packages/` rather than copied.

### Manifest reference

`docs/manifest.md` documents every `amoeba.yaml` field for people and AI tools, with `docs/amoeba.schema.json` as its machine-readable form, linked from the README and listed in `llms.txt`.

A test walks every bundled manifest against the schema and fails when a manifest uses a key the schema lacks, so new fields cannot ship undocumented. `docs/index.md` makes `docs/` ready to serve with GitHub Pages.

### Console library tab

The deploy dialog opens on "App library": search by name, tag, image (any tag or digest) or GitHub URL, pick an app, and fill only its declared inputs.

[[web/src/lib/library.ts#searchLibrary]] ranks exact image or repo matches first; [[web/src/lib/library.ts#normalizeQuery]] strips schemes, `.git`, tags, digests and `docker.io/`. When an image or repo typed in the Docker Image or Git tab is a library app ([[web/src/lib/library.ts#libraryMatch]]), the tab offers its library setup. After deploy, the service's `interface` shows in the table, starts the Test dialog with its sample request, and sets the MCP path in the token dialog.
