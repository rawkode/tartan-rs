# Spec Delta

## ADDED Requirements

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
