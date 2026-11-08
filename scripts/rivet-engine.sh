#!/usr/bin/env bash
# Prints the path of the Rivet engine binary matching the rivetkit crate, downloading and
# checksum-verifying it into .dev/bin on first use.
set -euo pipefail

version="3.0.6"
artifact="rivet-engine-x86_64-unknown-linux-musl"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/.dev/bin/rivet-engine-$version"

if [[ ! -x "$dest" ]]; then
	mkdir -p "$(dirname "$dest")"
	base="https://releases.rivet.dev/rivet/$version/engine"
	sums="$(curl -fsS "$base/SHA256SUMS")"
	expected="$(awk -v a="$artifact" '$2 == a { print $1 }' <<<"$sums")"
	[[ -n "$expected" ]] || { echo "no checksum for $artifact" >&2; exit 1; }
	curl -fsS -o "$dest.part" "$base/$artifact"
	echo "$expected  $dest.part" | sha256sum -c --quiet
	chmod +x "$dest.part"
	mv "$dest.part" "$dest"
fi
echo "$dest"
