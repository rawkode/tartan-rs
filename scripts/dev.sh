#!/usr/bin/env bash
# Runs the forge locally: RustFS on :9100, walgit on :9200, a source CUE registry on :9300,
# tartan-devnet (a local ATProto network) on :9400, the Rivet engine on :6420 and the Tartan
# gateway, with its own CUE registry, on :8080.
#
# The forge is built with the `dev-identity` feature and runs with TARTAN_DEV_IDENTITY=1, so
# identity (DIDs, handles, sign-in and signature records) comes from tartan-devnet and nothing
# reaches the public network. Its only owner is the devnet's @alice.test, and `readers` keeps its
# private default, so signing in as anyone else shows the private-forge page.
#
# This script does not start the web interface. TARTAN_PUBLIC_URL is http://127.0.0.1:4321, so
# run the Astro dev server on exactly that port (`npm --prefix web run dev`); sign-in works only
# through it. scripts/demo.sh starts everything, the web interface included.
#
# The extension modules (tartan.rawkode.academy/types, Issues and Kanban) are built and published
# to the source registry, and the forge's deployment configuration (.dev/forge.cue) lists them,
# so the forge copies them into its registry at startup. A module version already in the source
# registry is not published again: bump its version, or delete .dev, to pick up changes.
#
# Needs `rustfs`, `rclone`, `jq`, `curl` and `wasm-tools` on PATH (or WASM_TOOLS), WALGIT_BIN
# pointing at a walgit binary, REGISTRY_BIN at a CNCF distribution `registry` binary, and CUE_BIN
# at cue (default: cue on PATH). Development credentials only; nothing here is a secret.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state="$root/.dev"
mkdir -p "$state/rustfs" "$state/logs" "$state/registry"
: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary (github.com/tobi/walgit)}"
: "${REGISTRY_BIN:?set REGISTRY_BIN to a CNCF distribution registry binary}"
cue="${CUE_BIN:-cue}"
source_registry="127.0.0.1:9300"
devnet="http://127.0.0.1:9400"
gateway="http://127.0.0.1:8080"
web_port=4321

port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
for port in 9100 9200 9300 9400 6420 8080; do
	if port_in_use "$port"; then
		echo "127.0.0.1:$port is already in use; stop what runs there (another scripts/dev.sh, a demo or a Rivet engine) first" >&2
		exit 1
	fi
done

export TARTAN_S3_ACCESS_KEY="tartandev" TARTAN_S3_SECRET_KEY="tartandevsecret"
export TARTAN_S3_ENDPOINT="http://127.0.0.1:9100" TARTAN_S3_BUCKET="tartan-dev"
export TARTAN_WEBHOOK_SECRET="dev-webhook-secret"
export TARTAN_GIT_HOST_TOKEN="dev-git-host-token" TARTAN_GIT_HOST_URL="http://127.0.0.1:9200"
export WALGIT__EVENTS__WEBHOOK_SECRET="$TARTAN_WEBHOOK_SECRET"
# walgit runs git in its cache, so the cache's path must not depend on the working directory.
export WALGIT__CACHE__DIR="$state/walgit-cache"
RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"
export RIVET_ENGINE_BINARY_PATH
# The forge resolves every configuration's imports through its own registry, its gateway.
export CUE_REGISTRY="127.0.0.1:8080+insecure"
export TARTAN_CONFIG="$state/forge.cue"
export TARTAN_STATE_DIR="$state/tartan"
# Identity from tartan-devnet only (design D12): no plc.directory, DNS or public Jetstream.
export TARTAN_DEV_IDENTITY=1
export TARTAN_PLC_URL="$devnet" TARTAN_DEV_HANDLE_RESOLVER="$devnet"
export TARTAN_JETSTREAM_URL="ws://127.0.0.1:9400/subscribe"
# The browser-facing origin is the Astro dev server, which forwards /-/ and /v2/ here and adds
# X-Forwarded-For.
export TARTAN_PUBLIC_URL="http://127.0.0.1:$web_port"
export TARTAN_TRUSTED_PROXIES="127.0.0.1/32"
export TARTAN_OAUTH_KEYS_FILE="$state/oauth-keys.json"

pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

(cd "$root" && cargo build --quiet -p tartan -p tt -p tartan-devnet \
	--features tartan/dev-identity,tt/dev-identity)
tt="$root/target/debug/tt"
devnet_bin="$root/target/debug/tartan-devnet"

"$devnet_bin" --listen 127.0.0.1:9400 --seed dev >"$state/logs/devnet.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do port_in_use 9400 && break; sleep 0.2; done
owner="$("$devnet_bin" did alice.test)"

# The web client's signing key, kept with the development state and readable only by its owner.
if [[ ! -s "$TARTAN_OAUTH_KEYS_FILE" ]]; then
	(umask 077 && "$tt" admin oauth keygen >"$TARTAN_OAUTH_KEYS_FILE")
fi
chmod 600 "$TARTAN_OAUTH_KEYS_FILE"

RUSTFS_ACCESS_KEY="$TARTAN_S3_ACCESS_KEY" RUSTFS_SECRET_KEY="$TARTAN_S3_SECRET_KEY" \
	rustfs server --address 127.0.0.1:9100 "$state/rustfs" >"$state/logs/rustfs.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do curl -fsS -o /dev/null http://127.0.0.1:9100/health 2>/dev/null && break; sleep 0.2; done
RCLONE_CONFIG_DEV_TYPE=s3 RCLONE_CONFIG_DEV_PROVIDER=Other \
	RCLONE_CONFIG_DEV_ENDPOINT=http://127.0.0.1:9100 \
	RCLONE_CONFIG_DEV_ACCESS_KEY_ID="$TARTAN_S3_ACCESS_KEY" \
	RCLONE_CONFIG_DEV_SECRET_ACCESS_KEY="$TARTAN_S3_SECRET_KEY" \
	rclone mkdir dev:tartan-dev

(cd "$root" && exec "$WALGIT_BIN" --config dev/walgit.toml >"$state/logs/walgit.log" 2>&1) &
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
	echo "// The devnet's @alice.test. readers keeps its default: only owners read."
	echo "owners: [\"$owner\"]"
	echo
	echo "sources: {"
	echo "	\"tartan.rawkode.academy\": \"$source_registry\""
	echo "}"
	echo "modules: ["
	for module in "${modules[@]}"; do echo "	\"$module\","; done
	echo "]"
} >"$TARTAN_CONFIG"

"$root/target/debug/tartan" &
pids+=($!)

echo "Sign in as @alice.test (the owner) at $TARTAN_PUBLIC_URL once the web dev server runs there." >&2

# An owner token for scripts and curl, minted through Rivet as an operator would. The first
# attempts can find the engine or the identity actor still starting.
token=""
for _ in $(seq 120); do
	if token="$("$tt" admin token create --did "$owner" --name dev.sh --scopes read,write,admin \
		--expires-in-days 30 2>/dev/null | grep '^tartan_pat_')"; then
		break
	fi
	sleep 1
done
[[ -n "$token" ]] || { echo "could not mint a token through the identity actor" >&2; exit 1; }
for _ in $(seq 120); do
	curl -fsS -o /dev/null -H "authorization: Bearer $token" "$gateway/-/api/me" 2>/dev/null && break
	sleep 0.5
done
(umask 077 && echo "$token" >"$state/token")

cat <<EOF

Tartan is running, with development identity from tartan-devnet ($devnet).

  Web interface   run \`npm --prefix web run dev\` and open $TARTAN_PUBLIC_URL
                  (the dev server must get port $web_port: TARTAN_PUBLIC_URL names that origin)
  Sign in as      @alice.test, the owner; other devnet people see a private forge
  Gateway API     $gateway/-/api   (Authorization: Bearer \$(cat $state/token))
  Logs            $state/logs

Press Ctrl-C to stop everything.
EOF
wait
