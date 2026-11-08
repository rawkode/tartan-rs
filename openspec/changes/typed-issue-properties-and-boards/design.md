# Design

## Context

Today packages are uploaded through `POST /-/api/packages` into object storage, and `tartan-config` evaluates a repository's CUE plus an injected schema file and default module. cuengine resolves module dependencies through `CUE_REGISTRY`. CUE resolves every module through one registry setting — there is no discovery from a module path's host — so a person's `cue` and the forge agree only if they use the same registry. Verified with CUE 0.17.1 and CNCF `distribution`: a module published with `source: kind: "self"` carries non-CUE files (`component.wasm`, `migrations/`), imports natively, and its files are present in the fetched module.

## Goals / Non-Goals

**Goals:**
- The golden rule: the forge applies exactly `cue export` of the repository's root package `tartan`, run against the forge's registry.
- One artifact per extension version, available because the deployment says so.
- Typed properties and boards whose references CUE itself checks.

**Non-Goals:**
- Pushing to the forge's registry from outside (only the forge's sync writes).
- Configuration shared across repositories other than through modules a repository imports.
- Checking `#Person` values against identities (none exist yet).

## Example

```cue
// /etc/tartan/forge.cue (the deployment's config map)
package forge

sources: "github.com/rawkode": "ghcr.io/rawkode/cue"
modules: [
	"tartan.dev/types@v0.1.0",
	"github.com/rawkode/tartan-ext/issues@v0.6.0",
	"github.com/rawkode/tartan-ext/kanban@v0.3.0",
]
```

```cue
// a repository: cue.mod/module.cue
module: "example.com/acme-api"
language: version: "v0.14.1"
deps: {
	"tartan.dev/types@v0":                          v: "v0.1.0"
	"github.com/rawkode/tartan-ext/issues@v0":      v: "v0.6.0"
	"github.com/rawkode/tartan-ext/kanban@v0":      v: "v0.3.0"
}

// tartan.cue
package tartan

import (
	"tartan.dev/types"
	"github.com/rawkode/tartan-ext/issues"
	"github.com/rawkode/tartan-ext/kanban"
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

- **The registry is served by the gateway, read only.** CUE needs only the OCI Distribution read API: `GET /v2/`, `GET`/`HEAD /v2/<name>/manifests/<ref>`, `GET`/`HEAD /v2/<name>/blobs/<digest>` and `GET /v2/<name>/tags/list`. The gateway implements these endpoints itself, with `oci-spec` for the manifest and descriptor types, and serves content from the object store (`registry/blobs/sha256/<hex>`, `registry/manifests/<name>/<tag>`). The only writer is the forge's own sync, so there is no push API, upload session or garbage collection. Alternatives:
  - CNCF `distribution` or angos as a separate process — angos fits well (S3 storage, pull-through cache, CEL access policies) but is a second process, against a single-binary forge.
  - Trow embedded (`build_app()` gives an axum router) — its `sqlx` links `libsqlite3-sys` 0.30, which cannot coexist with Rivet SQLite's `rusqlite` 0.37 (0.35), and it stores on the local filesystem.
  - angos's protocol crates (`angos-oci`, `angos-oci-service`) — not published on crates.io.
  - `ferro-oci-server` (months old, metadata in memory); `container-registry` (unmaintained since 2024).
- **Deployment configuration in CUE.** `TARTAN_CONFIG` names a CUE file or directory (package `forge`), evaluated natively with cuengine and no registry: `modules` (fully qualified `path@version`) and `sources` (module path prefix → OCI registry and repository prefix, in `CUE_REGISTRY` syntax). Other settings now in environment variables can move there later.
- **Sync at startup, idempotent.** For each listed module the forge resolves its source, pulls the manifest and blobs with ORAS's `oci-client` and writes them to the object store unless the registry already has that version with the same digest, and — when the module has a `tartan.json` — reads the manifest, component and migrations out of the module zip and registers the package (`id@version` and digest). A version whose digest changed is refused, keeping `id@version` immutable. Modules dropped from the list stay in the registry but their packages stop being available: configurations naming them are refused; existing installations keep running until their repository's configuration changes.
- **`#Extension` fixes id and version.** The module's schema sets `version` to the module's version; the forge checks, when registering, that the module version and `tartan.json`'s version agree.
- **Evaluate exactly what is committed.** No schema or module file is added; the forge's process environment sets `CUE_REGISTRY` to its own registry for cuengine. After evaluation Rust checks the shape: ids, semver versions, object settings.
- **Kanban depends on Issues through its module.** Kanban's module imports Issues' and declares `#issues: issues.#Extension`; boards check `columns: #issues.settings.properties[columns.name]` (or a `{property, values}` subset, or `"state"`), so lookalikes and typos fail natively. Kanban's `tartan.json` `requires` `rawkode.issues`, which reconciliation enforces.
- **Repository only, by removal**, as before: org installations, org configuration repository, contributions, `RecipientScope::Org`, org routes.
- **Kanban views.** Lanes × columns, each cell holding groups; multi-select under each value; "none" last; titles from labels.

## Risks / Trade-offs

- [The forge depends on its registry for every evaluation] → It runs beside the forge; an unreachable registry refuses the configuration and nothing changes.
- [Source registries unreachable at startup] → Modules already in the forge's registry keep working; the sync reports what it could not copy and retries on the next start.
- [Breaking: the publish API and org boards are gone] → No deployments exist; the deployment configuration replaces publishing.
