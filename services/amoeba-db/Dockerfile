# --- Build ---
FROM rust:alpine AS builder
WORKDIR /usr/src/amoeba-db
RUN apk add --no-cache musl-dev build-base
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin amoeba-db

# --- Runtime ---
FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 10001 amoeba
COPY --from=builder /usr/src/amoeba-db/target/release/amoeba-db /usr/local/bin/amoeba-db
USER amoeba
EXPOSE 8080
CMD ["amoeba-db"]
