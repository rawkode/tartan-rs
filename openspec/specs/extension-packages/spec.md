# extension-packages Specification

## Purpose
Extension packages (manifest, component, migrations) are published to object storage and are immutable per id and version.

## Requirements

### Requirement: id@version never changes
A registered `id@version` SHALL keep its digest: a synced module with the same id and version but other contents SHALL not replace it.

#### Scenario: Changed contents
- **WHEN** a module with a changed migration is listed under the same version
- **THEN** the forge keeps the registered package and reports the mismatch

### Requirement: A package declares where it installs
A manifest's `storage.scope` SHALL be `repo`; every package installs on repositories.

#### Scenario: Wrong scope
- **WHEN** a module's manifest has `storage.scope: "org"`
- **THEN** its package is not available

### Requirement: Packages come from synced modules
For each synced module holding a `tartan.json`, the forge SHALL check that the manifest targets `tartan:ext@0.1.0`, that its version equals the module's, that the migrations match the manifest's list in order, and that the component fits the world, and SHALL register the package under its id, version and the module's digest, recording `package.published`. A module that fails these checks SHALL not be available.

#### Scenario: Register
- **WHEN** the Issues module v0.6.0 is synced
- **THEN** `issues.tartan.rawkode.academy@0.6.0` is available and the forge's log gains `package.published`

#### Scenario: Not a component
- **WHEN** a module's `component.wasm` is not WebAssembly
- **THEN** its package is not available and the sync reports why

### Requirement: A package names its requirements
A manifest MAY list the extensions it requires (`requires`). Reconciliation SHALL refuse installing a package whose required extensions the same configuration does not install.

#### Scenario: Kanban without Issues
- **WHEN** a repository's configuration installs Kanban but not Issues
- **THEN** the configuration is refused, naming `issues.tartan.rawkode.academy`
