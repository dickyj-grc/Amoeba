# Secrets

How an app's secrets get from the installer to the running service without being stored in its config or package.

Install-time handling lives in [[src/apps/manager.rs#AppManager#install]]. Request-time injection is covered in [[gateway#Upstream credentials]].

## Declaring secrets

A secret exists only if the app manifest declares it under `schema.secrets`. Each entry says whether it is required and how it is delivered (`inject`).

`inject: env` (default) puts the value in the container environment at start through `env_from_secret`. `inject: proxy` keeps it on the gateway and sends it as `X-Amoeba-Secret-<key>` on every request through `header_from_secret`. Mapping is done by [[src/apps/manager.rs#build_service_config]].

The value comes from the `values.secrets` field of the install request, or from an age-encrypted file in the package (`file`) decrypted with `AMOEBA_AGE_SECRET_KEY`. A supplied value wins over the file.

## Gateway credential from a secret

An app that requires auth on every request gets it from one declared secret, used twice: the container receives it, and the gateway sends it upstream.

The manifest's `upstream_auth: {type, secret, header_name?}` names a key in `schema.secrets`. Install checks the key is declared and the type is `bearer_static` or `custom_header` (which needs `header_name`). [[src/apps/manager.rs#build_service_config]] writes `upstream_auth.token_secret = "<app>/<key>"`, and the proxy reads that file per request. Callers still authenticate to Amoeba with their own JWT and never see the token.

`examples/app-packages/obscura` uses this: Obscura 0.2.3+ exits at start unless `OBSCURA_MCP_TOKEN` (32+ bytes) is set, then requires `Authorization: Bearer` on every MCP call. The secret is declared with `inject: env` and a `^.{32,}$` pattern, and compose passes it through with `environment: [OBSCURA_MCP_TOKEN]`.

## Storage

Each value is written to `<config dir>/secrets/<app>/<key>` with mode `0600`. The catalog stores only the `"<app>/<key>"` reference, never the value.

Uninstall removes the app's secrets directory. The driver reads the files when it starts the container; the proxy reads them on each request.

## Install-time validation

Install rejects a secret it cannot place safely, before anything is written to disk.

- A declared secret name must match `[A-Za-z0-9_][A-Za-z0-9_.-]*` ([[src/apps/manager.rs#is_safe_secret_key]]). The name becomes a file name, so separators or a leading dot could escape the app's directory.
- Every key in `values.secrets` must be declared. An undeclared value would be written under a caller-chosen name and never injected.
- Required secrets must be present, and a `validation_pattern` must match.

Checks run in [[src/apps/manager.rs#validate_manifest]] and [[src/apps/manager.rs#validate_values]].

## Console deploy

The web console's deploy form builds a real app package so that rows marked as secret reach the service as `inject: env` secrets.

[[web/src/lib/deployPackage.ts#buildDeployForm]] writes a `.zip` containing `amoeba.yaml` (JSON, which is valid YAML) that declares every env var and secret row, and sends the values as the multipart `values` field. Secret values never appear in the package. Row names must be valid env var names.

`tests/fixtures/console-deploy.zip` is a package produced by that code; the Rust test installs it so a change on either side that breaks the contract fails. Regenerate it if the manifest shape changes.
