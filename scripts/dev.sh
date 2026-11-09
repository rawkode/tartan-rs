#!/usr/bin/env bash
# Runs the forge locally: RustFS on :9100, walgit on :9200, a source CUE registry on :9300, the
# Rivet engine on :6420 and the Tartan gateway, with its own CUE registry, on :8080.
#
# The extension modules (tartan.rawkode.academy/types, Issues and Kanban) are built and published to the
# source registry, and the forge's deployment configuration (.dev/forge.cue) lists them, so the
# forge copies them into its registry at startup. A module version already in the source
# registry is not published again: bump its version, or delete .dev, to pick up changes.
#
# Needs `rustfs`, `rclone`, `jq` and `wasm-tools` on PATH (or WASM_TOOLS), WALGIT_BIN pointing at
# a walgit binary, REGISTRY_BIN at a CNCF distribution `registry` binary, and CUE_BIN at cue
# (default: cue on PATH). Development credentials only; nothing here is a secret.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state="$root/.dev"
mkdir -p "$state/rustfs" "$state/logs" "$state/registry"
: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary (github.com/tobi/walgit)}"
: "${REGISTRY_BIN:?set REGISTRY_BIN to a CNCF distribution registry binary}"
cue="${CUE_BIN:-cue}"
source_registry="127.0.0.1:9300"

export TARTAN_S3_ACCESS_KEY="tartandev" TARTAN_S3_SECRET_KEY="tartandevsecret"
export TARTAN_S3_ENDPOINT="http://127.0.0.1:9100" TARTAN_S3_BUCKET="tartan-dev"
export TARTAN_ADMIN_TOKEN="dev-admin-token" TARTAN_WEBHOOK_SECRET="dev-webhook-secret"
export TARTAN_GIT_HOST_TOKEN="dev-git-host-token" TARTAN_GIT_HOST_URL="http://127.0.0.1:9200"
export WALGIT__EVENTS__WEBHOOK_SECRET="$TARTAN_WEBHOOK_SECRET"
# walgit runs git in its cache, so the cache's path must not depend on the working directory.
export WALGIT__CACHE__DIR="$state/walgit-cache"
export RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"
# The forge resolves every configuration's imports through its own registry, its gateway.
export CUE_REGISTRY="127.0.0.1:8080+insecure"
export TARTAN_CONFIG="$state/forge.cue"

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

cat >"$state/registry.yml" <<YAML
version: 0.1
log:
  level: warn
storage:
  filesystem:
    rootdirectory: $state/registry
http:
  addr: $source_registry
YAML
"$REGISTRY_BIN" serve "$state/registry.yml" >"$state/logs/registry.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do curl -fsS -o /dev/null "http://$source_registry/v2/" 2>/dev/null && break; sleep 0.2; done

# Publishes the module in directory $1 as version $2, unless the source registry has it.
publish() {
	local dir="$1" version="$2" path
	path="$(sed -n 's/^module: *"\([^"@]*\).*/\1/p' "$dir/cue.mod/module.cue")"
	if curl -fsS -o /dev/null -H 'Accept: application/vnd.oci.image.manifest.v1+json' \
		"http://$source_registry/v2/$path/manifests/$version" 2>/dev/null; then
		echo "$path@$version is already published" >&2
	else
		(cd "$dir" && CUE_REGISTRY="$source_registry+insecure" "$cue" mod publish "$version" >&2)
	fi
	echo "$path@$version"
}

modules=()
modules+=("$(publish "$root/cue/types" v0.2.0)")
for extension in issues kanban; do
	dir="$("$root/scripts/build-extension.sh" "$root/extensions/$extension" "$state/modules/$extension")"
	modules+=("$(publish "$dir" "v$(jq -r .version "$dir/tartan.json")")")
done

{
	echo "package forge"
	echo
	echo "sources: {"
	echo "	\"tartan.rawkode.academy\": \"$source_registry\""
	echo "}"
	echo "modules: ["
	for module in "${modules[@]}"; do echo "	\"$module\","; done
	echo "]"
} >"$TARTAN_CONFIG"

cd "$root"
cargo run -p tartan
