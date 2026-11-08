// Package kanban is the settings schema of Kanban (`rawkode.kanban`).
//
// Kanban shows a repository's issues on boards. Its boards refer to the
// properties of the repository's Issues installation, which the repository
// binds as #issues:
//
//	let props = extensions."rawkode.issues".settings.properties
//
//	extensions: "rawkode.kanban": kanban.#Extension & {
//		#issues: extensions."rawkode.issues"
//		settings: boards: triage: {columns: props.status, lanes: props.priority}
//	}
//
// A board's columns must be one of those select properties, so a typo, an
// inline select whose options differ, or a property of another kind fails
// evaluation instead of showing an empty board.
package kanban

import (
	"list"
	"strings"

	"github.com/rawkode/tartan-ext/issues"
)

// An installation of Kanban on a repository.
#Extension: {
	id:      "rawkode.kanban"
	version: "0.3.0"

	// The repository's Issues installation, whose properties boards use.
	#issues: issues.#Extension

	settings: #Settings & {#properties: #issues.settings.properties}
}

// Kanban's settings, given the Issues properties boards may use.
#Settings: {
	// Issues' properties, by name.
	#properties: issues.#Settings.properties

	let selects = [for _, p in #properties if p.kind == "select" {p}]
	let groupable = [for _, p in #properties if list.Contains(["select", "multiselect", "person"], p.kind) {p}]

	// The boards, by id.
	boards: [ID=#BoardID]: #Board & {
		name:       *ID | _
		#selects:   selects
		#groupable: groupable
	}
}

// A board's id: 1 to 64 lowercase letters, digits and `-`.
#BoardID: =~"^[a-z0-9-]{1,64}$"

// A board: a view of the repository's issues, or of one project's.
#Board: {
	#selects: [...]
	#groupable: [...]

	// The title people see; the board's id by default.
	name: string & strings.MinRunes(1) & strings.MaxRunes(100)

	// Only the issues filed against this project (its root path, such as
	// `crates/lsp`).
	project?: =~"^[^/]+(/[^/]+)*$" & strings.MaxRunes(512)

	// The columns: one of Issues' select properties, with a column for each
	// option in order; some of its options in a chosen order
	// (`{property, values}`); or "state", the issue's open or closed.
	columns: "state" | or(#selects) | #Subset & {property: or(#selects)}

	// Rows: one per value of a select, multi-select or person property, or
	// one per project.
	lanes?: "project" | or(#groupable)

	// Headed groups inside each cell, by the same kinds of values as lanes.
	group?: "project" | or(#groupable)
}

// Some options of a select property, in the order the columns show them.
#Subset: {
	property: {options: [...string], ...}
	values: [_, ...or(property.options)] & list.UniqueItems()
}
