#!/usr/bin/env bash
# Runs a seeded Tartan forge and its web interface for a demo, all on this machine:
#
#   RustFS        127.0.0.1:9100   object store for Git, the registry and extension packages
#   walgit        127.0.0.1:9200   the Git host
#   registry      127.0.0.1:9300   a source CUE registry (CNCF distribution) holding the
#                                  tartan.rawkode.academy/types, Issues and Kanban modules
#   tartan-devnet 127.0.0.1:9400   a local ATProto network: PLC directory, PDS, authorization
#                                  server and Jetstream, with the people @alice.test,
#                                  @bob.test, @carol.test, @dave.test and @erin.test
#   Rivet engine  127.0.0.1:6420   started by the forge
#   gateway       127.0.0.1:8080   the forge's API and its own CUE registry
#   web           127.0.0.1:4321   the web interface (Astro dev server; the first port from 4321
#                                  that is free, chosen before the forge starts, because the
#                                  forge's TARTAN_PUBLIC_URL names it)
#
# By default everything is offline: the forge is a `dev-identity` build with
# TARTAN_DEV_IDENTITY=1, so people, sign-in and signature records come from tartan-devnet. Its
# owner is @alice.test and `readers` is "signed_in", so any devnet person can sign in and read
# (sign in as @bob.test to see read-only access). The devnet's authorization server approves at
# once, so signing in needs no password.
#
# The demo creates org acme and repository acme/api and pushes a configuration with git through
# walgit: a cue.mod/module.cue depending on the modules, and a tartan.cue installing Issues with
# typed properties, Kanban with two boards, and a signing rule that main and release/* must be
# signed by @alice.test. alice and bob are signed in to `tt` without a browser, each with their
# own `tt` state under the demo's state directory. The configuration commit is signed by alice;
# then a commit signed by bob goes to release/1.0, which the rule does not allow, so the
# repository shows a signing violation there. A dozen issues are filed, assigned to the devnet
# people by DID.
#
#   scripts/demo.sh          start from nothing: earlier demo state is deleted and reseeded
#   scripts/demo.sh --keep   reuse the previous demo's state (the OAuth key and the seeded
#                            forge; alice and bob sign in to tt again, since tartan-devnet keeps
#                            no state); seed only if seeding never finished
#
# With --owner, the forge runs against the real ATProto network instead, with that DID as its
# only owner, so you can sign in to the web interface as yourself:
#
#   scripts/demo.sh --owner did:web:example.com
#   scripts/demo.sh --owner did:web:example.com --client-metadata-url https://example.com/tartan-dev.json
#
# A forge at http://127.0.0.1 cannot be reached by your authorization server, so this mode uses
# a development sign-in client (design D1, *Development sign-in clients*): by default ATProto's
# `http://localhost` client (TARTAN_DEV_LOCALHOST_CLIENT=1), which servers following the reference
# implementation, such as bsky.social, accept. Servers that accept only https client ids refuse
# it; for those, host the document the forge serves at /-/auth/client-metadata.json at an https
# URL and pass that URL as --client-metadata-url. This mode seeds no signing rule and signs in
# nobody to `tt`. Its state lives in .dev/demo-owner.
#
# State lives in .dev/demo. Ctrl-C stops every process the script started. The demo never writes
# your Git configuration or your own `tt` configuration, credentials or state.
#
# Needs: cargo with the wasm32-unknown-unknown target, npm, jq, curl, git (2.19 or later for
# signing with tt), rclone and rustfs on PATH, wasm-tools (or WASM_TOOLS), CUE_BIN (or cue on
# PATH), REGISTRY_BIN (a distribution `registry` binary) and WALGIT_BIN (a walgit binary).
# Development credentials only; nothing here is a secret.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
usage() {
	echo "usage: $0 [--keep] [--owner <did> [--client-metadata-url <https url>]]" >&2
	exit 2
}
keep=false
owner=""
client_metadata_url=""
while (($#)); do
	case "$1" in
	--keep) keep=true ;;
	--owner)
		(($# >= 2)) || usage
		owner="$2"
		shift
		;;
	--client-metadata-url)
		(($# >= 2)) || usage
		client_metadata_url="$2"
		shift
		;;
	*) usage ;;
	esac
	shift
done
if [[ -n "$client_metadata_url" && -z "$owner" ]]; then
	echo "--client-metadata-url needs --owner: the offline demo signs in through tartan-devnet" >&2
	exit 2
fi
if [[ -n "$owner" && ! "$owner" =~ ^did:(plc|web): ]]; then
	echo "--owner takes a DID (did:plc:… or did:web:…), not ${owner@Q}" >&2
	exit 2
fi
if [[ -n "$client_metadata_url" && ! "$client_metadata_url" =~ ^https:// ]]; then
	echo "--client-metadata-url must be an https URL: authorization servers fetch it" >&2
	exit 2
fi
# The offline demo and the real-network demo keep separate state.
devnet_mode=true
state="$root/.dev/demo"
if [[ -n "$owner" ]]; then
	devnet_mode=false
	state="$root/.dev/demo-owner"
fi

: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary (github.com/tobi/walgit)}"
: "${REGISTRY_BIN:?set REGISTRY_BIN to a CNCF distribution registry binary}"
cue="${CUE_BIN:-cue}"
for tool in "$cue" cargo npm jq curl git rclone rustfs; do
	command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 1; }
done

port_in_use() {
	(exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null || (exec 3<>"/dev/tcp/::1/$1") 2>/dev/null
}
ports=(9100 9200 9300 6420 8080)
$devnet_mode && ports+=(9400)
for port in "${ports[@]}"; do
	if port_in_use "$port"; then
		echo "127.0.0.1:$port is already in use; stop what runs there (scripts/dev.sh, another demo, a devnet or a Rivet engine) first" >&2
		exit 1
	fi
done
# The web port is fixed before the forge starts: TARTAN_PUBLIC_URL names it, and the dev server
# refuses to move to another port (strictPort).
web_port=""
for port in $(seq 4321 4399); do
	if ! port_in_use "$port"; then
		web_port="$port"
		break
	fi
done
[[ -n "$web_port" ]] || { echo "no free port between 4321 and 4399 for the web interface" >&2; exit 1; }
web="http://127.0.0.1:$web_port"

if $devnet_mode; then
	# `gpg.x509.program` needs Git 2.19; Git shows a signing program's error messages from 2.40.
	git_version="$(git --version | sed -n 's/^git version \([0-9]*\)\.\([0-9]*\).*/\1 \2/p')"
	read -r git_major git_minor <<<"$git_version"
	if ((git_major < 2 || (git_major == 2 && git_minor < 19))); then
		echo "signing with tt needs Git 2.19 or later; this is $(git --version)" >&2
		exit 1
	fi
	if ((git_major == 2 && git_minor < 40)); then
		echo "warning: Git before 2.40 hides tt's error messages when signing fails" >&2
	fi
fi

# A fresh demo starts from nothing. Directories CUE extracted modules into are read only.
if ! $keep || [[ ! -f "$state/seeded" ]]; then
	if [[ -d "$state" ]]; then
		chmod -R u+w "$state"
		rm -rf "$state"
	fi
fi
mkdir -p "$state/logs" "$state/rustfs" "$state/registry"

git_token="dev-git-host-token"
gateway="http://127.0.0.1:8080"
source_registry="127.0.0.1:9300"
devnet="http://127.0.0.1:9400"
export TARTAN_S3_ACCESS_KEY="tartandev" TARTAN_S3_SECRET_KEY="tartandevsecret"
export TARTAN_S3_ENDPOINT="http://127.0.0.1:9100" TARTAN_S3_BUCKET="tartan-demo"
export TARTAN_WEBHOOK_SECRET="dev-webhook-secret"
export TARTAN_GIT_HOST_TOKEN="$git_token" TARTAN_GIT_HOST_URL="http://127.0.0.1:9200"
export TARTAN_LISTEN="127.0.0.1:8080"
# The forge resolves every configuration's imports through its own registry, its gateway.
export CUE_REGISTRY="127.0.0.1:8080+insecure"
export CUE_CACHE_DIR="$state/cue-cache"
export TARTAN_CONFIG="$state/forge.cue"
export TARTAN_STATE_DIR="$state/tartan"
# The browser-facing origin is the Astro dev server, which forwards /-/ and /v2/ to the gateway
# and adds X-Forwarded-For, which the gateway trusts from it.
export TARTAN_PUBLIC_URL="$web"
export TARTAN_TRUSTED_PROXIES="127.0.0.1/32"
# Identity: tartan-devnet only (design D12), or the real network with a development client.
unset TARTAN_DEV_IDENTITY TARTAN_PLC_URL TARTAN_DEV_HANDLE_RESOLVER TARTAN_JETSTREAM_URL \
	TARTAN_DEV_LOCALHOST_CLIENT TARTAN_DEV_CLIENT_METADATA_URL TARTAN_OAUTH_KEYS_FILE
if $devnet_mode; then
	export TARTAN_DEV_IDENTITY=1
	export TARTAN_PLC_URL="$devnet" TARTAN_DEV_HANDLE_RESOLVER="$devnet"
	export TARTAN_JETSTREAM_URL="ws://127.0.0.1:9400/subscribe"
	export TARTAN_OAUTH_KEYS_FILE="$state/oauth-keys.json"
elif [[ -n "$client_metadata_url" ]]; then
	export TARTAN_DEV_CLIENT_METADATA_URL="$client_metadata_url"
else
	export TARTAN_DEV_LOCALHOST_CLIENT=1
fi
# The Rivet engine keeps its data with the demo's, not in ~/.rivetkit.
export RIVETKIT_STORAGE_PATH="$state/rivet"
RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"
export RIVET_ENGINE_BINARY_PATH
export RUST_LOG="${RUST_LOG:-info}"

pids=()
stop() {
	trap - EXIT INT TERM
	if ((${#pids[@]})); then
		echo
		echo "Stopping the demo..."
		kill "${pids[@]}" 2>/dev/null || true
		wait "${pids[@]}" 2>/dev/null || true
	fi
}
trap stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

step() { echo "==> $*"; }

# wait_for <what> <log> <command...>: retries the command for up to two minutes.
wait_for() {
	local what="$1" log="$2"
	shift 2
	for _ in $(seq 240); do
		"$@" >/dev/null 2>&1 && return 0
		sleep 0.5
	done
	echo "$what did not come up; see $log" >&2
	tail -n 20 "$log" >&2 || true
	exit 1
}

step "Building the forge, tt, tartan-devnet and the extensions"
(cd "$root" && cargo build --quiet -p tartan -p tt -p tartan-devnet \
	--features tartan/dev-identity,tt/dev-identity)
tt="$root/target/debug/tt"
devnet_bin="$root/target/debug/tartan-devnet"
modules=()
issues_dir="$("$root/scripts/build-extension.sh" "$root/extensions/issues" "$state/modules/issues")"
kanban_dir="$("$root/scripts/build-extension.sh" "$root/extensions/kanban" "$state/modules/kanban")"
issues_version="$(jq -r .version "$issues_dir/tartan.json")"
kanban_version="$(jq -r .version "$kanban_dir/tartan.json")"

# The people the demo's issues are assigned to, by DID.
declare -A people=()
if $devnet_mode; then
	step "Starting tartan-devnet"
	"$devnet_bin" --listen 127.0.0.1:9400 --seed dev >"$state/logs/devnet.log" 2>&1 &
	pids+=($!)
	wait_for tartan-devnet "$state/logs/devnet.log" port_in_use 9400
	for name in alice bob carol dave erin; do
		people[$name]="$("$devnet_bin" did "$name.test")"
	done
	owner="${people[alice]}"
	owner_handle="@alice.test"
	# The web client's signing key, kept with the demo's state and readable only by its owner.
	if [[ ! -s "$TARTAN_OAUTH_KEYS_FILE" ]]; then
		(umask 077 && "$tt" admin oauth keygen >"$TARTAN_OAUTH_KEYS_FILE")
	fi
	chmod 600 "$TARTAN_OAUTH_KEYS_FILE"
else
	# The real network has one person this demo knows: the owner.
	for name in alice bob carol dave erin; do people[$name]="$owner"; done
	owner_handle="$owner"
fi

step "Starting RustFS"
RUSTFS_ACCESS_KEY="$TARTAN_S3_ACCESS_KEY" RUSTFS_SECRET_KEY="$TARTAN_S3_SECRET_KEY" \
	rustfs server --address 127.0.0.1:9100 "$state/rustfs" >"$state/logs/rustfs.log" 2>&1 &
pids+=($!)
wait_for RustFS "$state/logs/rustfs.log" curl -fsS http://127.0.0.1:9100/health
RCLONE_CONFIG_DEMO_TYPE=s3 RCLONE_CONFIG_DEMO_PROVIDER=Other \
	RCLONE_CONFIG_DEMO_ENDPOINT="$TARTAN_S3_ENDPOINT" \
	RCLONE_CONFIG_DEMO_ACCESS_KEY_ID="$TARTAN_S3_ACCESS_KEY" \
	RCLONE_CONFIG_DEMO_SECRET_ACCESS_KEY="$TARTAN_S3_SECRET_KEY" \
	rclone mkdir "demo:$TARTAN_S3_BUCKET"

step "Starting walgit"
cat >"$state/walgit.toml" <<TOML
[server]
listen = "127.0.0.1:9200"
public_url = "http://127.0.0.1:9200"
auto_create_on_push = false

[server.tls]
mode = "off"

[server.auth]
mode = "token"
anonymous_read = true
tokens = [{ principal = "tartan", token_env = "TARTAN_GIT_HOST_TOKEN", write = true, admin = true }]

[store]
backend = "s3"
bucket = "$TARTAN_S3_BUCKET"
prefix = "git"

[store.s3]
endpoint = "$TARTAN_S3_ENDPOINT"
region = "us-east-1"
access_key_env = "TARTAN_S3_ACCESS_KEY"
secret_key_env = "TARTAN_S3_SECRET_KEY"
force_path_style = true

[cache]
dir = "$state/walgit-cache"
max_bytes = "1GiB"

[events]
webhook_url = "$gateway/-/hooks/git-host"
# Without bucket notifications, the sweep is the only wake-up. The webhook secret comes from
# WALGIT__EVENTS__WEBHOOK_SECRET.
sweep_interval = "1s"
TOML
WALGIT__EVENTS__WEBHOOK_SECRET="$TARTAN_WEBHOOK_SECRET" \
	"$WALGIT_BIN" --config "$state/walgit.toml" >"$state/logs/walgit.log" 2>&1 &
pids+=($!)
wait_for walgit "$state/logs/walgit.log" port_in_use 9200

step "Starting the source registry and publishing the modules"
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
wait_for "the source registry" "$state/logs/registry.log" curl -fsS "http://$source_registry/v2/"

# Publishes the module in directory $1 as version $2, unless the source registry has it.
publish() {
	local dir="$1" version="$2" path
	path="$(sed -n 's/^module: *"\([^"@]*\).*/\1/p' "$dir/cue.mod/module.cue")"
	if ! curl -fsS -o /dev/null -H 'Accept: application/vnd.oci.image.manifest.v1+json' \
		"http://$source_registry/v2/$path/manifests/$version" 2>/dev/null; then
		(cd "$dir" && CUE_REGISTRY="$source_registry+insecure" "$cue" mod publish "$version" >/dev/null)
	fi
	echo "$path@$version"
}
modules+=("$(publish "$root/cue/types" v0.2.0)")
modules+=("$(publish "$issues_dir" "v$issues_version")")
modules+=("$(publish "$kanban_dir" "v$kanban_version")")
{
	echo "package forge"
	echo
	echo "owners: [\"$owner\"]"
	if $devnet_mode; then
		echo "// Any devnet person who signs in may read; only the owner writes."
		echo "readers: \"signed_in\""
	fi
	echo
	echo "sources: {"
	echo "	\"tartan.rawkode.academy\": \"$source_registry+insecure\""
	echo "}"
	echo "modules: ["
	for module in "${modules[@]}"; do echo "	\"$module\","; done
	echo "]"
} >"$TARTAN_CONFIG"

step "Starting the forge (gateway, actors and the Rivet engine)"
"$root/target/debug/tartan" >"$state/logs/tartan.log" 2>&1 &
pids+=($!)
wait_for "the gateway" "$state/logs/tartan.log" curl -fsS "$gateway/-/health"

step "Starting the web interface on $web"
[[ -d "$root/web/node_modules" ]] || npm --prefix "$root/web" ci --silent
(cd "$root/web" && ./node_modules/.bin/panda codegen >/dev/null)
# --ignore-lock keeps the dev server in the foreground of this script, so stopping the script
# stops it, whatever environment it runs in.
(cd "$root/web" && TARTAN_GATEWAY="$gateway" \
	exec ./node_modules/.bin/astro dev --ignore-lock --host 127.0.0.1 --port "$web_port") \
	>"$state/logs/web.log" 2>&1 &
pids+=($!)
wait_for "the web interface" "$state/logs/web.log" curl -fsS "$web/"

echo "Sign in to the web interface as $owner_handle (the owner)."

# A token for the owner, minted through Rivet as an operator would (`tt admin token create`),
# for the seeding calls below. The first attempts can find the identity actor still starting.
mint_token() {
	local printed
	printed="$("$tt" admin token create --did "$owner" --name demo.sh --scopes read,write,admin \
		--expires-in-days 1)" || return 1
	token="$(grep '^tartan_pat_' <<<"$printed")"
	[[ -n "$token" ]]
}
token=""
wait_for "a token from the identity actor" "$state/logs/tartan.log" mint_token
me() { curl -fsS -H "authorization: Bearer $token" "$gateway/-/api/me"; }
wait_for "the token" "$state/logs/tartan.log" me

# api <method> <path> [body]: calls the gateway as the owner and prints the response body.
# Fails on an error status.
api() {
	local method="$1" path="$2" body="${3:-}"
	local args=(-fsS -X "$method" -H "authorization: Bearer $token")
	[[ -n "$body" ]] && args+=(-H 'content-type: application/json' --data "$body")
	curl "${args[@]}" "$gateway$path"
}
has_packages() {
	api GET /-/api/packages | jq -e --arg issues "issues.tartan.rawkode.academy@$issues_version" \
		--arg kanban "kanban.tartan.rawkode.academy@$kanban_version" \
		'[.[] | "\(.id)@\(.version)"] as $p | ($p | index($issues)) and ($p | index($kanban))'
}
wait_for "the module sync" "$state/logs/tartan.log" has_packages

# person_env <name>: the environment every `tt` and `git` command run for that person gets:
# their own tt configuration, state, cache and file credential store under the demo's state.
person_env() {
	local home="$state/people/$1"
	mkdir -p "$home/config" "$home/state" "$home/cache"
	printf '%s\n' "XDG_CONFIG_HOME=$home/config" "XDG_STATE_HOME=$home/state" \
		"XDG_CACHE_HOME=$home/cache" "TT_CREDENTIAL_STORE=file"
}
# as <name> <command...>: runs the command as that person.
as() {
	local name="$1" envs
	shift
	mapfile -t envs < <(person_env "$name")
	env "${envs[@]}" "$@"
}
# demo_git <name> <git arguments...>: git as that person, ignoring the developer's own Git
# configuration and never writing it.
demo_git() {
	local name="$1"
	shift
	as "$name" env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
		git -c "user.name=${name^}" -c "user.email=$name@example.com" -c init.defaultBranch=main \
		-c "http.extraHeader=Authorization: Bearer $git_token" "$@"
}
# signed_commit <name> <message>: commits the index, signed by that person's DID with the tt
# built above (never a tt on PATH: a release tt refuses to run with TARTAN_DEV_IDENTITY set).
signed_commit() {
	demo_git "$1" -c gpg.format=x509 -c "gpg.x509.program=$tt" -c "user.signingKey=${people[$1]}" \
		commit --quiet -S -m "$2"
}

# tt_login <name>: signs the person in to tt for signing without a browser. tt prints the
# authorization URL; the devnet's authorization server approves at once for the login_hint, so
# following it with curl lands on tt's loopback callback.
tt_login() {
	local name="$1" log="$state/logs/tt-login-$1.log" port url pid
	# The loopback port is below the kernel's usual ephemeral range, so it does not collide with
	# outgoing connections; a port that is taken anyway is skipped.
	for port in $(seq 29100 29199); do
		port_in_use "$port" && continue
		as "$name" "$tt" sign --login --forge "$web" --handle "$name.test" --no-open \
			--port "$port" >"$log" 2>&1 &
		pid=$!
		url=""
		for _ in $(seq 120); do
			url="$(grep -o '^ *https\?://[^ ]*' "$log" | head -n 1 | tr -d ' ')" || true
			[[ -n "$url" ]] && break
			kill -0 "$pid" 2>/dev/null || break
			sleep 0.5
		done
		[[ -n "$url" ]] && break
		wait "$pid" 2>/dev/null || true
		grep -q 'Address already in use' "$log" || break
	done
	if [[ -z "$url" ]]; then
		echo "tt sign --login for $name printed no URL; see $log" >&2
		cat "$log" >&2
		exit 1
	fi
	curl -fsSL -o /dev/null "$url" || true
	if ! wait "$pid"; then
		echo "tt sign --login for $name failed:" >&2
		cat "$log" >&2
		exit 1
	fi
}

seed() {
	step "Creating org acme and repository acme/api"
	# The first calls after a start can find the actors still warming up; wait_for retries.
	wait_for "org acme" "$state/logs/tartan.log" api POST /-/api/orgs '{"slug":"acme"}'
	wait_for "repository acme/api" "$state/logs/tartan.log" \
		api POST /-/api/orgs/acme/repos '{"slug":"api"}'
	local clone_url
	clone_url="$(api GET /-/api/orgs/acme/repos/api | jq -r .clone_url)"
	# The forge's ref and signing events, kept for anyone who reads them with the token.
	api PUT /-/api/subscribers/demo '{"pattern":"ref.>"}' >/dev/null

	if $devnet_mode; then
		step "Signing alice and bob in to tt"
		tt_login alice
		tt_login bob
	fi

	step "Pushing the repository's configuration through walgit"
	local work="$state/work/api"
	rm -rf "$work"
	mkdir -p "$work/cue.mod"
	write_configuration "$work"
	# The configuration evaluates with plain `cue` against the forge's registry, exactly as
	# the forge evaluates it after the push.
	(cd "$work" && "$cue" export --out json . >/dev/null)
	(
		cd "$work"
		demo_git alice init --quiet
		demo_git alice add .
		if $devnet_mode; then
			signed_commit alice "Install Issues and Kanban, and require alice's signature"
		else
			demo_git alice commit --quiet -m "Install Issues and Kanban"
		fi
		demo_git alice push --quiet "$clone_url" main
	)
	local config_commit
	config_commit="$(demo_git alice -C "$work" rev-parse HEAD)"
	installed() {
		api GET /-/api/orgs/acme/repos/api/extensions |
			jq -e '[.[].extension_id] | sort == ["issues.tartan.rawkode.academy", "kanban.tartan.rawkode.academy"]'
	}
	wait_for "the installations from the pushed configuration" "$state/logs/tartan.log" installed
	applied() {
		api GET /-/api/orgs/acme/repos/api/config/status |
			jq -e --arg commit "$config_commit" '.applied.commit == $commit'
	}
	wait_for "the configuration (and its signing rule) in force" "$state/logs/tartan.log" applied

	step "Filing issues"
	local tool=/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools
	local issue number dids
	dids="$(for name in "${!people[@]}"; do echo "$name ${people[$name]}"; done |
		jq -Rn '[inputs | split(" ") | {(.[0]): .[1]}] | add')"
	while IFS= read -r issue; do
		number="$(api POST "$tool/create" "$(jq -c 'del(.closed)' <<<"$issue")" | jq -r .issue.number)"
		if jq -e '.closed == true' <<<"$issue" >/dev/null; then
			api POST "$tool/close" "{\"number\":$number}" >/dev/null
		fi
	done < <(jq -c --argjson dids "$dids" '.[] |
		if .properties.assignee then .properties.assignee = $dids[.properties.assignee] else . end' <<<"$ISSUES")
	boards_ready() {
		api POST /-/api/orgs/acme/repos/api/extensions/kanban.tartan.rawkode.academy/tools/board '{"board":"triage"}' |
			jq -e '[.lanes[].cells[].groups[].cards[]] | length >= 10'
	}
	wait_for "the boards" "$state/logs/tartan.log" boards_ready

	if $devnet_mode; then
		# The configuration commit was judged by the rule before it (none); this one is judged
		# by the rule it put in force.
		step "Pushing a commit signed by alice to main, which the rule covers"
		(
			cd "$work"
			cat >>README.md <<'MD'

`main` and `release/*` must be signed by @alice.test: see `signing` in `tartan.cue`.
MD
			demo_git alice add README.md
			signed_commit alice "Explain the signing rule"
			demo_git alice push --quiet "$clone_url" main
		)
		main_compliant() {
			api GET "/-/api/orgs/acme/repos/api/signatures?ref=refs/heads/main" |
				jq -e --arg did "${people[alice]}" \
					'.compliant == true and .status == "verified" and .did == $did'
		}
		wait_for "the signature check of main" "$state/logs/tartan.log" main_compliant

		step "Pushing a commit signed by bob to release/1.0, which the rule does not allow"
		(
			cd "$work"
			demo_git bob checkout --quiet -b release/1.0
			echo "# Changelog" >CHANGELOG.md
			demo_git bob add CHANGELOG.md
			signed_commit bob "Start the 1.0 changelog"
			demo_git bob push --quiet "$clone_url" release/1.0
		)
		judged() {
			api GET "/-/api/orgs/acme/repos/api/signatures?ref=refs/heads/release/1.0" |
				jq -e '.compliant == false'
		}
		wait_for "the signature check of release/1.0" "$state/logs/tartan.log" judged
	fi
	touch "$state/seeded"
}

write_configuration() {
	local dir="$1"
	cat >"$dir/cue.mod/module.cue" <<CUE
module: "example.com/acme/api"
language: version: "v0.14.1"
deps: {
	"tartan.rawkode.academy/types@v0": v: "v0.2.0"
	"tartan.rawkode.academy/issues@v0": v: "v$issues_version"
	"tartan.rawkode.academy/kanban@v0": v: "v$kanban_version"
}
CUE
	cat >"$dir/tartan.cue" <<'CUE'
package tartan

import (
	"tartan.rawkode.academy/types"
	"tartan.rawkode.academy/issues"
	"tartan.rawkode.academy/kanban"
)

let props = extensions."issues.tartan.rawkode.academy".settings.properties

// Issues, with the properties every issue of this repository may have.
extensions: "issues.tartan.rawkode.academy": issues.#Extension & {settings: properties: {
	status: types.#Select & {
		label:   "Status"
		options: ["backlog", "todo", "doing", "review", "done"]
		labels: {backlog: "Backlog", todo: "To do", doing: "In progress", review: "In review", done: "Done"}
		default: "backlog"
	}
	priority: types.#Select & {
		label:   "Priority"
		options: ["urgent", "high", "medium", "low"]
		labels: {urgent: "Urgent", high: "High", medium: "Medium", low: "Low"}
	}
	assignee: types.#Person & {label: "Assignee"}
	estimate: types.#Number & {label: "Estimate (points)", min: 0, max: 40}
	due:      types.#Date & {label: "Due"}
	notes:    types.#LongText & {label: "Notes", maxLength: 5000}
	link:     types.#URL & {label: "Link"}
	budget:   types.#Money & {label: "Budget", currency: "GBP"}
	labels: types.#MultiSelect & {
		label:   "Labels"
		options: ["bug", "feature", "docs", "performance", "security"]
		labels: {bug: "Bug", feature: "Feature", docs: "Docs", performance: "Performance", security: "Security"}
	}
}}

// Kanban, with boards over those properties. A board's columns must be one of the select
// properties above: a typo or an inline lookalike fails evaluation.
extensions: "kanban.tartan.rawkode.academy": kanban.#Extension & {
	#issues: extensions."issues.tartan.rawkode.academy"
	settings: boards: {
		triage: {
			name:    "Triage"
			columns: props.status
			lanes:   props.priority
			group:   "project"
		}
		web: {
			name:    "Web app"
			project: "web"
			columns: {property: props.status, values: ["todo", "doing", "review", "done"]}
			lanes:   props.labels
			group:   props.assignee
		}
	}
}
CUE
	if $devnet_mode; then
		cat >>"$dir/tartan.cue" <<CUE

// main and release branches must be signed by @alice.test (${people[alice]}).
signing: types.#Signing & {require: [{
	refs: ["refs/heads/main", "refs/heads/release/*"]
	signers: ["${people[alice]}"]
}]}
CUE
	fi
	cat >"$dir/README.md" <<'MD'
# acme/api

The demo repository. `tartan.cue` installs Issues and Kanban; `cue export` with
`CUE_REGISTRY=127.0.0.1:8080+insecure` shows exactly what the forge applies.
MD
}

# The demo's issues: properties by name (money in pence, assignees by the person's short name,
# replaced with their DID when filed), a project, and whether to close it.
ISSUES='[
	{"title": "Login fails with an expired session cookie", "project": "web",
		"body": "Signing in after a week away shows a blank page instead of the login form.\n\n1. Sign in\n2. Wait for the session to expire\n3. Reload",
		"properties": {"status": "doing", "priority": "urgent", "assignee": "alice", "estimate": 3,
			"due": "2026-11-12", "labels": ["bug", "security"],
			"link": "https://example.com/support/4821"}},
	{"title": "Dark mode for the dashboard", "project": "web",
		"properties": {"status": "todo", "priority": "medium", "assignee": "bob", "estimate": 8,
			"labels": ["feature"], "budget": 120000,
			"notes": "Follow the system preference, with a **manual override** in settings."}},
	{"title": "Board drag and drop on touch screens", "project": "web",
		"properties": {"status": "review", "priority": "high", "assignee": "carol", "estimate": 5,
			"labels": ["feature"], "due": "2026-11-20"}},
	{"title": "Document the onboarding flow", "project": "web",
		"properties": {"status": "done", "priority": "low", "assignee": "bob", "estimate": 2,
			"labels": ["docs"]}},
	{"title": "Rate limit the public API", "project": "crates/api",
		"body": "Unauthenticated clients can issue unbounded requests.",
		"properties": {"status": "todo", "priority": "urgent", "assignee": "dave", "estimate": 5,
			"due": "2026-11-15", "labels": ["security", "performance"], "budget": 250000}},
	{"title": "Paginate the issues endpoint", "project": "crates/api",
		"properties": {"status": "doing", "priority": "high", "assignee": "alice", "estimate": 3,
			"labels": ["performance"]}},
	{"title": "OpenAPI description of the gateway", "project": "crates/api",
		"properties": {"status": "backlog", "priority": "medium", "estimate": 5, "labels": ["docs"],
			"link": "https://spec.openapis.org/oas/v3.1.0"}},
	{"title": "Slow cold start of the CLI", "project": "crates/cli",
		"properties": {"status": "review", "priority": "medium", "assignee": "erin", "estimate": 2,
			"labels": ["performance", "bug"]}},
	{"title": "Shell completions", "project": "crates/cli",
		"properties": {"status": "backlog", "priority": "low", "labels": ["feature"]}},
	{"title": "Quarterly dependency audit",
		"properties": {"status": "todo", "priority": "high", "assignee": "dave", "due": "2026-12-01",
			"labels": ["security"], "budget": 80000,
			"notes": "- Review advisories\n- Bump pinned versions\n- Record exceptions"}},
	{"title": "Pick a name for the 1.0 release",
		"properties": {"status": "backlog", "estimate": 1}},
	{"title": "Typo on the pricing page", "project": "web",
		"properties": {"status": "done", "priority": "low", "assignee": "carol", "estimate": 1,
			"labels": ["bug"]}, "closed": true}
]'

if [[ -f "$state/seeded" ]]; then
	step "Reusing the seeded demo in $state"
	if $devnet_mode; then
		# tartan-devnet keeps its PDS and authorization server state in memory, so the signing
		# sessions from the previous run ended with it.
		step "Signing alice and bob in to tt again"
		tt_login alice
		tt_login bob
	fi
else
	seed
fi

if $devnet_mode; then
	cat <<EOF

Tartan demo is running, with every person from tartan-devnet (nothing leaves this machine).

  Web interface   $web/#/acme/api
  Triage board    $web/#/acme/api/boards/triage
  Web app board   $web/#/acme/api/boards/web
  Sign in as      @alice.test (the owner), or @bob.test, @carol.test, @dave.test or @erin.test
                  to see read-only access; the devnet approves at once, with no password
  Signatures      main is signed by @alice.test; release/1.0 has a commit signed by @bob.test,
                  whom the signing rule does not allow
  Gateway API     $gateway/-/api   (Authorization: Bearer <a token from Settings, Access tokens>)
  Clone           git clone http://127.0.0.1:9200/acme/api.git
  Push            git -c "http.extraHeader=Authorization: Bearer $git_token" push origin main
  Sign as alice   XDG_CONFIG_HOME=$state/people/alice/config XDG_STATE_HOME=$state/people/alice/state \\
                  XDG_CACHE_HOME=$state/people/alice/cache TT_CREDENTIAL_STORE=file \\
                  git -c gpg.format=x509 -c gpg.x509.program=$tt \\
                  -c user.signingKey=${people[alice]} commit -S
                  (in a shell with TARTAN_DEV_IDENTITY=1 TARTAN_PLC_URL=$devnet
                  TARTAN_DEV_HANDLE_RESOLVER=$devnet)
  Evaluate        CUE_REGISTRY=127.0.0.1:8080+insecure cue export   (in a clone)
  Logs            $state/logs

Press Ctrl-C to stop everything.
EOF
else
	cat <<EOF

Tartan demo is running against the real ATProto network, owned by $owner.

  Web interface   $web/#/acme/api
  Sign in as      $owner (or its handle)
EOF
	if [[ -n "$client_metadata_url" ]]; then
		cat <<EOF
  Sign-in client  a public native client whose id is $client_metadata_url:
                  that URL must serve exactly what $web/-/auth/client-metadata.json serves
EOF
	else
		cat <<EOF
  Sign-in client  ATProto's http://localhost development client. If your authorization server
                  refuses it ("client_id must be a safe https metadata URL"), host what
                  $web/-/auth/client-metadata.json serves once started with
                  --client-metadata-url <https URL> at that URL, and run the demo with it.
EOF
	fi
	cat <<EOF
  Signing         tt sign --login --client localhost --handle <your handle>, or, where the
                  authorization server refuses that client, host the document tt prints and run
                  tt sign --login --client-metadata-url <https URL> --handle <your handle>; then
                  git -c gpg.format=x509 -c gpg.x509.program=$tt -c user.signingKey=$owner commit -S
                  (signing also needs the server to grant the granular scope
                  repo:academy.rawkode.tartan.signature?action=create)
  Gateway API     $gateway/-/api   (Authorization: Bearer <a token from Settings, Access tokens>)
  Clone           git clone http://127.0.0.1:9200/acme/api.git
  Logs            $state/logs

Press Ctrl-C to stop everything.
EOF
fi
wait
