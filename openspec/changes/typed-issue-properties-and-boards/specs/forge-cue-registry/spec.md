# Spec Delta

## Purpose
The forge is the CUE registry for its repositories, and its deployment configuration decides which modules it serves.

## ADDED Requirements

### Requirement: The forge serves a read-only OCI registry
The gateway SHALL serve the OCI Distribution read API at `/v2/` — manifests, blobs and tag lists, `GET` and `HEAD` — from the object store, so `CUE_REGISTRY=<forge>` resolves modules with the `cue` command. Writes SHALL be refused.

#### Scenario: cue fetches a module
- **WHEN** a person runs `cue mod tidy` with `CUE_REGISTRY` set to the forge and imports a module the forge serves
- **THEN** the module is fetched from the forge

#### Scenario: Push refused
- **WHEN** a client starts a blob upload at `/v2/<name>/blobs/uploads/`
- **THEN** the forge answers 405

### Requirement: The deployment configuration lists available modules
The forge SHALL read the CUE package `forge` from the path in `TARTAN_CONFIG`: `modules`, fully qualified `path@version` entries, and `sources`, module path prefixes mapped to OCI registries. An invalid configuration SHALL stop the forge from starting.

#### Scenario: Unknown source
- **WHEN** a listed module's path matches no source
- **THEN** the forge does not start and names the module

### Requirement: Modules are synced at startup
At startup the forge SHALL copy each listed module from its source into its registry unless it already holds that version with the same digest, and SHALL refuse a listed version whose digest differs from the one it holds. A module it cannot fetch SHALL be reported and the forge SHALL start with the modules it has.

#### Scenario: Already synced
- **WHEN** the forge restarts with the same module list
- **THEN** nothing is copied again

#### Scenario: Changed digest
- **WHEN** a source serves different contents for a version the forge already holds
- **THEN** the forge keeps its copy and reports the mismatch
