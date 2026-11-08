# Spec Delta

## ADDED Requirements

### Requirement: Installations come only from configuration
An installation SHALL exist only because a committed `tartan.cue` names it (see `config-reconciliation`). `GET /-/api/orgs/<org>/extensions` and `GET /-/api/orgs/<org>/repos/<repo>/extensions` SHALL list the enabled installations.

#### Scenario: Listing
- **WHEN** a repository's configuration installs Issues
- **THEN** `GET …/extensions` lists `rawkode.issues` with its version

### Requirement: Uninstalling disables and keeps data
An installation its configuration no longer names SHALL be disabled: its subscriptions are removed, calls to it answer 404, and its data is kept. Naming it again SHALL enable the same installation with its data.

#### Scenario: Removed and restored
- **WHEN** Issues is removed from `tartan.cue` and later added back
- **THEN** while removed its tools answer 404, and once restored its earlier issues are there

### Requirement: Upgrading runs the new package's migrations
A new version SHALL run the new package's migrations not yet applied and call `init` again; a settings change alone SHALL do neither.

#### Scenario: Upgrade
- **WHEN** a configuration moves an installation to a new version
- **THEN** the new migrations run and `extension.upgraded` is recorded

## MODIFIED Requirements

### Requirement: Calling an installation
`POST …/extensions/<id>/tools/<tool>`, `…/render/<slot>` and `…/actions/<action>` SHALL call an enabled installation, acting as the caller. Extension errors SHALL map to HTTP statuses: `denied` 403, `not_found` 404, `invalid` 400, `conflict` 409, `unavailable` 503, otherwise 502, with the extension's message and kind. Calling an extension that is not installed or is disabled SHALL answer 404.

#### Scenario: Tool call
- **WHEN** Issues' `create` tool is called with a title
- **THEN** the issue is created in the installation's database and returned

#### Scenario: Extension refusal
- **WHEN** Issues' `create` is called without a title
- **THEN** the API answers 400 with kind `invalid`

#### Scenario: Not installed
- **WHEN** a tool of an extension that is not installed on the repository is called
- **THEN** the API answers 404

## REMOVED Requirements

### Requirement: Installing through the API
**Reason**: Installations and their settings come only from committed `tartan.cue`.
**Migration**: Name the extension, its version and its settings in the repository's `tartan.cue` (or the org's `tartan` repository for org installations) and push to `main`.
