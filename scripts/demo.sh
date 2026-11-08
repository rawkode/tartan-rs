#!/usr/bin/env bash
# Runs a seeded Tartan forge and its web interface for a demo, all on this machine:
#
#   RustFS        127.0.0.1:9100   object store for Git, the registry and extension packages
#   walgit        127.0.0.1:9200   the Git host
#   registry      127.0.0.1:9300   a source CUE registry (CNCF distribution) holding the
#                                  tartan.dev/types, Issues and Kanban modules
#   Rivet engine  127.0.0.1:6420   started by the forge
#   gateway       127.0.0.1:8080   the forge's API and its own CUE registry
#   web           127.0.0.1:4321   the web interface (Astro dev server; the next free port
#                                  when 4321 is taken)
#
# The forge's deployment configuration lists the three modules, so it copies them from the
# source registry into its own at startup, and CUE_REGISTRY points it at itself. The demo then
# creates org acme and repository acme/api, pushes a configuration with git through walgit (a
# cue.mod/module.cue depending on the modules and a tartan.cue installing Issues with typed
# properties and Kanban with two boards), and files a dozen issues.
#
#   scripts/demo.sh          start from nothing: earlier demo state is deleted and reseeded
#   scripts/demo.sh --keep   reuse the previous demo's state; seed only if it never finished
#
# State lives in .dev/demo. Ctrl-C stops every process the script started.
#
# Needs: cargo with the wasm32-unknown-unknown target, npm, jq, curl, git, rclone and rustfs
# on PATH, wasm-tools (or WASM_TOOLS), CUE_BIN (or cue on PATH), REGISTRY_BIN (a distribution
# `registry` binary) and WALGIT_BIN (a walgit binary). Development credentials only; nothing
# here is a secret.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
state="$root/.dev/demo"
keep=false
case "${1:-}" in
"") ;;
--keep) keep=true ;;
*)
	echo "usage: $0 [--keep]" >&2
	exit 2
	;;
esac

: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary (github.com/tobi/walgit)}"
: "${REGISTRY_BIN:?set REGISTRY_BIN to a CNCF distribution registry binary}"
cue="${CUE_BIN:-cue}"
for tool in "$cue" cargo npm jq curl git rclone rustfs; do
	command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 1; }
done

port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
for port in 9100 9200 9300 6420 8080; do
	if port_in_use "$port"; then
		echo "127.0.0.1:$port is already in use; stop what runs there (scripts/dev.sh, another demo or a Rivet engine) first" >&2
		exit 1
	fi
done

# A fresh demo starts from nothing. Directories CUE extracted modules into are read only.
if ! $keep || [[ ! -f "$state/seeded" ]]; then
	if [[ -d "$state" ]]; then
		chmod -R u+w "$state"
		rm -rf "$state"
	fi
fi
mkdir -p "$state/logs" "$state/rustfs" "$state/registry"

admin_token="dev-admin-token"
git_token="dev-git-host-token"
gateway="http://127.0.0.1:8080"
source_registry="127.0.0.1:9300"
export TARTAN_S3_ACCESS_KEY="tartandev" TARTAN_S3_SECRET_KEY="tartandevsecret"
export TARTAN_S3_ENDPOINT="http://127.0.0.1:9100" TARTAN_S3_BUCKET="tartan-demo"
export TARTAN_ADMIN_TOKEN="$admin_token" TARTAN_WEBHOOK_SECRET="dev-webhook-secret"
export TARTAN_GIT_HOST_TOKEN="$git_token" TARTAN_GIT_HOST_URL="http://127.0.0.1:9200"
export TARTAN_LISTEN="127.0.0.1:8080"
# The forge resolves every configuration's imports through its own registry, its gateway.
export CUE_REGISTRY="127.0.0.1:8080+insecure"
export CUE_CACHE_DIR="$state/cue-cache"
export TARTAN_CONFIG="$state/forge.cue"
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

step "Building the forge and the extensions"
(cd "$root" && cargo build --quiet -p tartan)
modules=()
issues_dir="$("$root/scripts/build-extension.sh" "$root/extensions/issues" "$state/modules/issues")"
kanban_dir="$("$root/scripts/build-extension.sh" "$root/extensions/kanban" "$state/modules/kanban")"
issues_version="$(jq -r .version "$issues_dir/tartan.json")"
kanban_version="$(jq -r .version "$kanban_dir/tartan.json")"

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
modules+=("$(publish "$root/cue/types" v0.1.0)")
modules+=("$(publish "$issues_dir" "v$issues_version")")
modules+=("$(publish "$kanban_dir" "v$kanban_version")")
{
	echo "package forge"
	echo
	echo "sources: {"
	echo "	\"tartan.dev\":                    \"$source_registry+insecure\""
	echo "	\"github.com/rawkode/tartan-ext\": \"$source_registry+insecure\""
	echo "}"
	echo "modules: ["
	for module in "${modules[@]}"; do echo "	\"$module\","; done
	echo "]"
} >"$TARTAN_CONFIG"

step "Starting the forge (gateway, actors and the Rivet engine)"
"$root/target/debug/tartan" >"$state/logs/tartan.log" 2>&1 &
pids+=($!)
wait_for "the gateway" "$state/logs/tartan.log" curl -fsS "$gateway/-/health"

# api <method> <path> [body]: calls the gateway as the admin and prints the response body.
# Fails on an error status.
api() {
	local method="$1" path="$2" body="${3:-}"
	local args=(-fsS -X "$method" -H "authorization: Bearer $admin_token")
	[[ -n "$body" ]] && args+=(-H 'content-type: application/json' --data "$body")
	curl "${args[@]}" "$gateway$path"
}
has_packages() {
	api GET /-/api/packages | jq -e --arg issues "rawkode.issues@$issues_version" \
		--arg kanban "rawkode.kanban@$kanban_version" \
		'[.[] | "\(.id)@\(.version)"] as $p | ($p | index($issues)) and ($p | index($kanban))'
}
wait_for "the module sync" "$state/logs/tartan.log" has_packages

seed() {
	step "Creating org acme and repository acme/api"
	# The first calls after a start can find the actors still warming up; wait_for retries.
	wait_for "org acme" "$state/logs/tartan.log" api POST /-/api/orgs '{"slug":"acme"}'
	wait_for "repository acme/api" "$state/logs/tartan.log" \
		api POST /-/api/orgs/acme/repos '{"slug":"api"}'
	local clone_url
	clone_url="$(api GET /-/api/orgs/acme/repos/api | jq -r .clone_url)"

	step "Pushing the repository's configuration through walgit"
	local work="$state/work/api"
	rm -rf "$work"
	mkdir -p "$work/cue.mod"
	write_configuration "$work"
	# The configuration evaluates with plain `cue` against the forge's registry, exactly as
	# the forge evaluates it after the push.
	(cd "$work" && "$cue" export --out json . >/dev/null)
	local git=(git -c user.name=Demo -c user.email=demo@example.com -c init.defaultBranch=main
		-c "http.extraHeader=Authorization: Bearer $git_token")
	(
		cd "$work"
		"${git[@]}" init --quiet
		"${git[@]}" add .
		"${git[@]}" commit --quiet -m "Install Issues and Kanban"
		"${git[@]}" push --quiet "$clone_url" main
	)
	installed() {
		api GET /-/api/orgs/acme/repos/api/extensions |
			jq -e '[.[].extension_id] | sort == ["rawkode.issues", "rawkode.kanban"]'
	}
	wait_for "the installations from the pushed configuration" "$state/logs/tartan.log" installed

	step "Filing issues"
	local tool=/-/api/orgs/acme/repos/api/extensions/rawkode.issues/tools
	local issue number
	while IFS= read -r issue; do
		number="$(api POST "$tool/create" "$(jq -c 'del(.closed)' <<<"$issue")" | jq -r .issue.number)"
		if jq -e '.closed == true' <<<"$issue" >/dev/null; then
			api POST "$tool/close" "{\"number\":$number}" >/dev/null
		fi
	done < <(jq -c '.[]' <<<"$ISSUES")
	boards_ready() {
		api POST /-/api/orgs/acme/repos/api/extensions/rawkode.kanban/tools/board '{"board":"triage"}' |
			jq -e '[.lanes[].cells[].groups[].cards[]] | length >= 10'
	}
	wait_for "the boards" "$state/logs/tartan.log" boards_ready
	touch "$state/seeded"
}

write_configuration() {
	local dir="$1"
	cat >"$dir/cue.mod/module.cue" <<CUE
module: "example.com/acme/api"
language: version: "v0.14.1"
deps: {
	"tartan.dev/types@v0": v: "v0.1.0"
	"github.com/rawkode/tartan-ext/issues@v0": v: "v$issues_version"
	"github.com/rawkode/tartan-ext/kanban@v0": v: "v$kanban_version"
}
CUE
	cat >"$dir/tartan.cue" <<'CUE'
package tartan

import (
	"tartan.dev/types"
	"github.com/rawkode/tartan-ext/issues"
	"github.com/rawkode/tartan-ext/kanban"
)

let props = extensions."rawkode.issues".settings.properties

// Issues, with the properties every issue of this repository may have.
extensions: "rawkode.issues": issues.#Extension & {settings: properties: {
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
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
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
	cat >"$dir/README.md" <<'MD'
# acme/api

The demo repository. `tartan.cue` installs Issues and Kanban; `cue export` with
`CUE_REGISTRY=127.0.0.1:8080+insecure` shows exactly what the forge applies.
MD
}

# The demo's issues: properties by name (money in pence), a project, and whether to close it.
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
else
	seed
fi

step "Starting the web interface"
[[ -d "$root/web/node_modules" ]] || npm --prefix "$root/web" ci --silent
(cd "$root/web" && ./node_modules/.bin/panda codegen >/dev/null)
# --ignore-lock keeps the dev server in the foreground of this script, so stopping the script
# stops it, whatever environment it runs in.
(cd "$root/web" && TARTAN_GATEWAY="$gateway" \
	exec ./node_modules/.bin/astro dev --ignore-lock --host 127.0.0.1 --port 4321) \
	>"$state/logs/web.log" 2>&1 &
pids+=($!)
# The dev server takes the next free port when 4321 is taken (for example by a server bound
# to ::1 only), so its address is read from its log.
web_url() { grep -o 'http://127\.0\.0\.1:[0-9]*' "$state/logs/web.log" | tail -n 1; }
web_ready() { [[ -n "$(web_url)" ]] && curl -fsS "$(web_url)/" >/dev/null; }
wait_for "the web interface" "$state/logs/web.log" web_ready
web="$(web_url)"

cat <<EOF

Tartan demo is running.

  Web interface   $web/#/acme/api
  Triage board    $web/#/acme/api/boards/triage
  Web app board   $web/#/acme/api/boards/web
  Gateway API     $gateway/-/api   (Authorization: Bearer $admin_token)
  Clone           git clone http://127.0.0.1:9200/acme/api.git
  Push            git -c "http.extraHeader=Authorization: Bearer $git_token" push origin main
  Evaluate        CUE_REGISTRY=127.0.0.1:8080+insecure cue export   (in a clone)
  Logs            $state/logs

Press Ctrl-C to stop everything.
EOF
wait
