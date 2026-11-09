# Spec Delta

## MODIFIED Requirements

### Requirement: Every state change appends an event atomically
Each actor (the forge, its identity actor, an org, a repository, an extension installation) SHALL append an event to its own log in the same transaction as the change it records. An event SHALL carry an id, its source (`forge`, `identity`, `org:<id>`, `repo:<id>`, `extension:<id>`), its sequence number in that source, its type, its time and a JSON payload. Identity bookkeeping and secrets (OAuth requests, sessions, use times, the Jetstream cursor) SHALL record no events.

#### Scenario: Org creation
- **WHEN** an org is created
- **THEN** the forge's log gains `org.created` and the org's log gains `org.ready`

#### Scenario: Sign-in
- **WHEN** a person signs in
- **THEN** the identity log gains `person.signed_in`
