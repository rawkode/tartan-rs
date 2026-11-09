# Spec Delta

## Purpose

Where a person publishes the SSH keys they sign commits with (records in their own ATProto repository, in Tangled's lexicon so keys work across forges), and how the forge finds, indexes and refreshes them.

## ADDED Requirements

### Requirement: Signing keys are records in the person's repository
A signing key SHALL be an `org.tangled.key.sshKey` record (`name`, `publicKey` in OpenSSH authorized_keys format, optional `usages`, `createdAt`) whose `usages` include `sign` or are absent, or a legacy `sh.tangled.publicKey` record (`key`, `name`, `createdAt`). Records with other usages, or keys that do not parse, SHALL be ignored.

#### Scenario: Auth-only key
- **WHEN** a person's only key record has `usages: ["auth"]`
- **THEN** it is not a signing key

#### Scenario: Legacy record
- **WHEN** a person has only a `sh.tangled.publicKey` record holding an Ed25519 key
- **THEN** that key is one of their signing keys

### Requirement: Supported key types
The forge SHALL accept `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ecdsa-sha2-nistp384`, `ecdsa-sha2-nistp521`, `sk-ssh-ed25519@openssh.com` and `sk-ecdsa-sha2-nistp256@openssh.com` keys. RSA and DSA keys SHALL be listed as unsupported and never verify a signature.

#### Scenario: RSA key
- **WHEN** a person publishes an `ssh-rsa` key
- **THEN** their key list shows it as unsupported

### Requirement: The forge indexes the keys of people it knows
The forge SHALL index, by SHA-256 fingerprint, the signing keys of each DID it knows: owners, readers who have signed in, DIDs named by a repository's signing rule, and DIDs named as committers of pushed commits. The index SHALL be built from the person's PDS listing and SHALL only find candidates; verification always fetches a proof.

#### Scenario: Owner's keys
- **WHEN** the forge starts with an owner who has two signing keys
- **THEN** both fingerprints are in the index for that DID

### Requirement: Key changes are recorded
Indexing SHALL record `signing_key.added {did, fingerprint, collection, rkey, name}` for a signing key it had not seen and `signing_key.removed {did, fingerprint, collection, rkey}` for one no longer published for signing, including a record replaced in place under the same rkey, on the `identity` source.

#### Scenario: Key deleted
- **WHEN** a person deletes a key record and the forge refreshes their keys
- **THEN** `signing_key.removed` is recorded with the key's fingerprint

#### Scenario: Key record replaced in place
- **WHEN** a person rewrites a key record under the same rkey with a different `publicKey`, or with `usages: ["auth"]`, and the forge refreshes their keys
- **THEN** `signing_key.removed` is recorded for the old fingerprint (and `signing_key.added` for a new signing key), and commits signed with the old key no longer verify from a cached proof

### Requirement: The index follows the network
The forge SHALL refresh a known DID's keys when Jetstream (`TARTAN_JETSTREAM_URL`, empty to disable) reports a commit to either key collection or an identity event for it, when the person signs in or asks, and at least every 6 hours. Every Jetstream filter update SHALL carry both key collections with the full DID list. Commits to other collections and events for unknown DIDs SHALL be ignored. Jetstream events SHALL only trigger a refresh.

#### Scenario: Missed event
- **WHEN** Jetstream is unreachable while a person adds a key
- **THEN** the key is indexed by the next periodic refresh

#### Scenario: Quiet forge
- **WHEN** no request reaches the forge for a day while a known person adds a key and Jetstream is disabled
- **THEN** the identity actor's scheduled sweep wakes it and the key is indexed within 6 hours

#### Scenario: Forged event
- **WHEN** a Jetstream message claims a key record that the person's PDS does not list
- **THEN** no key is added

#### Scenario: Unrelated commit
- **WHEN** a known DID writes an `app.bsky.feed.post` record
- **THEN** no key refresh happens for it

#### Scenario: Filter kept across DID-set changes
- **WHEN** the forge learns a new DID and sends Jetstream an updated DID list
- **THEN** the update also carries both key collections, and a later post by a known DID still triggers no refresh

### Requirement: People see and manage their keys
`GET /-/api/people/<did>/keys` SHALL list a DID's indexed keys with name, type, fingerprint, collection and whether each is supported. A person SHALL refresh their own keys with `POST /-/api/people/<did>/keys/refresh`.

#### Scenario: Listing
- **WHEN** a signed-in person opens their signing-key settings
- **THEN** each published key is listed with its fingerprint

### Requirement: Keys can be added and removed from the forge
`POST /-/auth/keys` (a top-level form POST) with `{name, publicKey}`, or `{remove: rkey}` naming an indexed `org.tangled.key.sshKey` record of the signed-in DID, SHALL start an OAuth authorization asking `atproto` plus exactly one declared `repo:org.tangled.key.sshKey?action=create` or `?action=delete` token, and on its callback SHALL write or delete the record, discard the tokens and refresh the index. Every outcome SHALL redirect to the settings page with an outcome code.

#### Scenario: Add a key
- **WHEN** a person adds an Ed25519 key named `laptop` and approves the request at their PDS
- **THEN** an `org.tangled.key.sshKey` record with `usages: ["sign"]` exists in their repository and the key is indexed

#### Scenario: Scope refused
- **WHEN** the person's authorization server refuses the granular scope at PAR with `invalid_scope`, or grants a scope without the requested `repo:` token
- **THEN** the settings page shows `scope_refused` with how to publish the record with another ATProto client, not `client_rejected`, and nothing is written

#### Scenario: Cancelled at the server
- **WHEN** the person denies the key write at their authorization server
- **THEN** the settings page says the key was not added because they cancelled, and the person stays signed in

#### Scenario: PDS write fails
- **WHEN** the person approves but their PDS answers `createRecord` with an error
- **THEN** the settings page shows `write_failed` naming the PDS host and its message as text, and no key is indexed

#### Scenario: Key removed
- **WHEN** a person removes a key published only as an `org.tangled.key.sshKey` record and approves the request
- **THEN** the record is deleted, the index is refreshed and the settings page shows `removed`

#### Scenario: Key also published under the legacy collection
- **WHEN** a person removes a key that is also published as a `sh.tangled.publicKey` record and approves the request
- **THEN** the forge deletes the `org.tangled` record, the legacy record remains, and the settings page shows `still_published` naming the legacy record, not `removed`

#### Scenario: Legacy record offered for removal
- **WHEN** a person's key is published only as a `sh.tangled.publicKey` record
- **THEN** the settings page marks it as published by another client with no remove button, and a `remove` naming its rkey answers `not_removable` without an authorization

#### Scenario: Signed out while on the settings page
- **WHEN** a person whose session has expired submits the key form
- **THEN** the forge redirects to the sign-in page returning to the settings page, not a bare 401

#### Scenario: Too many pending requests
- **WHEN** the client address or the forge has reached its cap of pending authorizations
- **THEN** the settings page shows `rate_limited` or `busy` and no authorization is started

#### Scenario: Invalid key
- **WHEN** a person submits a public key that does not parse or is longer than 4096 bytes
- **THEN** the settings page shows `invalid_key` and no authorization is started

### Requirement: Key-write outcome details reach only the initiating session
Key-write outcome redirects SHALL carry only the outcome code and an outcome id, never a host, a PDS message, a collection or an rkey. The forge SHALL keep those details for at most 10 minutes and reveal them once through `POST /-/auth/outcome`, only to the session that started the write. Any other viewer SHALL see the generic message for the code.

#### Scenario: Crafted outcome link
- **WHEN** a signed-in person opens `/#/-/settings?keys=write_failed&host=evil.example&message=…`
- **THEN** the settings page shows the generic `write_failed` message and nothing from `host` or `message`

#### Scenario: Outcome read by another session
- **WHEN** a key-write redirect, outcome id included, is opened by a different session
- **THEN** the forge answers 404 for its details and the page shows the generic message for the code

### Requirement: Removal is judged by the refreshed index
After deleting a key record, the forge SHALL refresh the DID's keys and report `removed` only when the key's fingerprint is no longer published by that DID in either collection. Otherwise it SHALL report `still_published` naming the remaining record, or `refresh_failed` when the refresh could not list the PDS. A failed listing SHALL keep the indexed keys unchanged.

#### Scenario: Delete of a missing record
- **WHEN** the PDS answers `deleteRecord` with success but the key is still listed afterwards
- **THEN** the settings page shows `still_published`, not `removed`

#### Scenario: Listing fails after the delete
- **WHEN** the record is deleted but the following key listing fails
- **THEN** the settings page shows `refresh_failed`, and no `signing_key.removed` is recorded for keys still indexed

#### Scenario: Deactivated repository
- **WHEN** a known person's PDS answers the key listing with `RepoDeactivated`
- **THEN** their indexed keys are kept and no `signing_key.removed` events are recorded
