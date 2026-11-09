# Spec Delta

## Purpose

Verifying the SSH signatures on pushed commits and tags against keys published in the signer's ATProto repository, keeping the result and its proof from push time, and enforcing a repository's signing rule.

## ADDED Requirements

### Requirement: New objects are checked after each push
For each `ref.updated` on `refs/heads/*` or `refs/tags/*` (not deletions), the forge SHALL list the commit `new` resolves to, the annotated tag if `new` is one, and every commit in `old..new`, newest first, at most 1000, and SHALL verify those it has no stored result for. Objects with a stored result SHALL NOT be verified again. Commits beyond the limit SHALL be `not_checked`.

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
Each checked object SHALL get one status: `verified`, `unsigned`, `unknown_key`, `ambiguous_key`, `bad_signature`, `unsupported_format` (OpenPGP, X.509 or an unsupported key type), `unverifiable` (identity or proof could not be fetched, or a proof did not verify, after retries) or `not_checked`.

#### Scenario: OpenPGP signature
- **WHEN** a commit carries a `-----BEGIN PGP SIGNATURE-----` signature
- **THEN** its status is `unsupported_format`

#### Scenario: No signature
- **WHEN** a commit has no `gpgsig` header
- **THEN** its status is `unsigned`

### Requirement: The signature is an SSH signature in namespace git
A signature SHALL verify only when it is an armored SSHSIG over the object without its signature (a commit without its `gpgsig` header; a tag up to its signature), in namespace `git`, hashed with SHA-256 or SHA-512, by the key it names. For `sk-` keys the user-presence flag SHALL be set.

#### Scenario: Altered message
- **WHEN** a signed commit's message is changed and the signature kept
- **THEN** its status is `bad_signature`

#### Scenario: Wrong namespace
- **WHEN** a commit is signed in namespace `file`
- **THEN** its status is `bad_signature`

### Requirement: The key is found by fingerprint
The forge SHALL look up the signature key's fingerprint in the signing-key index. When no DID has it and the commit's committer email is a DID, the forge SHALL refresh that DID's keys and look again. No match SHALL give `unknown_key`.

#### Scenario: Committer named by DID
- **WHEN** an unknown person pushes a commit whose committer email is their DID and whose key they publish
- **THEN** the forge indexes their keys and the commit is `verified`

### Requirement: One signer per key
When the committer email is a DID, the commit SHALL verify only as that DID. Otherwise, when several DIDs publish the key, the forge SHALL choose the only one allowed by the signing rule in force when the update was pushed, else give `ambiguous_key` listing the DIDs. `verified` SHALL mean the key is published by the signer DID, not that the DID proved holding it.

#### Scenario: Copied key
- **WHEN** Mallory publishes Alice's public key and Alice's commit names Alice's DID as committer
- **THEN** the commit is `verified` as Alice

#### Scenario: Committer DID does not publish the key
- **WHEN** a commit names Alice's DID as committer but is signed by a key only Mallory publishes
- **THEN** its status is `unknown_key`

#### Scenario: No way to choose
- **WHEN** two DIDs publish the key and the commit names neither
- **THEN** its status is `ambiguous_key`

### Requirement: The key record is proved back to the DID
Before `verified`, the forge SHALL fetch `com.atproto.sync.getRecord` for the key's record, check every block's hash, check the repository commit's DID and its low-S signature by the DID's verified `#atproto` key, walk the MST to the record and require the record to hold the key with signing usage. A signed MST proof that the record does not exist SHALL be conclusive: the key is marked removed and, with no other record of it, the result is `unknown_key`.

#### Scenario: PDS proves the record absent
- **WHEN** `getRecord` answers 200 with a validly signed commit and a complete MST path without the record's leaf
- **THEN** the key is marked removed and the commit is `unknown_key` at once, not `unverifiable`

#### Scenario: PDS lies
- **WHEN** a PDS returns a CAR whose record block does not match the CID in the signed MST
- **THEN** the commit is not `verified`

#### Scenario: Record deleted
- **WHEN** the record no longer exists in the signer's repository at push time, whether the PDS proves it absent or answers `RecordNotFound`
- **THEN** the key is removed from the index and the commit is `unknown_key`

#### Scenario: Repository deactivated
- **WHEN** the signer's PDS answers `getRecord` with `RepoDeactivated`
- **THEN** the commit is `unknown_key` with that detail and the signer's account status is refreshed

### Requirement: A proof that does not verify is not final
When the repository commit's signature or the MST proof of a key record fails, the forge SHALL drop its cached proofs for the DID, refresh the DID's identity without any cache, and retry the proof once. A proof that still fails SHALL be a transient failure, never `bad_signature` or `unknown_key`. A record that no longer holds the key, or whose usages exclude `sign`, SHALL give `unknown_key` once the DID's keys are refreshed and no other record holds the key.

#### Scenario: Key rotated while the PDS lags
- **WHEN** a signer's `#atproto` key is rotated in PLC and their PDS serves a commit signed with the old key for one attempt before re-signing
- **THEN** the check refreshes the identity, ends `verified`, and never records the commit as `bad_signature`

#### Scenario: Proof never verifies
- **WHEN** every proof the signer's PDS serves fails against the DID's freshly verified `#atproto` key for an hour
- **THEN** the commit is recorded `unverifiable` with a reason naming the PDS and the DID, and an owner can re-check it

#### Scenario: Record no longer holds the key
- **WHEN** the indexed record now holds a different key and no other record of the DID publishes the signature's key
- **THEN** the signer's keys are refreshed and the commit is `unknown_key` with a detail naming the record

### Requirement: Results are kept with their proof
Each result SHALL be stored once with the object id, status, signer DID, fingerprint, record and the digest of a proof bundle in the object store (`proofs/sha256/<hex>.json`) holding the getRecord CAR, the identity evidence and the times. A stored result SHALL never change except `unverifiable`, which an owner may re-check; a re-check SHALL judge again every update that listed the object, each by the rule it was first judged by.

#### Scenario: Owner re-checks an unverifiable commit
- **WHEN** an `unverifiable` commit was judged on `main` and on `release/1.0`, the signing rule then changes, and an owner re-checks the commit once the signer's PDS is back
- **THEN** both updates are judged again by the rules copied when they were pushed, and if `main`'s head update is now compliant its configuration is applied

#### Scenario: Offline re-check
- **WHEN** an auditor downloads a verified commit's proof bundle
- **THEN** they can verify the record proof and, for a `did:plc` signer, the DID evidence without contacting the network; for `did:web` the bundle records the document as fetched over TLS

### Requirement: Later key changes annotate results
When a key that verified a stored result is later removed, or the signer's DID is deactivated, reads of the result SHALL show it as `verified` with `key_removed_at` or `did_deactivated_at`, not as a different status.

#### Scenario: Key removed after the push
- **WHEN** Alice deletes her key a week after her commit was verified
- **THEN** the commit still shows verified as Alice, with the date the key was removed

### Requirement: Each check is recorded
After checking a ref update the forge SHALL record `ref.signatures.checked {ref, old, new, commits: [{sha, status, did}], truncated}` on the repository's source, in the same transaction as the stored results.

#### Scenario: Event
- **WHEN** a push adds two signed commits to `main`
- **THEN** one `ref.signatures.checked` lists both with their statuses

### Requirement: Transient failures are retried
When the git host, a DID document or a proof cannot be fetched, or a proof does not verify after a fresh identity, the forge SHALL retry with a growing delay for up to one hour before giving up, keep the last failure's reason (without secrets) with the queued check, and keep checking other updates, including later updates of the same ref. Retries SHALL use one durable schedule per repository that wakes it, and checks SHALL never run concurrently.

#### Scenario: PDS down briefly
- **WHEN** the signer's PDS is unreachable for five minutes after a push
- **THEN** the commit is checked once it is back and is `verified`

#### Scenario: Retry while the forge is idle
- **WHEN** an attempt fails and no other request reaches the repository until its next attempt is due
- **THEN** a schedule registered at that attempt's `next_at` wakes the repository and the attempt runs

#### Scenario: Many queued checks fail together
- **WHEN** a push queues 1500 tag updates while the git host is failing
- **THEN** the repository has at most one pending signature-check schedule, and a failed event delivery can still schedule its retry

#### Scenario: Overlapping wakes
- **WHEN** two signature-check wakes of one repository overlap
- **THEN** each ref update records `ref.signatures.checked` once

### Requirement: A check that gives up still judges its update
A check that gives up (after an hour, or at once when the git host no longer serves `new`) SHALL store the objects it completed as they are and those still failing as `unverifiable`, judge the update with them (on the tip alone when the git host never listed the update's commits), and record the update's result and events. An owner's re-check SHALL queue the update again and check every commit it adds.

#### Scenario: Object gone from the host
- **WHEN** a ref is force-pushed again before its previous update is checked and the old `new` is no longer served
- **THEN** that update's head is recorded `unverifiable` at once with a reason naming the missing object, and the update is judged with it

#### Scenario: Git host down for an hour
- **WHEN** the git host fails every attempt for an hour after a push to a covered ref
- **THEN** the tip is recorded `unverifiable`, the update is judged non-compliant, and an owner's re-check queues the update again and checks every commit it adds

### Requirement: Signatures are readable
`GET /-/api/orgs/<org>/repos/<repo>/commits/<sha>/signature` SHALL answer the stored result with its annotations and a proof URL, and `GET /-/api/orgs/<org>/repos/<repo>/signatures?ref=<ref>` the result and compliance of the ref's latest update. While the commit's check, or the ref's latest update, is queued, they SHALL answer `{status: "pending", attempts, next_at, last_error}`.

#### Scenario: Older update finishes last
- **WHEN** `main` moves to W and then to X, and W's check finishes after X's
- **THEN** the ref's result and the repository page report X's result, and W's result is kept only for W's update

#### Scenario: Unchecked commit
- **WHEN** a client asks for a commit the forge has neither checked nor queued
- **THEN** the API answers 404

#### Scenario: Check pending
- **WHEN** a client asks for a just-pushed head whose signer's PDS is unreachable
- **THEN** the API answers `pending` with the attempts so far and the last error, and the repository page shows that it is retrying

### Requirement: The signing rule is enforced after the push
When the rule in force before a push covers an updated ref, every object the update lists, including the commit `new` resolves to, SHALL be judged on its stored result against this ref's `signers`. A result made on an earlier update SHALL count only while its DID still publishes the key for signing and is not inactive. Any other object, or a truncated walk, SHALL record `ref.signature.violation` and mark the ref non-compliant until a later update to it is compliant.

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
- **WHEN** `main` requires signatures and is force-pushed back to an ancestor verified as an allowed signer whose key is still published
- **THEN** the update is compliant

#### Scenario: Key removed before the fast-forward
- **WHEN** a commit signed with Alice's key is verified on an uncovered branch, Alice's key record is then deleted, and `main`, which requires Alice, is fast-forwarded to it
- **THEN** `ref.signature.violation` is recorded for `main` with the problem `<oid> was verified with a key since removed`, its configuration is refused, and the commit itself still reads `verified` with the removal noted

#### Scenario: Signer deactivated before the fast-forward
- **WHEN** a commit verified as Alice on an uncovered branch reaches `main`, which requires Alice, after Alice's account is deactivated
- **THEN** `ref.signature.violation` is recorded for `main` with the problem `<oid> signer's account is not active`

#### Scenario: Tag of a tree
- **WHEN** a covered tag ref is pushed pointing, through annotated tags, at a tree rather than a commit
- **THEN** the tip is `not_checked` and the ref is not compliant

#### Scenario: Rule removed in the same push
- **WHEN** a push to `main` removes the signing rule and adds an unsigned commit
- **THEN** the rule in force before the push applies and the violation is recorded
