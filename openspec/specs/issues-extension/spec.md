# issues-extension Specification

## Purpose
`rawkode.issues`: numbered issues per repository with configured fields, emitting a snapshot of each issue on every write.

## Requirements

### Requirement: Issues with configured fields
Each repository installation SHALL keep issues numbered from 1, open or closed, optionally filed against a project (a root path). Fields SHALL come from the installation's settings (`fields: <name>: {type, options, default, …}`); a value that does not fit its field SHALL be refused with `invalid`.

#### Scenario: Create and list
- **WHEN** two issues are created and the issues are listed
- **THEN** they are numbered 1 and 2 and both are open

### Requirement: Every write emits a snapshot
Every write SHALL emit `x.rawkode.issues.issue.created` or `.updated` carrying the issue's number, title, project, state, fields, version and update time, without its body, so other extensions can keep a copy by keeping the highest version.

#### Scenario: Created
- **WHEN** two issues are created
- **THEN** two `x.rawkode.issues.issue.created` events are recorded
