# Spec Delta

## MODIFIED Requirements

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

## ADDED Requirements

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
