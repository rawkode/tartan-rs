# extension-packages Specification

## Purpose
Extension packages (manifest, component, migrations) are published to object storage and are immutable per id and version.

## Requirements

### Requirement: Publishing checks and stores a package
`POST /-/api/packages {"manifest", "component" (base64), "migrations": [{path, sql}]}` SHALL check that the manifest targets `tartan:ext@0.1.0`, that the migrations match the manifest's list in order, and that the component fits the world, and SHALL store the package under its id, version and a digest of all its contents. An invalid package SHALL be refused with 400; the forge SHALL record `package.published`.

#### Scenario: Publish
- **WHEN** the Issues package is published
- **THEN** the API answers 201 and the forge's log gains `package.published`

#### Scenario: Not a component
- **WHEN** a package's component is not WebAssembly
- **THEN** the API answers 400

### Requirement: id@version never changes
Publishing the same contents again SHALL answer 200 with the existing package; publishing other contents under a published `id@version` SHALL answer 409.

#### Scenario: Changed contents
- **WHEN** a package with a changed migration is published under the same version
- **THEN** the API answers 409

### Requirement: A package declares where it installs
A manifest's `storage.scope` SHALL be `repo` or `org`; a package SHALL only be installed where its scope says.

#### Scenario: Wrong scope
- **WHEN** an org-scoped package is installed on a repository
- **THEN** it is refused
