## MODIFIED Requirements

### Requirement: The signer can re-check their own result
`POST /-/api/tree/<repo path>/-/commits/<sha>/signature/recheck` SHALL also be allowed, rate-limited, to the principal whose DID is stored on that `unverifiable` result, whatever its rights, and SHALL run the owner's re-check. For any other oid such a caller SHALL get 404.

#### Scenario: Signer recovers a late record
- **WHEN** a signer who cannot read the forge publishes a record eight days after the push and runs `tt sign recheck <commit> --repo acme/api`
- **THEN** the commit is re-checked and ends `verified`, with no owner action

#### Scenario: Someone else's commit
- **WHEN** a non-owner asks to re-check an `unverifiable` commit whose stored DID is not theirs
- **THEN** the forge answers 404 and nothing is queued

#### Scenario: Probing for private repositories
- **WHEN** a signed-in non-owner who may not read the forge asks to re-check a commit in a group or repository that does not exist, under a malformed name, or in an existing repository with no result of theirs
- **THEN** every answer is the same 404 body, so it reveals nothing about which groups and repositories exist

### Requirement: Signatures are readable
`GET /-/api/tree/<repo path>/-/commits/<sha>/signature` SHALL answer the stored result with its annotations and a proof URL, and `GET /-/api/tree/<repo path>/-/signatures?ref=<ref>` the result and compliance of the ref's latest update; without `ref` it SHALL answer the same for every branch head. While the commit's check, or the ref's latest update, is queued, they SHALL answer `{status: "pending", attempts, next_at, last_error, did}`.

#### Scenario: Older update finishes last
- **WHEN** `main` moves to W and then to X, and W's check finishes after X's
- **THEN** the ref's result and the repository page report X's result, and W's result is kept only for W's update

#### Scenario: Unchecked commit
- **WHEN** a client asks for a commit the forge has neither checked nor queued
- **THEN** the API answers 404

#### Scenario: Check pending
- **WHEN** a client asks for a just-pushed head whose signature record is not published yet
- **THEN** the API answers `pending` with the attempts so far and the last error, and the repository page shows that it is retrying
