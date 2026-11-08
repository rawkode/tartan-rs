# Spec Delta

## MODIFIED Requirements

### Requirement: The org's tartan repository configures the org
The repository named `tartan` in an org SHALL be the org's configuration: each entry with a `version` installs an `org`-scoped package on the org, and each entry without one gives org-wide settings for a `repo`-scoped extension. When org-wide settings change, every repository in the org SHALL apply its latest configuration again with them.

#### Scenario: Org install
- **WHEN** `acme/tartan`'s configuration names `rawkode.kanban` version `0.3.0`
- **THEN** Kanban is installed on `acme`

#### Scenario: Org-wide settings change
- **WHEN** `acme/tartan` adds an Issues property
- **THEN** every repository in `acme` that installs Issues has the new property
