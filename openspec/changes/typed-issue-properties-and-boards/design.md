# Design

## Context

`tartan-config` loads the root package `tartan` and evaluates it once against the forge's schema with cuengine. Settings are free-form JSON. Issues validates its fields in Rust from `fields`; Kanban reads `boards` and names its field by string. The org's `tartan` repository refuses entries without a version. CUE 0.17 resolves a package placed under `cue.mod/pkg/<import path>` in a module, so the forge can inject packages without a registry.

## Goals / Non-Goals

**Goals:**
- Typed, referential configuration: a board's columns, lanes and groups are CUE references to Issues properties, checked when the configuration is evaluated.
- One definition of org-wide properties, usable by every repository and every org board.

**Non-Goals:**
- Identity for `#Person` values (they are principal ids; checking that a person exists waits for identity).
- Per-project properties, and boards configured anywhere but the org's and repositories' configuration.

## Example

```cue
// acme/tartan
package tartan

import "tartan.dev/types"

let issues = extensions."rawkode.issues".settings.properties

extensions: "rawkode.issues": settings: properties: {
	status:   types.#Select & {options: ["todo", "doing", "done"], labels: {todo: "To do"}, default: "todo"}
	priority: types.#Select & {options: ["p1", "p2", "p3"]}
	assignee: types.#Person
	notes:    types.#LongText
}
extensions: "rawkode.kanban": {
	version: "0.3.0"
	settings: boards: triage: {
		name:    "Triage"
		columns: issues.status
		lanes:   issues.priority
		group:   "repository"
	}
}
```

## Decisions

- **Two evaluation passes.** Pass one evaluates the package as today, which tells which extensions are named and at which versions. Pass two adds, under `cue.mod/pkg`, the types package and each named extension's schema (from its published package), plus a generated binding file in the root that unifies each entry's `settings` with its `#Settings` and gives each schema its dependencies (`#issues: extensions."rawkode.issues".settings`), and for a repository the org-wide settings of the repository extensions it installs. Pass two's result is the configuration. Alternative: one pass with every published schema — loads schemas no configuration uses and cannot know versions.
- **Which schema version.** An entry with a version uses that package's schema. A version-less entry uses, in a repository, the version the org installs (for a contribution) and, in the org's repository, the latest published version (for org-wide settings of a repository extension).
- **Properties carry their names.** `properties: [N=string]: types.#Property & {name: N}` makes a reference self-describing, so Kanban stores `columns.name` and `columns.options` from a reference, and its schema checks `columns: #issues.properties[columns.name]` — the reference must be the Issues property, not a lookalike.
- **Org-wide settings are unified, not merged.** A repository's evaluation unifies the org's settings for an extension with its own, so redefining an org property differently is a CUE conflict and refuses the repository's configuration; adding properties is allowed. When the org's settings change, the org actor asks each repository to reapply its latest configuration.
- **Issues validates in Rust too.** The schema stops bad configuration; Issues still checks every value it stores against the property's type.
- **Kanban views.** A board view is lanes × columns, each cell holding groups of cards. A multi-select lane or group shows a card under each of its values; a card without a value goes under a final "none" lane or group. Lanes and groups by `repository` or `project` use the card's repository path and project.

## Risks / Trade-offs

- [Two passes double evaluation time] → Evaluations take milliseconds; schemas are cached with their packages.
- [Org-wide settings validated against the latest published Issues may differ from an older installed version] → Each repository's pass two checks them against its installed version, and refuses there if they no longer fit.
- [Breaking renames (fields → properties, monetary → money)] → No deployments exist; the docs and tests move together.
