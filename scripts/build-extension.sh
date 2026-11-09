#!/usr/bin/env bash
# Builds a Rust extension crate into a CUE module directory: the crate's cue/ tree (its
# cue.mod/module.cue and its #Extension schema), component.wasm, tartan.json and the manifest's
# migrations. Publish it with `cue mod publish v<version>`, where version is tartan.json's.
#
#   scripts/build-extension.sh <crate-dir> <out-dir>
#
# A crate without a cue/ directory gets a minimal module, tartan.rawkode.academy/<dir>,
# whose #Extension fixes the manifest's id and version and accepts any settings.
#
# Needs cargo with the wasm32-unknown-unknown target, jq, and wasm-tools on PATH (or
# WASM_TOOLS).
set -euo pipefail

crate="$(cd "$1" && pwd)"
out="$2"
wasm_tools="${WASM_TOOLS:-wasm-tools}"
target_dir="${CARGO_TARGET_DIR:-$crate/target}"

name="$(awk -F'"' '/^name *=/ { print $2; exit }' "$crate/Cargo.toml")"
(cd "$crate" && CARGO_TARGET_DIR="$target_dir" cargo build --quiet --release --target wasm32-unknown-unknown)

# The output is rebuilt from scratch, so files dropped from the crate do not linger in the
# module. Only a directory this script built before (or an empty one) is replaced.
if [[ -e "$out" ]]; then
	if [[ -f "$out/tartan.json" || -z "$(ls -A "$out")" ]]; then
		rm -rf "$out"
	else
		echo "$out exists and is not an extension module directory" >&2
		exit 1
	fi
fi
mkdir -p "$out"

id="$(jq -r '.id' "$crate/tartan.json")"
version="$(jq -r '.version' "$crate/tartan.json")"
if [[ -d "$crate/cue" ]]; then
	cp -R "$crate/cue/." "$out/"
else
	dir="$(basename "$crate")"
	package="${dir//-/_}"
	mkdir -p "$out/cue.mod"
	cat >"$out/cue.mod/module.cue" <<MODULE
module: "tartan.rawkode.academy/$dir@v${version%%.*}"
language: version: "v0.14.1"
source: kind: "self"
MODULE
	cat >"$out/$package.cue" <<SCHEMA
package $package

#Extension: {
	id:      "$id"
	version: "$version"
	settings: {...}
}
SCHEMA
fi

"$wasm_tools" component new "$target_dir/wasm32-unknown-unknown/release/${name//-/_}.wasm" -o "$out/component.wasm"
cp "$crate/tartan.json" "$out/tartan.json"
for migration in $(jq -r '.storage.migrations[]? // empty' "$crate/tartan.json"); do
	mkdir -p "$out/$(dirname "$migration")"
	cp "$crate/$migration" "$out/$migration"
done
echo "$out"
