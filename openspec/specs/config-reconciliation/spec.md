# config-reconciliation Specification

## Purpose
Applies the `tartan.cue` committed in repositories to the forge: what each repository and org installs and how it is configured.

## Requirements

### Requirement: A push to main applies the repository's configuration
When a repository's `refs/heads/main` moves to a commit, the forge SHALL load the repository's configuration package (see `repo-config`) at that commit and apply it to the repository: each entry with a `version` installs that version of a `repo`-scoped package with its settings as configuration. When several pushes arrive, the latest commit's configuration SHALL win.

#### Scenario: Installing by push
- **WHEN** a push to `acme/api`'s `main` adds `extensions: "rawkode.issues": {version: "0.5.0", settings: fields: …}` to the root package `tartan`
- **THEN** Issues 0.5.0 is installed on `acme/api` with those fields, and the repository's log gains `extension.installed` and `config.applied`

#### Scenario: Pushing another branch
- **WHEN** a push changes the configuration only on `refs/heads/topic`
- **THEN** nothing is applied

### Requirement: The org's tartan repository configures the org
The repository named `tartan` in an org SHALL be the org's configuration: each entry with a `version` installs an `org`-scoped package on the org. An entry without a `version` there SHALL refuse the configuration.

#### Scenario: Org install
- **WHEN** `acme/tartan`'s `tartan.cue` names `rawkode.kanban` version `0.1.0`
- **THEN** Kanban is installed on `acme`

### Requirement: Configuration is applied all or nothing
Before changing anything, the forge SHALL check every entry: the version is published and the package's scope matches where the file lives. Invalid CUE or any failed check SHALL refuse the configuration, record `config.refused` with the problems and the commit, and leave every installation as it was.

#### Scenario: Unpublished version
- **WHEN** a pushed `tartan.cue` names `rawkode.issues` version `9.9.9`, which is not published
- **THEN** `config.refused` is recorded and the installed Issues is unchanged

#### Scenario: Invalid CUE
- **WHEN** a pushed `tartan.cue` does not evaluate
- **THEN** `config.refused` is recorded with the evaluation error

### Requirement: Each change is recorded
Applying a configuration SHALL record one event per installation it changes — `extension.installed`, `extension.upgraded` (a new version), `extension.configured` (new settings) or `extension.uninstalled` — and then `config.applied` with the commit and the changes. Applying an unchanged configuration SHALL record only `config.applied`.

#### Scenario: Settings change
- **WHEN** a push changes only Issues' settings
- **THEN** `extension.configured` and `config.applied` are recorded and the installation keeps its data

### Requirement: Repositories contribute settings to org installations
An entry without a `version` in a repository's `tartan.cue` SHALL contribute its settings to the org's installation of that extension. The org installation's configuration SHALL be its own settings plus `repos`: for each contributing repository, its id, path and settings. A contribution SHALL take effect when it changes and when the org installation is installed or reconfigured.

#### Scenario: Board contribution
- **WHEN** `acme/api`'s `tartan.cue` sets `extensions: "rawkode.kanban": settings: boards: api: …` and Kanban is installed on `acme`
- **THEN** Kanban's configuration includes `repos.<api-id>.settings.boards.api`
