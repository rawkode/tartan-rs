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

### Requirement: The forge reads pushed objects
The forge SHALL read the raw commit and tag objects a ref update introduced from the Git host over smart HTTP with the Git host token, and SHALL queue each update for signature checking in the same transaction that records its `ref.updated`, so no update is skipped across restarts.

#### Scenario: Restart before the check
- **WHEN** the forge stops after recording `ref.updated` but before checking its commits
- **THEN** the commits are checked after it starts again

#### Scenario: Raw bytes
- **WHEN** a pushed commit carries a `gpgsig` header
- **THEN** the forge checks the signature over the commit's exact bytes without that header

### Requirement: Pushes are not attributed to people
The Git host token SHALL remain the only push credential; `push.accepted`'s pusher SHALL stay the Git host principal, and attribution to a person SHALL come only from verified signatures.

#### Scenario: Pusher
- **WHEN** an owner pushes a signed commit
- **THEN** `push.accepted` names the Git host principal and `ref.signatures.checked` names the owner's DID
