# E2E

The nightly e2e provisions a DigitalOcean droplet, starts Amoeba there with Docker Compose, and runs `scripts/e2e/` against it over HTTP.

## What runs on the droplet

The droplet must run the same commit as the tests, or a branch run checks new tests against an old server.

`scripts/e2e-cloud-init.sh` clones the repo, checks out `AMOEBA_E2E_REF`, and runs the orchestrator image named by `AMOEBA_E2E_IMAGE` through `docker-compose.e2e.yml`. Both are substituted by `scripts/e2e/conftest.py`; the defaults are `main` and `ghcr.io/dickyj-grc/amoeba:latest`.

The scheduled run on `main` uses `:latest` from `docker-release.yml`. A manual run on another branch (`gh workflow run nightly-e2e.yml --ref <branch>`) first builds and pushes `ghcr.io/dickyj-grc/amoeba:sha-<commit>` and runs that.
