# Spec Delta

## Purpose
Extensions check their own settings in CUE, may depend on another extension's settings, and may be given org-wide settings.

## ADDED Requirements

### Requirement: Settings are checked against the extension's schema
When an extension's package ships a settings schema, its settings in a configuration SHALL be evaluated against the schema's `#Settings`; settings that do not fit SHALL make the configuration invalid. An entry with a version SHALL use that version's schema; a contribution SHALL use the org installation's version; org-wide settings SHALL use the latest published version.

#### Scenario: Invalid settings
- **WHEN** a repository configures Issues with a property of an unknown type
- **THEN** the configuration is refused and nothing changes

### Requirement: Extensions may require another extension's settings
A package MAY declare that its settings require another extension, under an alias. Its schema SHALL then see that extension's settings in the same configuration as `#<alias>`, and configuring it where that extension is not configured SHALL make the configuration invalid.

#### Scenario: Kanban without Issues
- **WHEN** the org's configuration installs Kanban but configures no Issues properties
- **THEN** the configuration is refused with a problem naming `rawkode.issues`

### Requirement: Org-wide settings for repository extensions
An entry without a version in the org's `tartan` repository SHALL give org-wide settings for that repository extension. Each repository that installs it SHALL evaluate its own settings unified with the org-wide ones, so it may add to them but a conflicting redefinition SHALL make its configuration invalid.

#### Scenario: Adding a property
- **WHEN** the org defines `status` and `acme/api` adds `component`
- **THEN** Issues on `acme/api` has both properties

#### Scenario: Redefining a property
- **WHEN** `acme/api` defines `status` with other options than the org's
- **THEN** `acme/api`'s configuration is refused
