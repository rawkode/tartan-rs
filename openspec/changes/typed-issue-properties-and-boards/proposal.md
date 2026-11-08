# Proposal

## Why

Issues fields and Kanban boards are configured in CUE, but as untyped settings: a board names its field by a string, nothing checks that it exists or is a select, and every repository defines its own fields, so an org-wide board cannot rely on them. Configuration should be typed and referential: issue properties declared with types, boards built from references to those properties, and mistakes refused when the configuration is evaluated rather than discovered on the board.

## What Changes

- The forge provides a CUE types package, `tartan.dev/types`, that every configuration can import: `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person`, `#URL`, `#Money`, and `#Property` for any of them.
- An extension package may ship a settings schema (`config.schema` in its manifest). The forge evaluates each configured extension's settings against its schema, so invalid settings refuse the configuration.
- An extension may declare that it depends on another (`config.requires`); its schema then sees that extension's settings (as `#<alias>`), and configuring it without the dependency refuses the configuration.
- **BREAKING** Issues 0.6.0 calls fields *properties* (`settings.properties`), typed with `tartan.dev/types`, and adds long text (Markdown), person, URL and money (renamed from monetary).
- The org's `tartan` repository can set an extension's settings without a version: these are org-wide settings for every repository's installation of a repository extension. For Issues, they are the org's properties; a repository's own `properties` add to them and cannot redefine them.
- **BREAKING** Kanban 0.3.0 depends on Issues. A board's `columns` is a reference to a select property (optionally a subset and order of its options), or the issue `state`; `lanes` (rows) and `group` (headed groups within each cell) are optional references to a select, multi-select or person property, or `repository`/`project`. Column, lane and group titles come from the property's option labels.

## Capabilities

### New Capabilities
- `config-types`: the forge's CUE types package and the typed property definitions.
- `extension-config-schemas`: extension settings schemas, dependencies between extensions' settings, and org-wide settings for repository extensions.

### Modified Capabilities
- `repo-config`: configurations can import `tartan.dev/types`; each extension's settings are checked against its schema.
- `config-reconciliation`: the org's `tartan` repository may give version-less settings for repository extensions; changing them reapplies every repository's configuration.
- `extension-packages`: a manifest may name a settings schema and the extensions its settings require.
- `issues-extension`: typed properties, org-wide and repository properties, the new types.
- `kanban-extension`: boards reference Issues properties for columns, swim lanes and groups; Kanban requires Issues.

## Impact

- `tartan-config`: a second evaluation pass with the types package, extension schemas, a generated binding and org-wide settings.
- `tartan-forge`: packages keep their schema; repository reconciliation includes org-wide settings; the org actor stores them and reapplies repositories when they change.
- `extensions/issues` (0.6.0) and `extensions/kanban` (0.3.0): schemas, property types, board views with lanes and groups.
- Docs: Configuration, Issues and Kanban pages; a page for the types package.
