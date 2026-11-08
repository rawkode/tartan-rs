#!/usr/bin/env bash
# Builds a Rust extension crate into a package directory: component.wasm, tartan.json and the
# manifest's migrations.
#
#   scripts/build-extension.sh <crate-dir> <out-dir>
#
# Needs cargo with the wasm32-unknown-unknown target, and wasm-tools on PATH (or WASM_TOOLS).
set -euo pipefail

crate="$(cd "$1" && pwd)"
out="$2"
wasm_tools="${WASM_TOOLS:-wasm-tools}"
target_dir="${CARGO_TARGET_DIR:-$crate/target}"

name="$(awk -F'"' '/^name *=/ { print $2; exit }' "$crate/Cargo.toml")"
(cd "$crate" && CARGO_TARGET_DIR="$target_dir" cargo build --quiet --release --target wasm32-unknown-unknown)

mkdir -p "$out"
"$wasm_tools" component new "$target_dir/wasm32-unknown-unknown/release/${name//-/_}.wasm" -o "$out/component.wasm"
cp "$crate/tartan.json" "$out/tartan.json"
for migration in $(jq -r '.storage.migrations[]? // empty' "$crate/tartan.json"); do
	mkdir -p "$out/$(dirname "$migration")"
	cp "$crate/$migration" "$out/$migration"
done
echo "$out"
