# Spec Delta

## REMOVED Requirements

### Requirement: The org's tartan repository configures the org
**Reason**: Org-wide configuration cannot be evaluated natively by `cue eval` of one repository; every extension installs on a repository.
**Migration**: Configure each repository's extensions in its own package `tartan`; share definitions by publishing a CUE module and importing it.

### Requirement: Repositories contribute settings to org installations
**Reason**: There are no org installations to contribute to.
**Migration**: Configure the extension on the repository itself.

## RENAMED Requirements

- FROM: `### Requirement: Configuration is applied all or nothing`
- TO: `### Requirement: Configuration is checked before it is applied`

## MODIFIED Requirements

### Requirement: Configuration is checked before it is applied
Before changing anything, the forge SHALL check every entry: its version's package is in a module the deployment lists, and the extensions its manifest `requires` are installed by the same configuration. Invalid CUE or any failed check SHALL refuse the configuration, record `config.refused` with the problems and the commit, and leave every installation as it was. Applying SHALL then initialise each extension in turn, the extensions a package requires first; when one fails, the installations already changed SHALL stay changed, nothing SHALL be recorded, and the configuration SHALL be applied again later.

#### Scenario: Unpublished version
- **WHEN** a pushed configuration names `rawkode.issues` version `9.9.9`, which no listed module holds
- **THEN** `config.refused` is recorded and the installed Issues is unchanged

#### Scenario: Invalid CUE
- **WHEN** a pushed configuration does not evaluate
- **THEN** `config.refused` is recorded with the evaluation error

## ADDED Requirements

### Requirement: Failures that may pass are retried
A configuration that cannot be evaluated because the Git host or the forge's registry failed, or whose application failed, SHALL stay unapplied and SHALL be tried again with a growing delay, and again when the repository actor starts or `main` moves; it SHALL NOT be refused for that reason. Configurations SHALL be applied only once the forge's module sync at startup has finished.

#### Scenario: Pushed during the sync
- **WHEN** a configuration depending on a newly listed module is pushed while the forge is still copying that module
- **THEN** it is applied once the sync has finished, not refused
