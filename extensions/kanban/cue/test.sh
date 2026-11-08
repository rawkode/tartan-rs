#!/usr/bin/env bash
# Checks Kanban's CUE module natively: a repository configuration that imports
# tartan.dev/types, Issues' module and Kanban's module from a registry evaluates
# with `cue export`, and boards that do not fit Issues' properties fail.
#
#   extensions/kanban/cue/test.sh <registry> [--publish]
#
# <registry> is a registry address in CUE_REGISTRY syntax without `+insecure`,
# such as localhost:5000; it is used over plain HTTP. With --publish the script
# first publishes the three modules from this repository's CUE sources
# (tartan.dev/types v0.1.0, Issues v0.6.0, Kanban v0.3.0) with `cue mod
# publish`: use it only with a throwaway registry, since a published version
# never changes and these modules hold no component. Without it the modules
# must already be in the registry, as scripts/build-extension.sh publishes them.
# ISSUES_PACKAGE and KANBAN_PACKAGE may name built package directories
# (tartan.json, component.wasm, migrations/) to publish inside the modules.
#
# Uses `cue` on PATH, or CUE_BIN.
set -euo pipefail

if [ $# -lt 1 ]; then
	echo "usage: $0 <registry> [--publish]" >&2
	exit 2
fi
registry="$1"
publish="${2:-}"
cue="${CUE_BIN:-cue}"
repo="$(cd "$(dirname "$0")/../../.." && pwd)"

work="$(mktemp -d)"
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
export CUE_REGISTRY="$registry+insecure"
export CUE_CACHE_DIR="$work/cache"
export CUE_CONFIG_DIR="$work/config"

# publish_module <module source> <version> [package dir]
publish_module() {
	local source="$1" version="$2" package="${3:-}" name
	name="$(basename "$(dirname "$source")")-$(basename "$source")"
	cp -R "$source" "$work/$name"
	if [ -n "$package" ]; then
		cp -R "$package"/. "$work/$name/"
	fi
	(cd "$work/$name" && "$cue" mod tidy && "$cue" mod publish "$version") >/dev/null
	echo "published $source $version${package:+ with $package}"
}

if [ "$publish" = "--publish" ]; then
	publish_module "$repo/cue/types" v0.1.0
	publish_module "$repo/extensions/issues/cue" v0.6.0 "${ISSUES_PACKAGE:-}"
	publish_module "$repo/extensions/kanban/cue" v0.3.0 "${KANBAN_PACKAGE:-}"
fi

consumer="$work/repo"
mkdir -p "$consumer/cue.mod"
cat >"$consumer/cue.mod/module.cue" <<'EOF'
module: "example.com/acme-api"
language: version: "v0.14.1"
deps: {
	"tartan.dev/types@v0": v: "v0.1.0"
	"github.com/rawkode/tartan-ext/issues@v0": v: "v0.6.0"
	"github.com/rawkode/tartan-ext/kanban@v0": v: "v0.3.0"
}
EOF

# The repository's Issues properties, shared by every case.
issues_entry='
let props = extensions."rawkode.issues".settings.properties
_props: props

extensions: "rawkode.issues": issues.#Extension & {settings: properties: {
	status:   types.#Select & {options: ["todo", "doing", "done"], labels: todo: "To do", default: "todo"}
	priority: types.#Select & {options: ["p1", "p2", "p3"]}
	tags:     types.#MultiSelect & {options: ["bug", "docs"]}
	owner:    types.#Person
	summary:  types.#ShortText & {maxLength: 80}
	notes:    types.#LongText
	budget:   types.#Money & {currency: "GBP"}
}}
'

# write_config <kanban entry>: the root package tartan with Issues and Kanban.
write_config() {
	cat >"$consumer/tartan.cue" <<EOF
package tartan

import (
	"tartan.dev/types"
	"github.com/rawkode/tartan-ext/issues"
	"github.com/rawkode/tartan-ext/kanban"
)

// Keeps every import and the props alias used whichever case is written.
_uses: [types.#Property, kanban.#Extension]
$issues_entry
$1
EOF
}

export_config() {
	(cd "$consumer" && "$cue" export --out json .)
}

failed=0
expect() {
	local what="$1" actual="$2" expected="$3"
	if [ "$actual" != "$expected" ]; then
		echo "FAIL: $what: got $actual, want $expected"
		failed=1
	fi
}

# The configuration from the design: it evaluates, and the exported value is
# what the forge applies.
write_config '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: {
		triage: {name: "Triage", columns: props.status, lanes: props.priority, group: "project"}
		lsp: {project: "crates/lsp", columns: {property: props.status, values: ["done", "todo"]}, lanes: props.tags, group: props.owner}
		open: {columns: "state", lanes: "project"}
	}
}
'
(cd "$consumer" && "$cue" mod tidy)
if ! exported="$(export_config 2>&1)"; then
	echo "FAIL: the valid configuration does not evaluate"
	echo "$exported" | sed 's/^/    /'
	exit 1
fi
q() { jq -c "$1" <<<"$exported"; }
expect "issues version" "$(q '.extensions."rawkode.issues".version')" '"0.6.0"'
expect "issues id" "$(q '.extensions."rawkode.issues".id')" '"rawkode.issues"'
expect "property name" "$(q '.extensions."rawkode.issues".settings.properties.status.name')" '"status"'
expect "kanban keys" "$(q '.extensions."rawkode.kanban" | keys')" '["id","settings","version"]'
expect "kanban version" "$(q '.extensions."rawkode.kanban".version')" '"0.3.0"'
expect "triage columns" "$(q '.extensions."rawkode.kanban".settings.boards.triage.columns | [.name, .kind, .options, .labels]')" '["status","select",["todo","doing","done"],{"todo":"To do"}]'
expect "triage lanes" "$(q '.extensions."rawkode.kanban".settings.boards.triage.lanes.name')" '"priority"'
expect "triage group" "$(q '.extensions."rawkode.kanban".settings.boards.triage.group')" '"project"'
expect "lsp columns" "$(q '.extensions."rawkode.kanban".settings.boards.lsp.columns | [.property.name, .values]')" '["status",["done","todo"]]'
expect "lsp lanes" "$(q '.extensions."rawkode.kanban".settings.boards.lsp.lanes.kind')" '"multiselect"'
expect "lsp group" "$(q '.extensions."rawkode.kanban".settings.boards.lsp.group.kind')" '"person"'
expect "open name" "$(q '.extensions."rawkode.kanban".settings.boards.open.name')" '"open"'
expect "open columns" "$(q '.extensions."rawkode.kanban".settings.boards.open.columns')" '"state"'

# refuse <case> <where> <kanban entry>: the configuration must fail evaluation,
# with an error at the path <where>.
refuse() {
	write_config "$3"
	if output="$(export_config 2>&1)"; then
		echo "FAIL (should be refused): $1"
		failed=1
	elif ! grep -qF -- "$2" <<<"$output"; then
		echo "FAIL (refused for another reason): $1"
		echo "$output" | sed 's/^/    /'
		failed=1
	else
		echo "ok: refused $1: $(head -n1 <<<"$output")"
	fi
}

refuse "a board without the Issues binding" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	settings: boards: triage: columns: props.status
}'
refuse "an inline select lookalike as columns" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: columns: types.#Select & {options: ["todo", "doing", "review", "done"]}
}'
refuse "a short text property as columns" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: columns: props.summary
}'
refuse "a multi-select property as columns" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: columns: props.tags
}'
refuse "a subset value that is not an option" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: columns: {property: props.status, values: ["todo", "blocked"]}
}'
refuse "an empty subset" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: columns: {property: props.status, values: []}
}'
refuse "lanes by a short text property" 'boards.triage.lanes' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: {columns: props.status, lanes: props.summary}
}'
refuse "a group by money" 'boards.triage.group' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: {columns: props.status, group: props.budget}
}'
refuse "a board without columns" 'boards.triage.columns' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: name: "Triage"
}'
refuse "an invalid board id" 'boards.Triage' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: Triage: columns: "state"
}'
refuse "an unknown board key" 'boards.triage.field' '
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: {columns: "state", field: "status"}
}'
refuse "another version of Kanban" '"rawkode.kanban".version' '
extensions: "rawkode.kanban": kanban.#Extension & {
	version: "0.2.0"
	settings: boards: triage: columns: "state"
}'

# Issues settings that do not fit Issues' module.
issues_entry='
extensions: "rawkode.issues": issues.#Extension & {settings: properties: status: {kind: "colour"}}
'
refuse "an Issues property of an unknown kind" 'properties.status' ''
issues_entry='
extensions: "rawkode.issues": issues.#Extension & {settings: properties: Status: types.#Boolean}
'
refuse "an Issues property with an invalid name" 'properties.Status' ''
issues_entry='
extensions: "rawkode.issues": issues.#Extension & {settings: properties: status: types.#Select & {options: ["a"], name: "other"}}
'
refuse "an Issues property naming another" 'properties.status.name' ''

if [ "$failed" -ne 0 ]; then
	exit 1
fi
echo "ok: Kanban's module evaluates natively and refuses boards that do not fit"
