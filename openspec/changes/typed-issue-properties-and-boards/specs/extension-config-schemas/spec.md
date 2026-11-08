# Spec Delta

## Purpose
Extension settings are typed by CUE modules in any OCI registry, which repositories import, so configuration evaluates natively.

## ADDED Requirements

### Requirement: Extensions publish a CUE module
An extension's settings schema SHALL be a CUE module in an OCI registry defining `#Extension`, which fixes the extension's `version` and types its `settings`. The module version SHALL equal the extension package's version.

#### Scenario: Version from the module
- **WHEN** a repository sets `extensions: "rawkode.issues": issues.#Extension & {settings: …}` with `tartan.dev/ext/issues@v0` at `v0.6.0`
- **THEN** the configuration installs Issues 0.6.0

### Requirement: Configuration evaluates natively
The forge SHALL evaluate a repository's configuration exactly as committed, adding no files, with the CUE registry configuration it is run with (`CUE_REGISTRY`), so `cue export` of the root package `tartan` with the same registry gives the configuration the forge applies. A module the registry cannot provide SHALL make the configuration invalid.

#### Scenario: Same as cue export
- **WHEN** a repository's configuration imports Issues' and Kanban's modules
- **THEN** the forge applies the value `cue export` gives for package `tartan`

#### Scenario: Unreachable module
- **WHEN** a configuration depends on a module version the registry does not have
- **THEN** the configuration is refused, naming the module

### Requirement: Settings that do not fit are refused
Settings that do not fit their extension's `#Extension` SHALL fail evaluation and refuse the configuration.

#### Scenario: Invalid settings
- **WHEN** Issues' settings give a property an unknown `kind`
- **THEN** the configuration is refused and nothing changes
