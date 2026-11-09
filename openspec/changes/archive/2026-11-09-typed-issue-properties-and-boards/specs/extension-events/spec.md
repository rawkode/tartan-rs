# Spec Delta

## MODIFIED Requirements

### Requirement: Subscriptions are scoped to the installation
An installation SHALL receive the events matching its manifest's `subscribe` patterns from its repository and from the extensions installed on that repository. An installation SHALL never receive its own events.

#### Scenario: Another repository
- **WHEN** a ref is updated in `acme/other`
- **THEN** an installation on `acme/api` does not receive it

#### Scenario: Another extension on the repository
- **WHEN** Issues on `acme/api` emits `x.rawkode.issues.issue.created`
- **THEN** Kanban on `acme/api`, subscribed to it, receives it
