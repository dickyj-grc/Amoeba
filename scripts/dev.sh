#!/usr/bin/env bash
# Run Amoeba locally in one step: API on http://localhost:8064 and, unless
# --no-console is given, the web console on http://localhost:3064.
#
# Picks a mode automatically:
#   native  macOS with Apple's `container` CLI and cargo: Amoeba runs as a
#           native binary and starts apps as Apple container micro-VMs.
#           No Docker Desktop needed. State lives in .dev/native/.
#   docker  everywhere else: the orchestrator runs in Docker Compose and
#           starts apps as Docker containers. State lives in config/.
#
#   scripts/dev.sh               start (auto mode)
#   scripts/dev.sh --docker      force docker mode (--native forces native)
#   scripts/dev.sh --build       docker mode: build the image from this checkout
#   scripts/dev.sh --no-console  start only the API
#   scripts/dev.sh --reset-admin give the admin user a new random password
#   scripts/dev.sh --down        stop everything started by this script
#
# First run creates .env (random JWT secret), an empty service catalog and an
# "admin" user, and prints the admin password (kept in <state>/.dev-admin-password).
set -euo pipefail

cd "$(dirname "$0")/.."
root=$(pwd)

mode=""
build=false
console=true
reset_admin=false
down=false
for arg in "$@"; do
  case "$arg" in
    --native) mode=native ;;
    --docker) mode=docker ;;
    --build) build=true ;;
    --no-console) console=false ;;
    --reset-admin) reset_admin=true ;;
    --down) down=true ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

native_dir="$root/.dev/native"
pid_file="$native_dir/amoeba.pid"
compose=(docker compose -f docker-compose.yml -f docker-compose.dev.yml)
admin_args=()

native_running() { [ -f "$pid_file" ] && kill -0 "$(cat "$pid_file")" 2>/dev/null; }

if $down; then
  if native_running; then
    kill "$(cat "$pid_file")" && rm -f "$pid_file"
    # Stop the app containers it started; leave unrelated containers alone.
    if [ -f "$native_dir/services.json" ]; then
      for name in $(python3 -c 'import json,sys; print(" ".join(json.load(open(sys.argv[1]))["services"]))' "$native_dir/services.json"); do
        container stop "$name" >/dev/null 2>&1 || true
      done
    fi
    echo "Stopped native Amoeba."
  fi
  if command -v docker >/dev/null && docker info >/dev/null 2>&1 \
     && [ -n "$("${compose[@]}" ps -q 2>/dev/null)" ]; then
    "${compose[@]}" down
  fi
  exit 0
fi

if [ -z "$mode" ]; then
  if [ "$(uname -s)" = Darwin ] && command -v container >/dev/null && command -v cargo >/dev/null; then
    mode=native
  else
    mode=docker
  fi
fi
echo "Mode: $mode"

if [ ! -f .env ]; then
  echo "AMOEBA_LOCAL_JWT_SECRET=$(openssl rand -hex 32)" > .env
  chmod 600 .env
  echo "Created .env with a random JWT secret."
fi

# Writes an empty catalog whose only machine uses the given driver.
write_catalog() {
  [ -f "$1" ] && return
  cat > "$1" <<JSON
{
  "version": 1,
  "machines": {
    "local": { "type": "vm", "drivers": ["$2"], "resources": { "memory": "8Gi" } }
  },
  "services": {}
}
JSON
  echo "Created an empty $1."
}

# Creates (or with --reset-admin, resets) the admin user. $1 is the state dir;
# the rest is the amoeba-admin command to run. admin_args are appended after
# the subcommand's own arguments (amoeba-admin wants flags last).
ensure_admin() {
  local dir=$1; shift
  local users="$dir/users.json" pw_file="$dir/.dev-admin-password"
  local exists=false
  [ -f "$users" ] && grep -q '"admin"' "$users" && exists=true
  if ! $exists || $reset_admin; then
    local pw
    pw=$(openssl rand -base64 18 | tr -d '/+=' | cut -c1-20)
    if $exists; then
      "$@" update-user admin --password "$pw" ${admin_args[@]+"${admin_args[@]}"} >/dev/null
    else
      "$@" add-user admin "$pw" --roles admin --org default ${admin_args[@]+"${admin_args[@]}"} >/dev/null
    fi
    printf '%s\n' "$pw" > "$pw_file"
    chmod 600 "$pw_file"
  fi
  if [ -f "$pw_file" ]; then
    admin_password=$(cat "$pw_file")
  else
    admin_password="(existing admin user kept; run with --reset-admin to set a new one)"
  fi
}

if [ "$mode" = native ]; then
  native_running && { echo "Already running (pid $(cat "$pid_file")); use --down first." >&2; exit 1; }
  container system status >/dev/null 2>&1 || container system start

  cargo build --release --bin amoeba --bin amoeba-admin
  mkdir -p "$native_dir/secrets"
  chmod 700 "$native_dir" "$native_dir/secrets"
  write_catalog "$native_dir/services.json" apple-container
  cat > "$native_dir/config.toml" <<TOML
[server]
bind_addr = "127.0.0.1:8064"

[auth]
mode = "local_jwt"
jwt_secret = "env:AMOEBA_LOCAL_JWT_SECRET"
users_file = "$native_dir/users.json"
revocation_file = "$native_dir/revoked_tokens.json"
TOML
  admin_args=(--users-file "$native_dir/users.json")
  ensure_admin "$native_dir" target/release/amoeba-admin

  set -a; . ./.env; set +a
  AMOEBA_CONFIG="$native_dir/config.toml" \
  AMOEBA_SERVICES_FILE="$native_dir/services.json" \
  AMOEBA_SECRETS_DIR="$native_dir/secrets" \
  RUST_LOG="${RUST_LOG:-info}" \
    nohup target/release/amoeba >"$native_dir/amoeba.log" 2>&1 &
  echo $! > "$pid_file"
  log_hint="tail -f .dev/native/amoeba.log"
else
  command -v docker >/dev/null || { echo "Docker is required: https://docs.docker.com/get-docker/" >&2; exit 1; }
  docker info >/dev/null 2>&1 || { echo "Docker is not running; start Docker Desktop and retry." >&2; exit 1; }
  if ! $build; then
    # docker-compose.e2e.yml swaps `build: .` for the published image, which
    # is linux/amd64 only; Apple Silicon runs it emulated. Use --build for a
    # native image.
    compose+=(-f docker-compose.e2e.yml)
    export DOCKER_DEFAULT_PLATFORM=linux/amd64
  fi

  write_catalog config/services.json docker
  if $build; then
    "${compose[@]}" build orchestrator
  else
    "${compose[@]}" pull orchestrator
  fi
  # amoeba-admin writes /etc/amoeba/users.json, which is config/users.json.
  ensure_admin config "${compose[@]}" run --rm --no-deps orchestrator amoeba-admin
  "${compose[@]}" up -d orchestrator
  log_hint="docker logs -f amoeba-orchestrator"
fi

printf 'Waiting for the API'
ready=false
for _ in $(seq 1 60); do
  # Any HTTP response means the server is listening.
  if curl -s -o /dev/null http://127.0.0.1:8064/auth/login; then ready=true; break; fi
  printf '.'
  sleep 1
done
if ! $ready; then
  echo " not up after 60s. Logs: $log_hint" >&2
  exit 1
fi
echo " ready."

echo
echo "  API:      http://localhost:8064"
$console && echo "  Console:  http://localhost:3064"
echo "  User:     admin"
echo "  Password: $admin_password"
echo "  Logs:     $log_hint"
echo "  Stop:     scripts/dev.sh --down"
echo

if $console; then
  command -v npm >/dev/null || { echo "npm not found; install Node.js 22+ for the console, or use --no-console." >&2; exit 1; }
  cd web
  [ -d node_modules ] || npm ci
  exec npm run dev
fi
