## ADDED Requirements

### Requirement: Each actor appends its events atomically
Each actor (the forge, its identity actor, a group, a repository, an extension installation) SHALL append an event to its own log in the same transaction as the change it records. An event SHALL carry an id, its source (`forge`, `identity`, `group:<id>`, `repo:<id>`, `extension:<id>`), its sequence number in that source, its type, its time and a JSON payload. Identity and signature-check bookkeeping and secrets (OAuth requests, sessions, cursors, retry state, caches) SHALL record no events.

#### Scenario: Group creation
- **WHEN** a group is created
- **THEN** its parent's log gains `group.created` with the group's path and id, and the group's own log gains `group.ready`

#### Scenario: Repository creation
- **WHEN** a repository is created in a group
- **THEN** the group's log gains `repo.created` carrying the repository's `path` and `parent_id`

#### Scenario: Sign-in
- **WHEN** a forge owner signs in
- **THEN** the identity log gains `person.signed_in`

## REMOVED Requirements

### Requirement: Every state change appends an event atomically
**Reason**: Orgs are removed, so the requirement is restated for the tree.
**Migration**: See "Each actor appends its events atomically".
