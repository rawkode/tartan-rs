#!/usr/bin/env bash
# Checks the tartan.rawkode.academy/types module: every case in testdata/valid must export,
# and every case in testdata/invalid must fail to. Then it checks the value
# vectors Issues shares (extensions/issues/testdata/property-values.json): a
# value Issues accepts for a property must be a valid `default` of it in CUE,
# and a value Issues refuses must not, so the module and the extension agree.
# The vectors are checked against the version of this module Issues depends on:
# an earlier version kept as a fixture in testdata/<version> (such as
# testdata/v0.1.0, a nested module that is not published with this one) when
# Issues pins it, otherwise this module.
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
for fixture in testdata/v[0-9]*/; do
	[ -d "$fixture" ] || continue
	(cd "$fixture" && "$cue" vet -c ./types.cue >/dev/null) || {
		echo "FAIL: the $fixture fixture does not evaluate"
		failed=1
	}
done
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

vectors="$module/../../extensions/issues/testdata/property-values.json"
issues_module="$module/../../extensions/issues/cue/cue.mod/module.cue"
checked=0
vectors_types="this module"
if [ -f "$vectors" ]; then
	types_source="$module"
	pinned="$(sed -n 's|.*"tartan.rawkode.academy/types@v0": *v: *"\(v[^"]*\)".*|\1|p' "$issues_module" 2>/dev/null | head -n 1)"
	if [ -n "$pinned" ] && [ -d "$module/testdata/$pinned" ]; then
		types_source="$module/testdata/$pinned"
		vectors_types="the $pinned fixture"
	fi
	work="$(mktemp -d)"
	trap 'rm -rf "$work"' EXIT
	cp -R "$types_source/cue.mod" "$types_source/types.cue" "$work/"
	mkdir "$work/vector"
	# check_vector <expectation> <property JSON> <value JSON>
	check_vector() {
		printf 'package vector\n\nimport "tartan.rawkode.academy/types"\n\np: types.#Property & %s & {default: %s}\n' "$2" "$3" >"$work/vector/v.cue"
		if (cd "$work" && "$cue" export ./vector/v.cue >/dev/null 2>&1); then
			result=valid
		else
			result=invalid
		fi
		if [ "$result" != "$1" ]; then
			echo "FAIL (vector should be $1): $2 default $3"
			failed=1
		fi
		checked=$((checked + 1))
	}
	# One NUL-separated triple per value: the expectation, the property and the
	# value, the last two as JSON (which never holds a raw newline).
	while IFS= read -r -d '' expectation && IFS= read -r -d '' property && IFS= read -r -d '' value; do
		check_vector "$expectation" "$property" "$value"
	done < <(jq -j '.[] | .property as $p | ((.valid[] | ["valid", $p, .]), (.invalid[] | ["invalid", $p, .])) | "\(.[0])\u0000\(.[1] | tojson)\u0000\(.[2] | tojson)\u0000"' "$vectors")
fi

valid="$(find testdata/valid -name '*.cue' | wc -l)"
invalid="$(find testdata/invalid -name '*.cue' | wc -l)"
if [ "$failed" -ne 0 ]; then
	exit 1
fi
echo "ok: $valid valid and $invalid invalid cases, $checked shared value vectors (against $vectors_types)"
