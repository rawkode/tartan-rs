# extension-events Specification

## Purpose
Installations receive the events their manifest subscribes to through `on-event`, scoped to where they are installed.

## Requirements

### Requirement: Subscriptions are scoped to the installation
An installation SHALL receive the events matching its manifest's `subscribe` patterns from its repository and from the extensions installed on that repository. An installation SHALL never receive its own events.

#### Scenario: Another repository
- **WHEN** a ref is updated in `acme/other`
- **THEN** an installation on `acme/api` does not receive it

#### Scenario: Another extension on the repository
- **WHEN** Issues on `acme/api` emits `x.issues.tartan.rawkode.academy.issue.created`
- **THEN** Kanban on `acme/api`, subscribed to it, receives it

### Requirement: Each event reaches on-event once
An installation SHALL pass each `(source, seq)` to `on-event` once, in order per source. An event `on-event` fails on (an error, a trap or an exhausted limit) SHALL be kept as a dead letter and SHALL NOT block the events after it.

#### Scenario: Failing event
- **WHEN** `on-event` fails on one event of three
- **THEN** the other two are processed and the failing one is a dead letter
