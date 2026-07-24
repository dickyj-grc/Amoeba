# --- Stage 1: Build Stage ---
FROM rust:alpine AS builder

WORKDIR /usr/src/amoeba
RUN apk add --no-cache musl-dev build-base

COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build release binaries (orchestrator + admin CLI) optimized for size and speed
RUN cargo build --release --bin amoeba --bin amoeba-admin

# --- Stage 2: Minimal Runtime Stage ---
FROM alpine:3.20

WORKDIR /app

# ca-certificates for outbound HTTPS (e.g. fetching JWKS); tzdata for correct log timestamps
RUN apk add --no-cache ca-certificates tzdata

COPY --from=builder /usr/src/amoeba/target/release/amoeba /usr/local/bin/amoeba
COPY --from=builder /usr/src/amoeba/target/release/amoeba-admin /usr/local/bin/amoeba-admin

EXPOSE 8080

CMD ["amoeba"]
