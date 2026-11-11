# Dev

`scripts/dev.sh` runs Amoeba locally in one command: the API on `localhost:8080` and the web console on `localhost:3000`.

It creates `.env` with a random JWT secret, an empty service catalog and an `admin` user, and prints the admin password. `--down` stops everything; `--help` lists the other flags.

## Mode selection

The script picks native mode on macOS when Apple's `container` CLI and `cargo` are installed, and docker mode everywhere else. `--native` and `--docker` override it.

Each mode keeps its own state so their catalogs never mix: native in `.dev/native/`, docker in `config/` (mounted at `/etc/amoeba`).

## Native mode on macOS

Amoeba runs as a native binary and starts apps as Apple `container` micro-VMs, so no Docker Desktop VM or x86 emulation is involved.

The script builds `amoeba` and `amoeba-admin` in release mode and writes a catalog whose only machine uses the `apple-container` driver. It then starts the server with `AMOEBA_CONFIG`, `AMOEBA_SERVICES_FILE` and `AMOEBA_SECRETS_DIR` pointing into `.dev/native/`. `AMOEBA_SERVICES_FILE` overrides the default `/etc/amoeba/services.json`.

Compose apps (`app.type: compose`, such as the Obscura and hello-world examples) still need Docker, so use docker mode for those.

### Apple container installs

An image app installed on an `apple-container` machine must leave a catalog the loader accepts, and its readiness probe must use the driver's resolved IP.

Apple containers get a new IP on every start, so the loader rejects `placement.ip` for them. [[src/apps/manager.rs#AppManager#install]] drops it, and validates the whole catalog with [[src/config/watcher.rs#validate_catalog]] before writing so a bad entry fails the install instead of being silently skipped on reload. [[src/apps/manager.rs#spawn_readiness_probe]] pulls with `container image pull` and probes the host returned by `ensure_started`. The Apple driver passes plain `env` as well as secrets, like the Docker driver.

## Docker mode

The orchestrator runs in Docker Compose with `docker-compose.dev.yml`, which publishes only the orchestrator on `127.0.0.1:8080` (no Caddy).

By default it pulls `ghcr.io/dickyj-grc/amoeba:latest`. That image is `linux/amd64` only, so Apple Silicon runs it emulated; `--build` builds a native image from the checkout instead.
