#!/usr/bin/env bash
# Runs every test, the end-to-end ones against a local Rivet engine and real CUE registries.
#
# Needs REGISTRY_BIN, a CNCF distribution `registry` binary: the source registry test modules
# and the first-party modules are published to (`cue mod publish`) and the forge under test
# syncs them from. CUE_BIN names the cue command (default: cue on PATH). WALGIT_BIN names a
# walgit binary for the git tests, which also need rustfs, rclone and git on PATH; building
# the extensions needs jq and wasm-tools (or WASM_TOOLS).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"
export RIVET_ENGINE_BINARY_PATH
: "${REGISTRY_BIN:?set REGISTRY_BIN to a CNCF distribution registry binary}"
: "${WALGIT_BIN:?set WALGIT_BIN to a walgit binary for the git tests}"
for tool in jq rustfs rclone git; do
	command -v "$tool" >/dev/null || { echo "$tool is not on PATH" >&2; exit 1; }
done
CUE_BIN="${CUE_BIN:-cue}"
export CUE_BIN
command -v "$CUE_BIN" >/dev/null || { echo "CUE_BIN ($CUE_BIN) is not a cue command" >&2; exit 1; }

cd "$root"

# The documentation site builds and its pages lint clean.
[[ -d docs/node_modules ]] || npm --prefix docs ci --silent
npm --prefix docs run --silent build >/dev/null
npm --prefix docs run --silent lint:docs

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# The registries: a fresh source registry, and the address the forge under test serves its own
# registry on. The CUE evaluator in each test process reads CUE_REGISTRY when it starts, so the
# forge's address is fixed here, before the tests run.
work="$(mktemp -d)"
pids=()
cleanup() {
	kill "${pids[@]}" 2>/dev/null || true
	wait "${pids[@]}" 2>/dev/null || true
	# CUE extracts modules into its cache read only.
	chmod -R u+w "$work"
	rm -rf "$work"
}
trap cleanup EXIT

port_in_use() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
free_port() {
	local port
	for _ in $(seq 100); do
		port=$((20000 + RANDOM % 40000))
		port_in_use "$port" || { echo "$port"; return; }
	done
	echo "no free port" >&2
	return 1
}

source_port="$(free_port)"
cat >"$work/registry.yml" <<YAML
version: 0.1
log:
  level: warn
storage:
  filesystem:
    rootdirectory: $work/registry
http:
  addr: 127.0.0.1:$source_port
YAML
"$REGISTRY_BIN" serve "$work/registry.yml" >"$work/registry.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do port_in_use "$source_port" && break; sleep 0.2; done
port_in_use "$source_port" || { cat "$work/registry.log" >&2; exit 1; }
export TARTAN_MODULE_SOURCE="127.0.0.1:$source_port"
export CUE_REGISTRY="127.0.0.1:$(free_port)+insecure"
export CUE_CACHE_DIR="$work/cue-cache"

# Extensions: their own checks and tests, then their module directories for the host's tests.
for extension in issues kanban; do
	(cd "$root/extensions/$extension" && cargo fmt --check && cargo clippy --quiet --all-targets -- -D warnings && cargo test --quiet)
done
TARTAN_ISSUES_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/issues" "$root/.dev/packages/issues")"
export TARTAN_ISSUES_PACKAGE
TARTAN_KANBAN_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/kanban" "$root/.dev/packages/kanban")"
export TARTAN_KANBAN_PACKAGE
TARTAN_FIXTURE_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/fixture" "$root/.dev/packages/fixture")"
export TARTAN_FIXTURE_PACKAGE

# The first-party modules, published to the source registry as a deployment's would be:
# tartan.rawkode.academy/types, then Issues and Kanban with their packages inside. Tests that configure
# repositories with them list them in their forge's deployment (TARTAN_EXTENSION_MODULES).
publish_module() {
	local dir="$1" version="$2" path
	path="$(sed -n 's/^module: *"\([^"@]*\).*/\1/p' "$dir/cue.mod/module.cue")"
	(cd "$dir" && CUE_REGISTRY="$TARTAN_MODULE_SOURCE+insecure" "$CUE_BIN" mod publish "$version" >/dev/null)
	echo "$path@$version"
}
extension_modules=("$(publish_module "$root/cue/types" v0.2.0)")
for dir in "$TARTAN_ISSUES_PACKAGE" "$TARTAN_KANBAN_PACKAGE"; do
	extension_modules+=("$(publish_module "$dir" "v$(jq -r .version "$dir/tartan.json")")")
done
export TARTAN_EXTENSION_MODULES="${extension_modules[*]}"
# The last v0.1.0 of the types module, kept as a fixture beside v0.2.0 so configurations written
# for it stay testable; it is not in TARTAN_EXTENSION_MODULES, whose modules a test repository
# depends on all at once.
TARTAN_TYPES_V0_1_MODULE="$(publish_module "$root/cue/types/testdata/v0.1.0" v0.1.0)"
export TARTAN_TYPES_V0_1_MODULE

# The modules' own CUE checks: the property types, and Kanban's boards against Issues'
# properties, evaluated natively from the source registry.
"$root/cue/types/test.sh"
"$root/extensions/kanban/cue/test.sh" "$TARTAN_MODULE_SOURCE"

cargo test -p tartan-extensions -- --include-ignored
cargo test -p tartan --test extensions -- --include-ignored
cargo test -p tartan --test subscriptions -- --include-ignored
cargo test -p tartan --test kanban -- --include-ignored

# The full stack: RustFS, walgit and the git CLI.
cargo test -p tartan --test git -- --include-ignored
echo "e2e: every check and suite passed"
