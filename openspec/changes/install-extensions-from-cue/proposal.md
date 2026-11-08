# Proposal

## Why

Extensions are installed and configured through the HTTP API today, so a repository's behaviour is not described by what is committed in it. Configuration belongs in the repository as code: what an org or repository installs, and how each extension is set up (Issues fields, Kanban boards), must come from `tartan.cue` and change only by a push.

## What Changes

- **BREAKING** Remove the install API (`POST /-/api/orgs/<org>/extensions`, `POST /-/api/orgs/<org>/repos/<repo>/extensions`). Listing and calling installations stay.
- A push to a repository's `main` reconciles its installations with its root `tartan.cue`: entries with a `version` are installed, upgraded or reconfigured; installations no longer named are uninstalled (disabled, data kept, restored if named again).
- The org's configuration repository, `<org>/tartan`, does the same for org installations.
- A repository's entries without a `version` contribute settings to the org's installation of that extension; the org installation's configuration carries every repository's contribution.
- A configuration is applied all or nothing: an unpublished version, a package of the wrong scope or invalid CUE refuses it, records why, and changes nothing.
- Each outcome is an event: `config.applied` (with `extension.installed`, `.upgraded`, `.configured`, `.uninstalled`) or `config.refused`.
- **BREAKING** Kanban boards come from settings: the org's Kanban settings define boards of any scope, and a repository's `tartan.cue` contributes boards for itself and its projects. `create_board` and `delete_board` are removed; `list_boards`, `board` and `move` stay.
- Start the Nimbus documentation site in `docs/`, covering what exists and this change.

## Capabilities

### New Capabilities
- `config-reconciliation`: applying `tartan.cue` on push — repository and org configuration repositories, all-or-nothing reconciliation, contributions to org installations, and the events that record each outcome.

### Modified Capabilities
- `extension-installations`: installations come only from configuration; the install API is removed; uninstalling disables and keeps data.
- `kanban-extension`: boards are defined in settings (org settings and repository contributions) instead of through tools.

## Impact

- `tartan-forge`: repository and org actors reconcile on pushes; the extension actor applies upgrades, configuration changes and disabling; the org actor keeps contributions.
- `tartan`: the install routes are removed.
- `GitHost` reads a file at a commit (walgit's blob API).
- `extensions/kanban` reads boards from its configuration; its board tools change.
- Tests: the extension, subscription and Kanban end-to-end tests configure through pushed `tartan.cue`; the git test pushes a `tartan.cue` through walgit.
- New `docs/` Nimbus site (npm dependencies under `docs/` only).
