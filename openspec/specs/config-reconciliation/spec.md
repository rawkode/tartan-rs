# config-reconciliation Specification

## Purpose
Applies the configuration committed in each repository (its root package `tartan`) to the forge: what the repository installs and how each extension is configured.

## Requirements

### Requirement: A push to main applies the repository's configuration
When a repository's `refs/heads/main` moves to a commit, the forge SHALL load the repository's configuration package (see `repo-config`) at that commit and apply it to the repository: each entry with a `version` installs that version of a `repo`-scoped package with its settings as configuration. When several pushes arrive, the latest commit's configuration SHALL win.

#### Scenario: Installing by push
- **WHEN** a push to `acme/api`'s `main` adds `extensions: "rawkode.issues": issues.#Extension & {settings: properties: …}` to the root package `tartan`, with Issues' module at `v0.6.0`
- **THEN** Issues 0.6.0 is installed on `acme/api` with those properties, and the repository's log gains `extension.installed` and `config.applied`

#### Scenario: Pushing another branch
- **WHEN** a push changes the configuration only on `refs/heads/topic`
- **THEN** nothing is applied

### Requirement: Configuration is checked before it is applied
Before changing anything, the forge SHALL check every entry: its version's package is in a module the deployment lists, and the extensions its manifest `requires` are installed by the same configuration. Invalid CUE or any failed check SHALL refuse the configuration, record `config.refused` with the problems and the commit, and leave every installation as it was.

#### Scenario: Unpublished version
- **WHEN** a pushed configuration names `rawkode.issues` version `9.9.9`, which no listed module holds
- **THEN** `config.refused` is recorded and the installed Issues is unchanged

#### Scenario: Invalid CUE
- **WHEN** a pushed configuration does not evaluate
- **THEN** `config.refused` is recorded with the evaluation error

### Requirement: Required extensions are initialised first
After the checks pass, the forge SHALL initialise each extension in turn, the extensions a package requires before it. When one fails, the installations already changed SHALL stay changed, nothing SHALL be recorded, and the configuration SHALL be applied again later.

#### Scenario: Kanban beside Issues
- **WHEN** a configuration installs Issues and Kanban, which requires it
- **THEN** Issues is initialised before Kanban

### Requirement: Each change is recorded
Applying a configuration SHALL record one event per installation it changes — `extension.installed`, `extension.upgraded` (a new version), `extension.configured` (new settings) or `extension.uninstalled` — and then `config.applied` with the commit and the changes. Applying an unchanged configuration SHALL record only `config.applied`.

#### Scenario: Settings change
- **WHEN** a push changes only Issues' settings
- **THEN** `extension.configured` and `config.applied` are recorded and the installation keeps its data

### Requirement: Failures that may pass are retried
A configuration that cannot be evaluated because the Git host or the forge's registry failed, or whose application failed, SHALL stay unapplied and SHALL be tried again with a growing delay, and again when the repository actor starts or `main` moves; it SHALL NOT be refused for that reason. Configurations SHALL be applied only once the forge's module sync at startup has finished.

#### Scenario: Pushed during the sync
- **WHEN** a configuration depending on a newly listed module is pushed while the forge is still copying that module
- **THEN** it is applied once the sync has finished, not refused
