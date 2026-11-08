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

# The full stack: RustFS, walgit and the git CLI. WALGIT_BIN names a walgit binary.
if [[ -n "${WALGIT_BIN:-}" ]]; then
	cargo test -p tartan --test git -- --include-ignored
else
	echo "WALGIT_BIN is not set: skipped the git tests" >&2
	exit 1
fi
