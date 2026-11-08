#!/usr/bin/env bash
# Checks the tartan.dev/types module: every case in testdata/valid must export,
# and every case in testdata/invalid must fail to.
#
#   cue/types/test.sh            # uses `cue` on PATH, or CUE_BIN
set -euo pipefail

cue="${CUE_BIN:-cue}"
module="$(cd "$(dirname "$0")" && pwd)"
cd "$module"

failed=0
"$cue" vet -c ./types.cue >/dev/null || {
	echo "FAIL: the types package does not evaluate"
	failed=1
}
for case in testdata/valid/*.cue; do
	if ! output="$("$cue" export "./$case" 2>&1)"; then
		echo "FAIL (should evaluate): $case"
		echo "$output" | sed 's/^/    /'
		failed=1
	fi
done
for case in testdata/invalid/*.cue; do
	if "$cue" export "./$case" >/dev/null 2>&1; then
		echo "FAIL (should be refused): $case"
		failed=1
	fi
done

valid="$(find testdata/valid -name '*.cue' | wc -l)"
invalid="$(find testdata/invalid -name '*.cue' | wc -l)"
if [ "$failed" -ne 0 ]; then
	exit 1
fi
echo "ok: $valid valid and $invalid invalid cases"
