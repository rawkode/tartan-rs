# Spec Delta

## MODIFIED Requirements

### Requirement: The configuration file and its schema
A repository's configuration SHALL be the CUE package `tartan` in its root directory: every root `*.cue` file declaring `package tartan`, evaluated together against the forge's schema, where `extensions` maps an extension id to an entry with an optional semver `version` and free-form `settings`. Root files of other packages SHALL be ignored. Any other key in an entry, an invalid id or version, a conflict or a syntax error SHALL make the configuration invalid. A root with no `package tartan` file SHALL be the empty configuration.

#### Scenario: Install and contribution
- **WHEN** the root package `tartan` sets `extensions: "rawkode.issues": {version: "0.5.0", settings: …}` and `extensions: "rawkode.kanban": settings: …`
- **THEN** the configuration installs Issues 0.5.0 with those settings and contributes settings to Kanban

#### Scenario: Invalid entry
- **WHEN** an entry has `version: "latest"` or an unknown key
- **THEN** the configuration is invalid

#### Scenario: Several files
- **WHEN** `issues.cue` and `kanban.cue` in the root both declare `package tartan`, one for each extension
- **THEN** the configuration is both files together

#### Scenario: Another package in the root
- **WHEN** the root also holds `env.cue` declaring `package cuenv`
- **THEN** it is not part of the configuration

## ADDED Requirements

### Requirement: Modules and imports
When the repository has `cue.mod/module.cue`, its module path and language version SHALL be used, its `cue.mod/` contents SHALL be loaded, and imports of packages inside the module SHALL be loaded from the same commit. Imports of other modules SHALL resolve through the forge's CUE registry configuration. Without `cue.mod/module.cue`, the configuration SHALL be evaluated as a module of its own.

#### Scenario: Importing a package of the repository
- **WHEN** the root package imports `example.com/repo/config/fields` and `config/fields/` defines the field definitions
- **THEN** the configuration uses them

### Requirement: Loading is bounded
Loading a configuration SHALL read at most 500 files, 64 directories and 4 MiB of CUE; a configuration needing more SHALL be invalid.

#### Scenario: Too large
- **WHEN** the root package and its imports exceed 4 MiB
- **THEN** the configuration is refused with a problem naming the limit
