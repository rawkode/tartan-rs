# Proposal

## Why

Issues fields and Kanban boards are configured in CUE but untyped: a board names its field by a string and nothing checks it. And the forge's evaluation is not what a person gets from `cue eval`: it injects a schema file and a default module, and org-wide configuration would inject settings from another repository. The golden rule is that CUE always evaluates natively — the forge applies exactly what `cue eval` of the repository gives, with real imports of real modules — so typing and references between extensions must come from CUE modules the repository imports, and configuration that cannot be evaluated natively (org-wide settings, contributions across repositories) goes.

## What Changes

- **Native evaluation.** The forge evaluates the repository's root package `tartan` exactly as committed, resolving module dependencies through the CUE registry configuration (`CUE_REGISTRY`) the operator gives it, as any CUE user does. It adds no files. It checks the exported value's shape (extension ids, versions) in Rust afterwards.
- **Extension modules.** An extension's settings schema is a CUE module published to any OCI registry; the manifest names it (`config.module`). The module defines `#Extension`, which fixes the extension's id and version and types its settings, so the module version a repository depends on is the version it installs.
- **The types module.** `tartan.dev/types` is a real CUE module (source in this repository) with `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person`, `#URL`, `#Money` and `#Property`.
- **BREAKING Repository only.** Org installations, the org's `tartan` configuration repository, contributions to org installations and org-wide event scope are removed. Every extension installs on a repository.
- **BREAKING Issues 0.6.0.** Fields become typed *properties* (`settings.properties`), adding long text, person, URL and money (from monetary). Its module is `tartan.dev/ext/issues`.
- **BREAKING Kanban 0.3.0.** Repository-scoped, with boards for the repository or one of its projects. Its module imports Issues' module — the dependency is a real module dependency — and binds the repository's Issues entry (`#issues`), so a board's `columns` must be one of that Issues' select properties (all options, or a subset and order), or `"state"`; optional `lanes` (rows) and `group` (headed groups within each cell) reference a select, multi-select or person property, or `"project"`. Titles come from option labels.

## Capabilities

### New Capabilities
- `config-types`: the `tartan.dev/types` module and its property types.
- `extension-config-schemas`: extension settings as CUE modules in any OCI registry, and native evaluation of a repository's configuration against them.

### Modified Capabilities
- `repo-config`: native evaluation — no added files, registry from configuration, shape checked after evaluation; entries need versions.
- `config-reconciliation`: the org's configuration repository and contributions are removed.
- `extension-packages`: every package installs on repositories and names its CUE module.
- `extension-installations`: listing and calling are per repository only.
- `extension-events`: an installation receives its repository's events only.
- `extension-calls`: calls stay inside the caller's repository.
- `event-delivery`: new recipients' history replay applies to repository installations.
- `issues-extension`: typed properties and the new types.
- `kanban-extension`: repository boards with columns, lanes and groups referencing Issues properties.

## Impact

- `tartan-config`: evaluation without added files; `CUE_REGISTRY` passed through; shape check in Rust.
- `tartan-forge`: removal of org installations, the org configuration path, contributions and org recipients; manifests' `config.module`.
- `tartan`: org extension routes removed.
- `cue/types`, `extensions/issues/cue`, `extensions/kanban/cue`: CUE modules, published by `scripts/publish-cue-modules.sh` to the registry in `CUE_REGISTRY`.
- Development and tests run an OCI registry (CNCF `distribution`) and publish the modules to it.
- Docs: Configuration, Issues, Kanban, a types page, and running locally with a registry.
