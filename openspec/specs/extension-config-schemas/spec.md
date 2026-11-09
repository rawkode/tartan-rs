# extension-config-schemas Specification

## Purpose
An extension is one CUE module carrying its settings schema and its package, and repositories' configuration evaluates natively against the modules they import.

## Requirements

### Requirement: An extension is one CUE module
An extension version SHALL be one CUE module holding its settings schema and its package (`tartan.json`, `component.wasm`, migrations). The schema SHALL define `#Extension`, which fixes the extension's `version` to the module's version and types its `settings`.

#### Scenario: Version from the module
- **WHEN** a repository sets `extensions: "issues.tartan.rawkode.academy": issues.#Extension & {settings: …}` with `tartan.rawkode.academy/issues@v0` at `v0.6.0`
- **THEN** the configuration installs Issues 0.6.0

### Requirement: Configuration evaluates natively
The forge SHALL evaluate a repository's configuration exactly as committed, adding no files, against its own registry, so `cue export` of the root package `tartan` with `CUE_REGISTRY` set to the forge gives the configuration the forge applies. A module the forge does not serve SHALL make the configuration invalid.

#### Scenario: Same as cue export
- **WHEN** a repository's configuration imports Issues' and Kanban's modules
- **THEN** the forge applies the value `cue export` gives for package `tartan`

#### Scenario: Unavailable module
- **WHEN** a configuration depends on a module version the deployment does not list
- **THEN** the configuration is refused, naming the module

#### Scenario: Dropped module
- **WHEN** the forge restarts with a deployment that no longer lists a module version and a configuration names its package
- **THEN** the configuration is refused, and an installation of the package keeps running until its repository's configuration changes

### Requirement: Settings that do not fit are refused
Settings that do not fit their extension's `#Extension` SHALL fail evaluation and refuse the configuration.

#### Scenario: Invalid settings
- **WHEN** Issues' settings give a property an unknown `kind`
- **THEN** the configuration is refused and nothing changes
