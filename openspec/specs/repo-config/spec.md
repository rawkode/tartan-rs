# repo-config Specification

## Purpose
A repository's root `tartan.cue` (CUE package `tartan`) declares the extensions it installs and their settings.

## Requirements

### Requirement: The configuration file and its schema
`tartan.cue` SHALL be evaluated as CUE package `tartan` against the forge's schema: `extensions` maps an extension id to an entry with an optional semver `version` and free-form `settings`. Any other key in an entry, an invalid id or version, a conflict or a syntax error SHALL make the configuration invalid. An absent file SHALL be the empty configuration.

#### Scenario: Install and contribution
- **WHEN** `tartan.cue` sets `extensions: "rawkode.issues": {version: "0.5.0", settings: …}` and `extensions: "rawkode.kanban": settings: …`
- **THEN** the configuration installs Issues 0.5.0 with those settings and contributes settings to Kanban

#### Scenario: Invalid entry
- **WHEN** an entry has `version: "latest"` or an unknown key
- **THEN** the configuration is invalid
