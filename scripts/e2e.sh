#!/usr/bin/env bash
# Runs every test, the end-to-end ones against a local Rivet engine.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
RIVET_ENGINE_BINARY_PATH="$("$root/scripts/rivet-engine.sh")"
export RIVET_ENGINE_BINARY_PATH

cd "$root"
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# Extensions: their own tests, then their packages for the host's tests.
(cd "$root/extensions/issues" && cargo test --quiet)
(cd "$root/extensions/kanban" && cargo test --quiet)
TARTAN_ISSUES_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/issues" "$root/.dev/packages/issues")"
export TARTAN_ISSUES_PACKAGE
TARTAN_KANBAN_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/kanban" "$root/.dev/packages/kanban")"
export TARTAN_KANBAN_PACKAGE
TARTAN_FIXTURE_PACKAGE="$("$root/scripts/build-extension.sh" "$root/extensions/fixture" "$root/.dev/packages/fixture")"
export TARTAN_FIXTURE_PACKAGE
cargo test -p tartan-extensions -- --include-ignored
cargo test -p tartan --test extensions -- --include-ignored
cargo test -p tartan --test subscriptions -- --include-ignored
cargo test -p tartan --test kanban -- --include-ignored

# The full stack: RustFS, walgit and the git CLI. WALGIT_BIN names a walgit binary.
if [[ -n "${WALGIT_BIN:-}" ]]; then
	cargo test -p tartan --test git -- --include-ignored
else
	echo "WALGIT_BIN is not set: skipped the git tests" >&2
	exit 1
fi
