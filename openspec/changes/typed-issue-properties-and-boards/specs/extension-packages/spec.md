# Spec Delta

## MODIFIED Requirements

### Requirement: A package declares where it installs
A manifest's `storage.scope` SHALL be `repo`; every package installs on repositories.

#### Scenario: Wrong scope
- **WHEN** a manifest's `storage.scope` is `org`
- **THEN** publishing answers 400

## ADDED Requirements

### Requirement: A package names its CUE module and requirements
A manifest SHALL name its settings module (`config.module`, a CUE module path) and MAY list the extensions it requires (`requires`). Reconciliation SHALL refuse installing a package whose required extensions are not installed on the same repository by the same configuration.

#### Scenario: Kanban without Issues
- **WHEN** a repository's configuration installs Kanban but not Issues
- **THEN** the configuration is refused, naming `rawkode.issues`
