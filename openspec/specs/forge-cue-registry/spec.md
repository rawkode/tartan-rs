# forge-cue-registry Specification

## Purpose
The forge is the CUE registry for its repositories, and its deployment configuration decides which modules it serves.

## Requirements

### Requirement: The forge serves a read-only OCI registry
The gateway SHALL serve the OCI Distribution read API at `/v2/` — manifests, blobs and tag lists, `GET` and `HEAD` — from the object store, so `CUE_REGISTRY=<forge>` resolves modules with the `cue` command. Writes SHALL be refused.

#### Scenario: cue fetches a module
- **WHEN** a person runs `cue mod tidy` with `CUE_REGISTRY` set to the forge and imports a module the forge serves
- **THEN** the module is fetched from the forge

#### Scenario: Push refused
- **WHEN** a client starts a blob upload at `/v2/<name>/blobs/uploads/`
- **THEN** the forge answers 405

### Requirement: The registry serves only listed modules
The registry SHALL serve only the module versions the deployment lists. A blob, or a manifest asked for by digest, SHALL be served under a repository name only when a listed manifest of that repository is or references it.

#### Scenario: Unlisted version
- **WHEN** the registry holds a module version the deployment no longer lists
- **THEN** its manifest answers 404 and its tag is not listed

#### Scenario: Content under another name
- **WHEN** a client asks for a listed module's layer under another repository name, or as a manifest
- **THEN** the forge answers 404

### Requirement: The deployment configuration lists available modules
The forge SHALL read the CUE package `forge` from the path in `TARTAN_CONFIG`: `modules`, fully qualified `path@version` entries, `sources`, module path prefixes mapped to OCI registries, `owners`, the DIDs of the forge's administrators, and optional `readers`, who else may read. Any other key, or an invalid configuration, SHALL stop the forge from starting.

#### Scenario: Unknown source
- **WHEN** a listed module's path matches no source
- **THEN** the forge does not start and names the module

#### Scenario: Owners
- **WHEN** the configuration sets `owners: ["did:plc:abcdefghijklmnopqrstuvwx"]`
- **THEN** a person signed in with that DID is an owner of the forge

#### Scenario: Readers
- **WHEN** the configuration sets `readers: "signed_in"`
- **THEN** any person who signs in may read the forge

### Requirement: Modules are synced at startup
At startup the forge SHALL copy each listed module from its source into its registry unless it already holds that version with the same digest, and SHALL refuse a listed version whose digest differs from the one it holds. A module it cannot fetch SHALL be reported and the forge SHALL start with the modules it has. No blob larger than 64 MiB SHALL be copied, and no module holding more than 64 MiB of files SHALL be registered.

#### Scenario: Already synced
- **WHEN** the forge restarts with the same module list
- **THEN** nothing is copied again

#### Scenario: Changed digest
- **WHEN** a source serves different contents for a version the forge already holds
- **THEN** the forge keeps its copy and reports the mismatch

### Requirement: Owners are valid, distinct DIDs
`owners` SHALL be a non-empty list of distinct `did:plc` or hostname-only `did:web` DIDs. An empty or missing list, a duplicate or an invalid DID SHALL stop the forge from starting, naming the entry; so SHALL an unset `TARTAN_CONFIG`.

#### Scenario: No owners
- **WHEN** the configuration has `modules` and `sources` but no `owners`
- **THEN** the forge does not start and says that `owners` must list at least one DID

#### Scenario: Handle instead of DID
- **WHEN** `owners` contains `alice.bsky.social`
- **THEN** the forge does not start and names the entry as not a DID

### Requirement: Readers are a mode or valid, distinct DIDs
`readers` SHALL be `"owners"`, `"signed_in"` or a list of distinct `did:plc` or hostname-only `did:web` DIDs, and SHALL default to `"owners"` when absent. Any other value, a duplicate or an invalid DID SHALL stop the forge from starting, naming the entry.

#### Scenario: Unknown mode
- **WHEN** the configuration sets `readers: "everyone"`
- **THEN** the forge does not start and names `readers` with its allowed values

#### Scenario: Default
- **WHEN** the configuration has no `readers`
- **THEN** the forge starts and only owners may read
