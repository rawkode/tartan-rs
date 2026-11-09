# Proposal

## Why

Issues fields and Kanban boards are configured in CUE but untyped: a board names its field by a string and nothing checks it. And the forge's evaluation is not what a person gets from `cue eval`: it injects a schema file and a default module. The golden rule is that CUE always evaluates natively — the forge applies exactly what `cue eval` of the repository gives, with real imports of real modules — so typing and references between extensions must come from CUE modules the repository imports, and from a registry that a person's `cue` and the forge both use. Tartan becomes that registry, and its deployment configuration decides which extensions it makes available.

## What Changes

- **Tartan is a CUE registry.** The gateway serves the OCI Distribution read API at `/v2/` from the object store, so the forge stays one binary. Users set `CUE_REGISTRY=<forge>`; the forge evaluates against the same registry, so `cue eval` and the forge agree.
- **An extension is one CUE module.** Its settings schema (`#Extension`, which fixes its id and version and types its settings) and its package — `tartan.json`, `component.wasm`, migrations — are one module, published with `cue mod publish`. Module paths are fully qualified (`github.com/rawkode/tartan-ext/issues@v0`).
- **The deployment configuration makes modules available.** A CUE file mounted from the deployment's config map (`TARTAN_CONFIG`) lists module versions and where to fetch them. At startup the forge copies each into its registry and registers the extension packages among them. **BREAKING** `POST /-/api/packages` is removed.
- **Native evaluation.** The forge evaluates a repository's root package `tartan` exactly as committed, with its own registry; it adds no files and checks the exported value's shape in Rust afterwards.
- **The types module** (`tartan.dev/types` in this repository, published like any module): `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person`, `#URL`, `#Money`, `#Property`.
- **BREAKING Repository only.** Org installations, the org's `tartan` configuration repository, contributions and org-wide event scope are removed.
- **BREAKING Issues 0.6.0.** Typed *properties* (`settings.properties`), adding long text, person, URL and money.
- **BREAKING Kanban 0.3.0.** Repository-scoped boards for the repository or a project. Its module imports Issues' module and binds the repository's Issues entry (`#issues`), so `columns` must be one of its select properties (all options, or a subset and order) or `"state"`; optional `lanes` (rows) and `group` (headed groups within each cell) reference a select, multi-select or person property, or `"project"`. Titles come from option labels.

## Capabilities

### New Capabilities
- `forge-cue-registry`: the forge's registry, and the deployment configuration that fills it.
- `config-types`: the types module.
- `extension-config-schemas`: an extension as one CUE module, and native evaluation of repositories against modules.

### Modified Capabilities
- `extension-packages`: packages come from modules the deployment makes available; the publish API is removed; every package installs on repositories.
- `repo-config`: native evaluation; the exported value's shape is checked; entries need versions.
- `config-reconciliation`: the org's configuration repository and contributions are removed.
- `extension-installations`, `extension-events`, `extension-calls`, `event-delivery`: repository only.
- `issues-extension`: typed properties and the new types.
- `kanban-extension`: repository boards with columns, lanes and groups referencing Issues properties.

## Impact

- New: the forge configuration file (`TARTAN_CONFIG`), a registry sync at startup (`oci-client` pulling modules into the object store), `/v2/` read endpoints in the gateway.
- `tartan-config`: evaluation without added files, against the forge's registry; shape check in Rust.
- `tartan-forge`: removal of org installations, the org configuration path, contributions and org recipients; packages read from modules.
- `tartan`: the publish route and org extension routes removed.
- `extensions/*` and `cue/types` become CUE modules; `scripts/build-extension.sh` builds the module directory; tests publish modules to a source registry (CNCF `distribution`, test-only) for the forge to sync from.
- Docs: Running locally and Deploying (the configuration file), Configuration, Extensions (authoring a module), Issues, Kanban, the types module.
