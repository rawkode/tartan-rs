# Design

## Context

Today `tartan-config` writes the repository's CUE files plus a forge schema file (and a default `cue.mod/module.cue` when there is none) to a directory and evaluates package `tartan` with cuengine. cuengine resolves module dependencies through `modconfig` (`CUE_REGISTRY`). Org installations, the org's `tartan` repository and contributions exist. CUE 0.17 resolves `import "tartan.dev/types"` from any registry module a `cue.mod/module.cue` depends on.

## Goals / Non-Goals

**Goals:**
- The golden rule: the configuration the forge applies is exactly `cue export` of the repository's root package `tartan`, run with the same registry configuration.
- Typed properties and boards whose references are checked by CUE itself.

**Non-Goals:**
- Running or choosing a registry: any OCI registry works; the operator configures `CUE_REGISTRY`.
- Configuration shared across repositories (other than through published CUE modules a repository imports).
- Checking `#Person` values against identities (none exist yet).

## Example

```cue
// cue.mod/module.cue
module: "example.com/acme-api"
language: version: "v0.14.1"
deps: {
	"tartan.dev/types@v0":       v: "v0.1.0"
	"tartan.dev/ext/issues@v0":  v: "v0.6.0"
	"tartan.dev/ext/kanban@v0":  v: "v0.3.0"
}

// tartan.cue
package tartan

import (
	"tartan.dev/types"
	"tartan.dev/ext/issues"
	"tartan.dev/ext/kanban"
)

let props = extensions."rawkode.issues".settings.properties

extensions: "rawkode.issues": issues.#Extension & {settings: properties: {
	status:   types.#Select & {options: ["todo", "doing", "done"], labels: todo: "To do", default: "todo"}
	priority: types.#Select & {options: ["p1", "p2", "p3"]}
	notes:    types.#LongText
}}
extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
	settings: boards: triage: {name: "Triage", columns: props.status, lanes: props.priority, group: "project"}
}
```

## Decisions

- **Evaluate exactly what is committed.** The forge loads the package's files as today and evaluates them unchanged; it adds no schema and no module file. A configuration that imports modules needs its own `cue.mod/module.cue`, as it would locally. After evaluation, Rust checks the shape: each `extensions` key is an id, each entry has a semver `version` and object `settings`. Alternative: keep the injected schema — refused by the golden rule.
- **Registry configuration is the operator's.** The gateway process's `CUE_REGISTRY` (and the CUE config it points to) is what cuengine uses, so `CUE_REGISTRY=… cue export` on a laptop gives the same result. The forge never rewrites module paths.
- **`#Extension` fixes id and version.** Each extension module defines `#Extension: {version: "<its version>", settings: #Settings}`; the module's published version and the WASM package's version are released together (`scripts/publish-cue-modules.sh` reads the manifest version). The manifest's `config.module` names the module so documentation and tooling can point users to it.
- **Kanban depends on Issues through its module.** Kanban's module imports `tartan.dev/ext/issues` and declares `#issues: issues.#Extension`; boards check `columns: #issues.settings.properties[columns.name]` (or a `{property, values}` subset, or `"state"`), so a lookalike or misspelt property fails natively. The repository binds `#issues: extensions."rawkode.issues"` — explicit, and evaluable by `cue eval`. At reconciliation the forge also refuses Kanban without Issues installed, which the manifest's `requires` declares.
- **Repository only, by removal.** Org installations, the org configuration repository, contributions, `RecipientScope::Org`, org extension routes and org `FlushTree` go; the code and specs describe only what exists.
- **Kanban views.** A view is lanes × columns, each cell holding groups. A multi-select lane or group lists a card under each value; cards without a value go under a final "none". Titles come from `labels`, falling back to values.
- **Test registry.** Development and tests run CNCF `distribution` on a local port; `scripts/publish-cue-modules.sh` publishes the types, Issues and Kanban modules to the registry named by `CUE_REGISTRY` with `cue mod publish`.

## Risks / Trade-offs

- [The forge's evaluation now needs the registry reachable] → An unreachable registry refuses the configuration and nothing changes; the error says which module could not be fetched.
- [Breaking: org boards across repositories are gone] → Accepted by the golden rule; cross-repository views can return later as something CUE can evaluate natively (for example a module repositories publish and an org repository imports).
- [Module and package versions released together] → One script publishes both from the manifest version; a mismatch is refused at reconciliation because `#Extension`'s version must name a published package.
