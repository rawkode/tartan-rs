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

# Extensions: the Issues package, built from TARTAN_ISSUES_SRC (an extension crate) unless
# TARTAN_ISSUES_PACKAGE already names a package directory.
if [[ -z "${TARTAN_ISSUES_PACKAGE:-}" ]]; then
	: "${TARTAN_ISSUES_SRC:?set TARTAN_ISSUES_SRC to the Issues extension crate, or TARTAN_ISSUES_PACKAGE}"
	TARTAN_ISSUES_PACKAGE="$("$root/scripts/build-extension.sh" "$TARTAN_ISSUES_SRC" "$root/.dev/packages/issues")"
fi
export TARTAN_ISSUES_PACKAGE
cargo test -p tartan-extensions -- --include-ignored
cargo test -p tartan --test extensions -- --include-ignored

# The full stack: RustFS, walgit and the git CLI. WALGIT_BIN names a walgit binary.
if [[ -n "${WALGIT_BIN:-}" ]]; then
	cargo test -p tartan --test git -- --include-ignored
else
	echo "WALGIT_BIN is not set: skipped the git tests" >&2
	exit 1
fi
