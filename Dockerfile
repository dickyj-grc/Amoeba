# --- Stage 1: Web console ---
FROM node:22-alpine AS web

WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web ./
RUN npm run build

# --- Stage 2: Build Stage ---
FROM rust:alpine AS builder

WORKDIR /usr/src/amoeba
RUN apk add --no-cache musl-dev build-base

COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The app library is compiled into the gateway.
COPY library ./library
COPY examples/app-packages ./examples/app-packages

# Build release binaries (orchestrator + admin CLI) optimized for size and speed
RUN cargo build --release --bin amoeba --bin amoeba-admin

# --- Stage 3: Minimal Runtime Stage ---
FROM alpine:3.20

WORKDIR /app

# ca-certificates for outbound HTTPS (e.g. fetching JWKS); tzdata for correct log timestamps.
# docker-cli + docker-cli-compose: the orchestrator shells out to `docker`/`docker compose`
# (see src/apps/manager.rs, src/lifecycle/compose.rs) against the host's Docker socket
# mounted in at /var/run/docker.sock -- it talks to bollard directly for plain `container`
# lifecycle management, but image pulls and `stack_spec`/compose-type apps go through the CLI.
RUN apk add --no-cache ca-certificates tzdata docker-cli docker-cli-compose

COPY --from=builder /usr/src/amoeba/target/release/amoeba /usr/local/bin/amoeba
COPY --from=builder /usr/src/amoeba/target/release/amoeba-admin /usr/local/bin/amoeba-admin
# Served at / by the gateway (AMOEBA_CONSOLE_DIR), next to the API.
COPY --from=web /web/dist /app/console

EXPOSE 8080

CMD ["amoeba"]
