#!/usr/bin/env bash
# Runs the forge locally: RustFS on :9100, the Rivet engine on :6420, walgit on :9200 and the
# Tartan gateway on :8080. Needs `rustfs` and `rclone` on PATH and WALGIT_BIN pointing at a
# walgit binary. Development credentials only; nothing here is a secret.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state="$root/.dev"
mkdir -p "$state/rustfs" "$state/logs"
: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary (github.com/tobi/walgit)}"

export TARTAN_S3_ACCESS_KEY="tartandev" TARTAN_S3_SECRET_KEY="tartandevsecret"
export TARTAN_ADMIN_TOKEN="dev-admin-token" TARTAN_WEBHOOK_SECRET="dev-webhook-secret"
export TARTAN_GIT_HOST_TOKEN="dev-git-host-token" TARTAN_GIT_HOST_URL="http://127.0.0.1:9200"
export WALGIT__EVENTS__WEBHOOK_SECRET="$TARTAN_WEBHOOK_SECRET"
export RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"

pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

RUSTFS_ACCESS_KEY="$TARTAN_S3_ACCESS_KEY" RUSTFS_SECRET_KEY="$TARTAN_S3_SECRET_KEY" \
	rustfs server --address 127.0.0.1:9100 "$state/rustfs" >"$state/logs/rustfs.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do curl -fsS -o /dev/null http://127.0.0.1:9100/health 2>/dev/null && break; sleep 0.2; done
RCLONE_CONFIG_DEV_TYPE=s3 RCLONE_CONFIG_DEV_PROVIDER=Other \
	RCLONE_CONFIG_DEV_ENDPOINT=http://127.0.0.1:9100 \
	RCLONE_CONFIG_DEV_ACCESS_KEY_ID="$TARTAN_S3_ACCESS_KEY" \
	RCLONE_CONFIG_DEV_SECRET_ACCESS_KEY="$TARTAN_S3_SECRET_KEY" \
	rclone mkdir dev:tartan-dev

(cd "$root" && "$WALGIT_BIN" --config dev/walgit.toml >"$state/logs/walgit.log" 2>&1) &
pids+=($!)

cd "$root"
cargo run -p tartan
