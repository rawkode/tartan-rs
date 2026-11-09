# Spec Delta

## Purpose

Verifying Tartan signatures on pushed commits and tags (each a reference to a `academy.rawkode.tartan.signature` record in the signer's ATProto repository), keeping the result and its proof from push time, and enforcing a repository's signing rule.

## ADDED Requirements

### Requirement: New objects are checked after each push
For each `ref.updated` on `refs/heads/*` or `refs/tags/*` (not deletions), the forge SHALL list the commit `new` resolves to, the annotated tag if `new` is one, and every commit in `old..new`, newest first, at most 1000, and SHALL verify those it has no stored result for, within the verification bounds. Objects with a stored result SHALL NOT be verified again. Commits beyond the limit SHALL be `not_checked`.

#### Scenario: Fast-forward
- **WHEN** `main` moves from `A` to `C` with `B` in between
- **THEN** `B` and `C` are checked and `A` is not checked again

#### Scenario: Rewind lists the tip
- **WHEN** `main` is moved back from `C` to its ancestor `A`
- **THEN** `A` is listed and judged even though `old..new` is empty

#### Scenario: Branch of existing commits
- **WHEN** a new branch is pushed at a commit already checked
- **THEN** no commit is verified again and the ref is judged on the stored results

### Requirement: Signature statuses
Each checked object SHALL get one status: `verified`, `unsigned`, `bad_signature`, `signer_inactive`, `unsupported_format` (OpenPGP, SSH, or X.509 that is not a Tartan signature), `unverifiable` (the record or identity could not be fetched or proved after retries) or `not_checked`.

#### Scenario: SSH signature
- **WHEN** a commit carries a `-----BEGIN SSH SIGNATURE-----` signature
- **THEN** its status is `unsupported_format`

#### Scenario: OpenPGP signature
- **WHEN** a commit carries a `-----BEGIN PGP SIGNATURE-----` signature
- **THEN** its status is `unsupported_format`

#### Scenario: No signature
- **WHEN** a commit has no `gpgsig` header
- **THEN** its status is `unsigned`

### Requirement: A signature is a proved record over the object
A signature SHALL verify only when it is a Tartan signature naming a `academy.rawkode.tartan.signature` record of a `did:plc` or hostname-only `did:web` DID, the record is proved back to that DID, its CID equals the signature's, its `namespace` is `git`, its `algorithm` is `hmac-sha256`, and its `digest` is HMAC-SHA256, keyed with the signature's `Salt`, of the object without its signature (a commit without its `gpgsig` header; a tag up to its signature).

#### Scenario: Altered message
- **WHEN** a signed commit's message is changed and the signature kept
- **THEN** its status is `bad_signature`

#### Scenario: Record for another object
- **WHEN** a commit's signature names a proved record whose digest is that of a different commit
- **THEN** its status is `bad_signature`

#### Scenario: Salt altered
- **WHEN** a signed commit's `Salt` line is replaced and the rest of the commit kept
- **THEN** its status is `bad_signature`

#### Scenario: Non-canonical Salt or CID encoding
- **WHEN** a signed commit's `Salt` is re-spelled with non-zero trailing bits, or its `CID` re-written in another multibase, so the decoded bytes are unchanged but the object id differs
- **THEN** its status is `bad_signature`

#### Scenario: Record replaced
- **WHEN** the named record exists but its proved CID differs from the signature's
- **THEN** its status is `bad_signature` with a detail saying the record was replaced

#### Scenario: Malformed Tartan signature
- **WHEN** a signature starts with `-----BEGIN SIGNED MESSAGE-----` and `Tartan-Signature: 1` but names a handle instead of a DID
- **THEN** its status is `bad_signature`

#### Scenario: Other X.509 signature
- **WHEN** a commit carries a CMS signature made by gitsign
- **THEN** its status is `unsupported_format`

### Requirement: The record is proved back to the DID
Before `verified`, the forge SHALL fetch `com.atproto.sync.getRecord` for the record, check every block's hash, check the repository commit's DID and its low-S signature by the DID's verified `#atproto` key, and walk the MST to the record. A signer whose verified account status is inactive SHALL give `signer_inactive`.

#### Scenario: PDS lies
- **WHEN** a PDS returns a CAR whose record block does not match the CID in the signed MST
- **THEN** the commit is not `verified`

#### Scenario: Repository deactivated
- **WHEN** the signer's PDS answers `getRecord` with `RepoDeactivated`
- **THEN** the signer's account status is refreshed and, once it is verified inactive, the commit is `signer_inactive`

### Requirement: A missing record is not final
When the record is proved absent, the PDS answers `RecordNotFound`, or the signer's PDS cannot be reached, the forge SHALL treat the check as a transient failure, because the signer's `tt` may not have published it yet, SHALL keep an update failing only for these reasons pending for up to seven days, retried at least hourly, and SHALL store `unverifiable` only when the check gives up.

#### Scenario: Record published late
- **WHEN** a commit is pushed while its record waits in the signer's local queue, and the record is published twenty minutes later
- **THEN** the check ends `verified` without another push

#### Scenario: Record published the next day
- **WHEN** a signed configuration commit is pushed to `main`, which requires signatures, while its record waits in the signer's queue, and the signer publishes it twenty hours later
- **THEN** the commit ends `verified` and the configuration is applied, with no owner or signer action at the forge

#### Scenario: Record never published
- **WHEN** a commit's record is still absent seven days after the push
- **THEN** the commit is `unverifiable` with a detail naming the record, and the signer's or an owner's re-check after it is published ends `verified`

### Requirement: A proof that does not verify is not final
When the repository commit's signature or the MST proof fails, the forge SHALL drop its cached proofs for the DID, refresh the DID's identity without any cache, and retry the proof once. A proof that still fails SHALL be a transient failure, never `bad_signature`.

#### Scenario: Key rotated while the PDS lags
- **WHEN** a signer's `#atproto` key is rotated in PLC and their PDS serves a commit signed with the old key for one attempt before re-signing
- **THEN** the check refreshes the identity, ends `verified`, and never records the commit as `bad_signature`

#### Scenario: Proof never verifies
- **WHEN** every proof the signer's PDS serves fails against the DID's freshly verified `#atproto` key for an hour
- **THEN** the commit is recorded `unverifiable` with a reason naming the PDS and the DID, and an owner can re-check it

### Requirement: Results are kept with their proof
Each result SHALL be stored once with the object id, status, signer DID, record key and CID, the repository revision that proved it, and the digest of a proof bundle in the object store (`proofs/sha256/<hex>.json`) holding the getRecord CAR, the identity evidence, the signature's salt and the times. A stored result SHALL never change except `unverifiable`, which an owner or its signer may re-check; a re-check SHALL judge again every update that listed the object, each by its own copied rule.

#### Scenario: Owner re-checks an unverifiable commit
- **WHEN** an `unverifiable` commit was judged on `main` and on `release/1.0`, the signing rule then changes, and an owner re-checks the commit once the signer's PDS is back
- **THEN** both updates are judged again by the rules copied when they were pushed, and if `main`'s head update is now compliant its configuration is applied

#### Scenario: Offline re-check
- **WHEN** an auditor downloads a verified commit's proof bundle
- **THEN** they can verify the record proof, its digest against the commit and, for a `did:plc` signer, the DID evidence without contacting the network; for `did:web` the bundle records the document as fetched over TLS

### Requirement: Later revocations annotate results
When a record that verified a stored result is later deleted or replaced, or the signer's DID becomes inactive, reads of the result SHALL show it as `verified` with `revoked_at` or `did_deactivated_at`, not as a different status.

#### Scenario: Record deleted after the push
- **WHEN** Alice deletes a signature record a week after the commit it signs was verified
- **THEN** the commit still shows verified as Alice, with the date the signature was revoked

### Requirement: Records the forge relies on are watched
The identity actor SHALL track every record that verified a result. A Jetstream delete or update in `academy.rawkode.tartan.signature` for a tracked record, or `POST /-/api/people/<did>/signatures/<rkey>/refresh` by that DID or an owner, SHALL re-prove it; a proved absence or another CID SHALL mark it revoked and record `signature.revoked {did, rkey, cid}`. A revoked record SHALL stay revoked. Jetstream SHALL only trigger.

#### Scenario: Deleted record
- **WHEN** a person deletes a tracked signature record and Jetstream reports it
- **THEN** the forge proves the record absent and records `signature.revoked`

#### Scenario: Forged deletion
- **WHEN** Jetstream reports the deletion of a tracked record that the PDS still proves present
- **THEN** nothing is marked revoked

#### Scenario: New records ignored
- **WHEN** Jetstream reports a person creating signature records
- **THEN** the forge fetches nothing

#### Scenario: Revoked record re-created
- **WHEN** a revoked record is re-created at the same key with the same CID and the forge proves it again
- **THEN** the record stays revoked

### Requirement: Revocations are confirmed with fresh evidence
A re-proof SHALL fetch `getRecord` afresh, never from the proof-reuse cache, and a Jetstream delete or update SHALL drop the record's cached bundle. Before a proof revokes a tracked record (a re-proof, a replaced record or a proved absence during verification), the forge SHALL re-resolve the DID without cache and prove again with that PDS and key; if that fails, or the proof's `rev` is lower than the stored one, nothing SHALL be revoked.

#### Scenario: Deleted within the reuse window
- **WHEN** a signature record verifies on a push and its signer deletes it less than ten minutes later, and Jetstream reports the delete
- **THEN** the forge proves the record absent from a fresh fetch and marks it revoked

#### Scenario: Stale PDS after migration
- **WHEN** the signer has moved to a new PDS and key, the forge still holds the old ones, and the old PDS serves a valid absence proof for a tracked record under the old key
- **THEN** the forge re-resolves the DID, finds the record at the new PDS with its original CID, and revokes nothing

#### Scenario: Fresh resolution fails
- **WHEN** a re-proof proves a tracked record absent but re-resolving the DID fails
- **THEN** nothing is revoked and the failure is transient

#### Scenario: Older repository state
- **WHEN** a re-proof proves a tracked record absent from a repository commit whose `rev` is lower than the one stored for the record
- **THEN** nothing is revoked

### Requirement: The signer can withdraw a signature on the forge
`POST /-/api/people/<did>/signatures/<rkey>/refresh` with `{"withdraw": true, "cid"}`, called by that DID itself from a browser or CLI session, whatever its rights, SHALL mark the record revoked with reason `withdrawn` and record `signature.revoked` without fetching it, inserting an already-revoked row, never deleted, for an untracked record within the caps (1,000 per DID, 100,000 forge-wide; beyond them only such inserts get 429). Owners, other people and access tokens SHALL NOT withdraw.

#### Scenario: Withdrawal wins the race
- **WHEN** a person withdraws a tracked record and the record is re-created with the same CID before any re-proof
- **THEN** the record stays revoked and the signature no longer counts toward signing rules

#### Scenario: Withdrawn before the forge saw it
- **WHEN** a person withdraws a record the forge does not track and an object signed by it is later pushed to a covered ref
- **THEN** the object's signature does not count toward the rule

#### Scenario: Pre-withdrawal does not expire
- **WHEN** a person withdraws an untracked record, the record is re-created with the same CID, and 31 days and several sweeps later an object signed by it is pushed to a covered ref
- **THEN** the object's signature does not count toward the rule

#### Scenario: Forge-wide cap
- **WHEN** the forge holds 100,000 untracked withdrawn rows
- **THEN** another untracked withdrawal answers 429, while a person's withdrawal of a tracked record succeeds

#### Scenario: Owner cannot withdraw
- **WHEN** an owner calls the route with `withdraw` for another person's record
- **THEN** it is refused and nothing is revoked

#### Scenario: Signer without rights withdraws
- **WHEN** a contributor who cannot read the forge withdraws, with a CLI session, a signature on a commit pushed to a covered ref
- **THEN** the record is marked revoked and the signature no longer counts toward the rule

### Requirement: The signer can re-check their own result
`POST /-/api/orgs/<org>/repos/<repo>/commits/<sha>/signature/recheck` SHALL also be allowed, rate-limited, to the principal whose DID is stored on that `unverifiable` result, whatever its rights, and SHALL run the owner's re-check. For any other oid such a caller SHALL get 404.

#### Scenario: Signer recovers a late record
- **WHEN** a signer who cannot read the forge publishes a record eight days after the push and runs `tt sign recheck <commit> --repo acme/api`
- **THEN** the commit is re-checked and ends `verified`, with no owner action

#### Scenario: Someone else's commit
- **WHEN** a non-owner asks to re-check an `unverifiable` commit whose stored DID is not theirs
- **THEN** the forge answers 404 and nothing is queued

#### Scenario: Probing for private repositories
- **WHEN** a signed-in non-owner who may not read the forge asks to re-check a commit in an org or repository that does not exist, under a malformed name, or in an existing repository with no result of theirs
- **THEN** every answer is the same 404 body, so it reveals nothing about which orgs and repositories exist

### Requirement: Each check is recorded
After checking a ref update the forge SHALL record `ref.signatures.checked {ref, old, new, commits: [{sha, status, did}], truncated}` on the repository's source, in the same transaction as the stored results.

#### Scenario: Event
- **WHEN** a push adds two signed commits to `main`
- **THEN** one `ref.signatures.checked` lists both with their statuses

### Requirement: Transient failures are retried
When the git host, a DID document or a record cannot be fetched, a record is missing, or a proof does not verify after a fresh identity, the forge SHALL retry with a growing delay for up to one hour (seven days on a covered ref waiting only on allowed signers' records, PDSes, DID hosts or the PLC directory), keep the last reason (no secrets), and keep checking other updates. A git host failure SHALL get its own hour from the first of its consecutive failures, without ending an update that is waiting on its signers sooner. A failure of the forge's own storage or services during an attempt SHALL count as a failed attempt of that update, backed off like any other. Retries SHALL use one durable schedule per repository, and checks SHALL never run concurrently.

#### Scenario: PDS down briefly
- **WHEN** the signer's PDS is unreachable for five minutes after a push
- **THEN** the commit is checked once it is back and is `verified`

#### Scenario: PDS down for a day
- **WHEN** the signer's PDS is unreachable for a day after a push to a covered ref
- **THEN** the commit stays `pending` meanwhile and is `verified` within an hour of the PDS coming back

#### Scenario: Signer's DID host down
- **WHEN** a covered ref's signer's DID document cannot be fetched (its `did:web` host or the PLC directory is down) for two hours after a push
- **THEN** the commit stays `pending` meanwhile and is `verified` once the host is back

#### Scenario: Git host blip while waiting on a signer
- **WHEN** an update on a covered ref has waited more than an hour for its allowed signer's record and one attempt then fails because the git host fails
- **THEN** the update stays `pending`, and is `verified` once the record is published

#### Scenario: Object store down during a check
- **WHEN** storing a proof bundle fails during an attempt
- **THEN** the attempt counts as failed, its next attempt is scheduled after the backoff rather than at once, and the commit is `verified` once the store is back

#### Scenario: Retry while the forge is idle
- **WHEN** an attempt fails and no other request reaches the repository until its next attempt is due
- **THEN** a schedule registered at that attempt's `next_at` wakes the repository and the attempt runs

#### Scenario: Many queued checks fail together
- **WHEN** a push queues 1500 tag updates while the git host is failing
- **THEN** the repository has at most one pending signature-check schedule, and a failed event delivery can still schedule its retry

#### Scenario: Overlapping wakes
- **WHEN** two signature-check wakes of one repository overlap
- **THEN** each ref update records `ref.signatures.checked` once

### Requirement: Verification work is bounded
One attempt SHALL end within 90 seconds, cancelling fetches in flight; unfinished objects SHALL carry to the next attempt without counting as failures. A run SHALL resolve each DID once, fail fast on a host that already failed, and work the head `main` update first. An update SHALL fetch for at most 64 signer DIDs; objects beyond are `not_checked` for that update, unstored, and an owner can re-check them.

#### Scenario: Tarpit signers on an uncovered branch
- **WHEN** someone pushes 1000 commits signed as 1000 distinct `did:web` signers whose PDSes never answer, to a branch no rule covers
- **THEN** each attempt ends within 90 seconds with a bounded number of outbound requests, and the update gives up after one hour

#### Scenario: Configuration pushed behind a slow check
- **WHEN** a signed `main` configuration is pushed while that tarpit update is queued
- **THEN** it is verified and applied without waiting for the tarpit update to resolve

#### Scenario: Large legitimate update
- **WHEN** a push adds 1000 commits signed by two allowed signers and they cannot all be proved in one attempt
- **THEN** results proved so far are kept and the next attempt continues with the rest

#### Scenario: Signer outside the rule
- **WHEN** a commit on covered `main` names a DID not in `signers` whose PDS cannot be reached
- **THEN** the update gives up after one hour, not seven days

### Requirement: A check that gives up still judges its update
A check that gives up (after an hour, after seven days when waiting on signers, or at once when the git host no longer serves `new`) SHALL store the objects it completed as they are and those still failing as `unverifiable`, judge the update with them (on the tip alone when the git host never listed the update's commits), and record the update's result and events. An owner's re-check SHALL queue the update again and check every commit it adds.

#### Scenario: Object gone from the host
- **WHEN** a ref is force-pushed again before its previous update is checked and the old `new` is no longer served
- **THEN** that update's head is recorded `unverifiable` at once with a reason naming the missing object, and the update is judged with it

#### Scenario: Git host down for an hour
- **WHEN** the git host fails every attempt for an hour after a push to a covered ref
- **THEN** the tip is recorded `unverifiable`, the update is judged non-compliant, and an owner's re-check queues the update again and checks every commit it adds

### Requirement: Signatures are readable
`GET /-/api/orgs/<org>/repos/<repo>/commits/<sha>/signature` SHALL answer the stored result with its annotations and a proof URL, and `GET /-/api/orgs/<org>/repos/<repo>/signatures?ref=<ref>` the result and compliance of the ref's latest update; without `ref` it SHALL answer the same for every branch head, leaving out deleted branches. A stored commit result SHALL carry the commit's claimed `author` and `committer` as `{name, email}`. While the commit's check, or the ref's latest update, is queued, they SHALL answer `{status: "pending", attempts, next_at, last_error, did}`. A commit an update left unchecked (deferred) SHALL answer the stored-result shape with `status: "not_checked"`, its `detail` and no annotations.

#### Scenario: Older update finishes last
- **WHEN** `main` moves to W and then to X, and W's check finishes after X's
- **THEN** the ref's result and the repository page report X's result, and W's result is kept only for W's update

#### Scenario: Deleted branch
- **WHEN** a branch whose update was judged non-compliant is deleted
- **THEN** it is no longer listed without `ref`, and `signatures?ref=` for it answers 404

#### Scenario: Unchecked commit
- **WHEN** a client asks for a commit the forge has neither checked nor queued
- **THEN** the API answers 404

#### Scenario: Check pending
- **WHEN** a client asks for a just-pushed head whose signature record is not published yet
- **THEN** the API answers `pending` with the attempts so far and the last error, and the repository page shows that it is retrying

### Requirement: The signing rule is enforced after the push
When the rule in force before a push covers an updated ref, every object the update lists, including the commit `new` resolves to, SHALL be judged on its stored result against this ref's `signers`. A `verified` result, reused or new, SHALL count only while its record is not revoked on the forge and its DID is not inactive. Any other object, or a truncated walk, SHALL record `ref.signature.violation` and mark the ref non-compliant until a later update is compliant.

#### Scenario: Unsigned commit on a protected branch
- **WHEN** a repository requires signatures on `refs/heads/main` and an unsigned commit is pushed there
- **THEN** `ref.signature.violation` is recorded and the repository page warns that `main` is not compliant

#### Scenario: Unsigned commit arrives via another branch
- **WHEN** an unsigned commit is pushed to an unprotected branch `x` and `main`, which requires signatures, is then fast-forwarded to it
- **THEN** `ref.signature.violation` is recorded for `main`

#### Scenario: Signer allowed elsewhere
- **WHEN** a commit verified as a DID not in `main`'s `signers` is checked first on another branch and `main` is then fast-forwarded to it
- **THEN** `ref.signature.violation` is recorded for `main`

#### Scenario: New protected ref with long history
- **WHEN** a new ref covered by a rule is pushed at a commit with more than 1000 ancestors
- **THEN** the commits beyond the limit are `not_checked` and the ref is not compliant

#### Scenario: Rewind to an unsigned ancestor
- **WHEN** `main` requires signatures and is force-pushed back to an unsigned ancestor commit
- **THEN** `ref.signature.violation` is recorded for `main` and that commit's configuration is refused with `signing: <oid> is unsigned`

#### Scenario: Rewind to a signed head
- **WHEN** `main` requires signatures and is force-pushed back to an ancestor verified as an allowed signer whose record is not revoked
- **THEN** the update is compliant

#### Scenario: Signature revoked before the fast-forward
- **WHEN** a commit signed by Alice is verified on an uncovered branch, Alice then deletes its signature record and the forge learns of it, and `main`, which requires Alice, is fast-forwarded to it
- **THEN** `ref.signature.violation` is recorded for `main` with the problem `<oid> signature was revoked`, its configuration is refused, and the commit itself still reads `verified` with the revocation noted

#### Scenario: Revoked signature pushed to another repository
- **WHEN** Alice's record is deleted and the forge learns of it, the record is re-created with the same CID, and the commit is pushed to a second repository whose rule requires Alice
- **THEN** the commit is stored `verified` there with the revocation noted, and `ref.signature.violation` is recorded with the problem `<oid> signature was revoked`

#### Scenario: Signer deactivated before the fast-forward
- **WHEN** a commit verified as Alice on an uncovered branch reaches `main`, which requires Alice, after Alice's account is deactivated
- **THEN** `ref.signature.violation` is recorded for `main` with the problem `<oid> signer's account is not active`

#### Scenario: Tag of a tree
- **WHEN** a covered tag ref is pushed pointing, through annotated tags, at a tree rather than a commit
- **THEN** the tip is `not_checked` and the ref is not compliant

#### Scenario: Rule removed in the same push
- **WHEN** a push to `main` removes the signing rule and adds an unsigned commit
- **THEN** the rule in force before the push applies and the violation is recorded
