# Spec Delta

## REMOVED Requirements

### Requirement: The configuration schema
**Reason**: The forge no longer adds a schema to the evaluation (the golden rule); it checks the exported value instead, and entries without a version (contributions) no longer exist.
**Migration**: See "The exported configuration is checked"; give every entry a version, usually through the extension module's `#Extension`.

## ADDED Requirements

### Requirement: The exported configuration is checked
After evaluation, the forge SHALL check the exported value: `extensions` maps an extension id to an entry with a semver `version` and object `settings`. Any other key in an entry, an invalid id, a missing or invalid version, a conflict or a syntax error SHALL make the configuration invalid.

#### Scenario: Install
- **WHEN** the root package `tartan` sets `extensions: "rawkode.issues": issues.#Extension & {settings: …}`
- **THEN** the configuration installs Issues at the module's version with those settings

#### Scenario: Invalid entry
- **WHEN** an entry has `version: "latest"`, no version, or an unknown key
- **THEN** the configuration is invalid

## MODIFIED Requirements

### Requirement: Modules and imports
The repository's `cue.mod/` SHALL be used as committed: imports of packages inside the module SHALL be loaded from the same commit and dependencies on other modules SHALL resolve through the forge's CUE registry configuration. A configuration without `cue.mod/module.cue` SHALL be evaluated as `cue` evaluates a directory without a module.

#### Scenario: Importing a package of the repository
- **WHEN** the root package imports `example.com/repo/config/fields` and `config/fields/` defines the field definitions
- **THEN** the configuration uses them
