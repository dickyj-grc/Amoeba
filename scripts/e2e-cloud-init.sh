#!/bin/bash
set -euo pipefail

# Cloud-init user-data for the Amoeba nightly e2e droplet.
# Installs Docker, Docker Compose, Caddy, and starts Amoeba with a few test apps.

export DEBIAN_FRONTEND=noninteractive

# --- Docker ---
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc
echo \
  "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu \
  $(. /etc/os-release && echo "$VERSION_CODENAME") stable" > /etc/apt/sources.list.d/docker.list
apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

# --- Clone Amoeba ---
AMOEBA_DIR=/opt/amoeba
git clone https://github.com/dickyj-grc/Amoeba.git "$AMOEBA_DIR" || true
cd "$AMOEBA_DIR"

# Inject secrets from environment (set by the orchestrator via write_files)
JWT_SECRET="${AMOEBA_LOCAL_JWT_SECRET:-change-me-in-production}"
AGE_SECRET="${AMOEBA_AGE_SECRET_KEY:-}"

mkdir -p /etc/amoeba/secrets
chmod 700 /etc/amoeba/secrets

# Place config files in the directory mounted into the Amoeba container
mkdir -p /opt/amoeba/config
cp config/config.example.toml /opt/amoeba/config/config.toml
cp config/users.example.json /opt/amoeba/config/users.json

# Start with an empty catalog; the e2e script installs apps via the admin API.
cat > /opt/amoeba/config/services.json <<'EOF'
{
  "version": 1,
  "machines": {
    "local": { "type": "vm", "drivers": ["docker"], "resources": { "memory": "8Gi" } }
  },
  "services": {}
}
EOF

# Use a simple HTTP Caddyfile for the e2e test (no TLS, no domain)
cat > "$AMOEBA_DIR/Caddyfile" <<'EOF'
{
    auto_https off
}
:80 {
    reverse_proxy orchestrator:8080
}
EOF

# Start Amoeba (Caddy + orchestrator) via Docker Compose, pulling the orchestrator
# image built by .github/workflows/docker-release.yml instead of compiling from
# source on every droplet (docker-compose.e2e.yml swaps `build: .` for `image:`).
cd "$AMOEBA_DIR"

# Persist secrets to .env so `docker compose` can interpolate them on every
# invocation (up, exec, logs, ...), not just the one command they're inlined
# into below. The e2e test suite runs `docker compose exec` over its own
# fresh SSH connections, which don't inherit this script's shell env.
cat > "$AMOEBA_DIR/.env" <<EOF
AMOEBA_LOCAL_JWT_SECRET=$JWT_SECRET
AMOEBA_AGE_SECRET_KEY=$AGE_SECRET
EOF
chmod 600 "$AMOEBA_DIR/.env"

docker compose -f docker-compose.yml -f docker-compose.e2e.yml pull
docker compose -f docker-compose.yml -f docker-compose.e2e.yml up -d

# Wait for Amoeba to be ready (Caddy proxies port 80 to the orchestrator)
for i in $(seq 1 60); do
    if curl -sS http://localhost/v1/health-check/ -o /dev/null 2>&1; then
        echo "Amoeba is ready"
        break
    fi
    sleep 5
done
