# git-hosting Specification

## Purpose
Git repositories are hosted by walgit in an object store; the forge turns walgit's ref events into forge events.

## Requirements

### Requirement: Git over smart HTTP
Clients SHALL clone, fetch and push over smart HTTP at the repository's clone URL. A push SHALL send the Git host token as `Authorization: Bearer <token>` (for example with `git -c http.extraHeader=…`); anonymous reads are allowed.

#### Scenario: Round trip
- **WHEN** a client pushes `main`, creates and deletes a branch, and clones
- **THEN** the clone's `HEAD` is the last pushed commit

### Requirement: The object store is the repository
Any walgit host pointed at the bucket SHALL serve the repository, including one with an empty cache.

#### Scenario: Fresh host
- **WHEN** a second walgit host with an empty cache serves a clone
- **THEN** the clone's `HEAD` is the last pushed commit

### Requirement: Ref events become forge events exactly once
The forge SHALL accept walgit's ref events only with a valid `X-Walgit-Signature` (HMAC-SHA256 of the body), SHALL append one `push.accepted` per walgit log entry and one `ref.updated` per ref update to the repository's log, and SHALL ignore repeats. Events for repositories the forge does not know SHALL be skipped.

#### Scenario: Bad signature
- **WHEN** `/-/hooks/git-host` receives a body with a wrong signature
- **THEN** it answers 401 and records nothing

#### Scenario: Repeated delivery
- **WHEN** the same batch arrives twice
- **THEN** each `push.accepted` and `ref.updated` is recorded once, in walgit's order
