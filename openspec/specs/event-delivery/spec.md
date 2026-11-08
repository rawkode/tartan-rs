# event-delivery Specification

## Purpose
Every change in the forge is an event; subscribers receive the events matching their pattern, in order per source and at least once.

## Requirements

### Requirement: Every state change appends an event atomically
Each actor (the forge, an org, a repository, an extension installation) SHALL append an event to its own log in the same transaction as the change it records. An event SHALL carry an id, its source (`forge`, `org:<id>`, `repo:<id>`, `extension:<id>`), its sequence number in that source, its type, its time and a JSON payload.

#### Scenario: Org creation
- **WHEN** an org is created
- **THEN** the forge's log gains `org.created` and the org's log gains `org.ready`

### Requirement: Subscribers receive matching events per source in order, at least once
A subscriber is registered with `PUT /-/api/subscribers/<name> {"pattern": …}`. A pattern is dot-separated tokens where `*` matches one token and a final `>` matches one or more. Each source SHALL deliver the events matching a subscriber's pattern in sequence order, SHALL retry until the subscriber has stored them, and the subscriber SHALL store each `(source, seq)` once.

#### Scenario: Pattern filter
- **WHEN** subscribers `audit` (`>`) and `refs` (`ref.*`) exist and a push is recorded
- **THEN** `audit` receives `push.accepted` and `ref.updated`, and `refs` receives only `ref.updated`

#### Scenario: Order and uniqueness
- **WHEN** a subscriber's received events are read (`GET /-/api/subscribers/<name>/events`)
- **THEN** no `(source, seq)` appears twice and each source's sequence numbers increase

### Requirement: A new recipient receives history
When an extension installation subscribes, the sources it covers SHALL be asked to deliver at once, so it receives every matching event already in their logs without waiting for a new one.

#### Scenario: Subscribing after the fact
- **WHEN** an org installation subscribes after its repositories recorded ref events
- **THEN** it receives those events promptly
