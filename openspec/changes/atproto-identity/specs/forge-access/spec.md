# Spec Delta

## Purpose

Who is calling the forge (a signed-in person, an agent acting for one, or nobody), who the forge's owners are, and what each caller may do.

## ADDED Requirements

### Requirement: Every request has a principal
The gateway SHALL resolve each request to a principal from a bearer token (`tartan_pat_` access token or `tartan_cli_` CLI session), else from the session cookie, else anonymous. An invalid, expired or revoked bearer token SHALL be refused with 401 even when a valid cookie is present.

#### Scenario: Bad token with a good cookie
- **WHEN** a request carries a revoked token and a valid session cookie
- **THEN** it is refused with 401

### Requirement: Owners are the DIDs in the deployment configuration
A principal SHALL be an owner when its DID, or the DID it acts on behalf of, is listed in the deployment configuration's `owners`. Owners SHALL change only when the forge restarts with a new configuration.

#### Scenario: Owner signs in
- **WHEN** a person whose DID is in `owners` signs in
- **THEN** `GET /-/api/me` answers their DID, handle and `owner: true`

### Requirement: What callers may do
Anonymous callers SHALL get 401 from `/-/api` except `GET /-/api/me`. Readers (non-owners the configuration's `readers` admits) SHALL only read: `GET` routes, extension `render` calls, and calls of the tools an extension's manifest declares `access: "read"`, which the forge SHALL run read-only (no writes, no effects, and calls to other installations only of their read tools, also read-only).

#### Scenario: Person who is not an owner
- **WHEN** a signed-in reader who is not an owner calls an Issues tool that creates an issue
- **THEN** the forge answers 403 and nothing changes

#### Scenario: Reading
- **WHEN** a signed-in reader who is not an owner lists an org's repositories
- **THEN** the list is returned

#### Scenario: Reader using a read tool
- **WHEN** a signed-in reader who is not an owner calls Issues' `list` tool or Kanban's `board` tool (declared `access: "read"`)
- **THEN** the answer is returned, and a call of a tool not declared `access: "read"` (such as Kanban's `move`) answers 403 with the read-only explanation

#### Scenario: Non-owner on a private forge
- **WHEN** `readers` is unset or `"owners"` and a signed-in person who is not an owner lists an org's repositories
- **THEN** the forge answers 403, and `GET /-/api/me` answers their DID with `rights: []`

#### Scenario: Non-owner on an open forge
- **WHEN** `readers` is `"signed_in"` and a signed-in person who is not an owner lists an org's repositories
- **THEN** the list is returned

#### Scenario: Ending one's own session without rights
- **WHEN** a signed-in person with `rights: []` ends their CLI session with `DELETE /-/api/sessions/current`
- **THEN** the session ends

#### Scenario: Write token
- **WHEN** an owner's token with scope `write` but not `admin` creates an org
- **THEN** the forge answers 403

### Requirement: Signers reach their own signature records
`POST /-/api/people/<did>/signatures/<rkey>/refresh` SHALL be allowed to any principal whose DID is `<did>`, whatever its rights, including readers and people with `rights: []`; owners MAY call it without a body for any DID. A body with `withdraw` SHALL be accepted only from a browser or CLI session; every `tartan_pat_` token SHALL get 403 for it, whatever its scopes. Calls that insert a row or fetch SHALL be rate-limited per DID; withdrawing a tracked record SHALL NOT be.

#### Scenario: Non-reader signer withdraws
- **WHEN** `readers` is `"owners"` and a signed-in person who is not an owner withdraws their own signature record with a CLI session
- **THEN** the record is marked revoked as `withdrawn`

#### Scenario: Reader signer withdraws
- **WHEN** a reader withdraws their own signature record from a browser session
- **THEN** the record is marked revoked as `withdrawn`

#### Scenario: Access token cannot withdraw
- **WHEN** a person's `tartan_pat_` token with scopes `read`, `write` and `admin` calls the route for that person's own record with `withdraw`
- **THEN** the forge answers 403 and nothing is revoked

#### Scenario: Re-proofs are rate-limited
- **WHEN** a person with `rights: []` sends 101 body-less re-proofs of their own records at once
- **THEN** the calls beyond the burst answer 429 with `Retry-After`, and the forge makes no fetch for them

#### Scenario: Another person's record
- **WHEN** a signed-in non-owner calls the route for another DID's record
- **THEN** the forge answers 403

### Requirement: The web UI shows read-only access
For a principal whose rights from `GET /-/api/me` are empty, the web UI SHALL show a private-forge page instead of the forge. For one whose rights hold `read` but lack `write`, it SHALL hide or disable its write controls, show "Read-only: only forge owners can make changes until membership exists.", and show that message for a 403 from a write call instead of the response body.

#### Scenario: Non-owner opens the issues page
- **WHEN** a signed-in person who is not an owner opens a repository's issues
- **THEN** there is no usable New issue button, the read-only notice is shown, and a forced write shows the read-only message rather than the raw 403 text

#### Scenario: Owner sees write controls
- **WHEN** an owner opens the same page
- **THEN** the New issue button works and no read-only notice is shown

#### Scenario: Private forge page
- **WHEN** a signed-in person whose `GET /-/api/me` rights are empty opens any forge page
- **THEN** the UI shows "This forge is private; ask an owner for access." with their handle and a sign-out button, and no raw 403

### Requirement: Readers are set in the deployment configuration
The deployment configuration's `readers` SHALL decide which non-owners may read: `"owners"` (the default) admits none, `"signed_in"` admits every signed-in person, and a list admits those DIDs. At startup the forge SHALL log the mode in force. A sign-in SHALL record `person.signed_in` and make the DID known only when the DID may read.

#### Scenario: Upgraded forge stays private
- **WHEN** a forge starts with a configuration that has `owners` but no `readers`
- **THEN** it logs that only owners can read, and a non-owner who signs in reads nothing

#### Scenario: Listed reader
- **WHEN** `readers` is `["did:plc:bbbbbbbbbbbbbbbbbbbbbbbb"]` and that DID signs in
- **THEN** it may read but not write, and another non-owner gets 403

#### Scenario: Stranger signs in to a private forge
- **WHEN** a DID with no read right signs in
- **THEN** it gets a session, but no `person.signed_in` is recorded and the forge does not watch the DID

### Requirement: Inactive accounts are refused
The forge SHALL refuse sessions and tokens of a DID whose verified account status is inactive (any status but active, throttled or desynchronized), within 30 seconds of learning it. Status SHALL come only from the DID's verified PLC log and `com.atproto.sync.getRepoStatus` on its verified PDS, refreshed on Jetstream identity or account events, at sign-in and at least every 6 hours; a Jetstream event's payload SHALL never set it, and a failed fetch SHALL keep the last status.

#### Scenario: Takendown signer
- **WHEN** a person's PDS reports their account `takendown`
- **THEN** their sessions and tokens are refused, and their newly checked commits are `signer_inactive`

#### Scenario: Forged account event
- **WHEN** Jetstream reports a person's account as `takendown` but their PDS reports it active
- **THEN** their session and tokens still work and their commits still verify

#### Scenario: Reactivated
- **WHEN** a person's account was inactive and their PDS later reports it active
- **THEN** their session and tokens are accepted again after the next status refresh

#### Scenario: Throttled account
- **WHEN** a person's PDS reports their account `throttled` or `desynchronized`
- **THEN** their session and tokens still work and their commits still verify, and the status is shown as a note

#### Scenario: PDS unreachable
- **WHEN** an active person's PDS cannot be reached during a status refresh
- **THEN** their status stays active

### Requirement: Session writes come from the forge's own pages
A request authenticated by the session cookie with a method other than `GET` or `HEAD` SHALL be refused with 403 unless its `Origin` header is the origin of `TARTAN_PUBLIC_URL`, the browser-facing origin that serves the web UI and proxies `/-/` to the gateway. Requests authenticated by a bearer token SHALL not need it.

#### Scenario: Cross-site form
- **WHEN** another site posts a form to `/-/api/orgs` with the person's cookie
- **THEN** the forge answers 403

#### Scenario: Misconfigured public URL
- **WHEN** a form POST to `/-/auth/login` arrives with an `Origin` other than that of `TARTAN_PUBLIC_URL`
- **THEN** the forge answers 403 with a message naming the expected origin and logs a warning

#### Scenario: Web UI behind the proxy
- **WHEN** the web UI at `TARTAN_PUBLIC_URL` sends a write through its proxy to the gateway with the person's cookie
- **THEN** the `Origin` check passes

### Requirement: The caller is passed on
Calls to extensions SHALL carry the request's principal as the viewer and actor, and events caused by an API call SHALL record that principal: a person as `{kind: user, id: <DID>}`, a token as `{kind: agent, id: "token:<id>", on_behalf_of: <DID>}`.

#### Scenario: Issue created by an agent
- **WHEN** an owner's agent token creates an issue
- **THEN** the issue's author is the agent on behalf of the owner's DID

### Requirement: The shared admin token is gone
The forge SHALL NOT accept a shared admin token, and SHALL refuse to start when `TARTAN_ADMIN_TOKEN` is set, naming `owners` as its replacement.

#### Scenario: Old environment
- **WHEN** the forge starts with `TARTAN_ADMIN_TOKEN=dev-admin-token`
- **THEN** it exits with an error telling the operator to list owners in the deployment configuration

### Requirement: Read-only calls cannot write
The database engine SHALL enforce that a read-only extension call writes nothing, whatever the statement's text.

#### Scenario: A write behind a WITH clause
- **WHEN** a tool declared `access: "read"`, called for a reader, runs `WITH a AS (SELECT 1) DELETE FROM t`
- **THEN** the statement fails and the installation's data is unchanged

### Requirement: Other signed-in people
Signed-in non-owners whom `readers` does not admit SHALL get 403 except `GET /-/api/me`, which reports `rights: []`, their own sessions at `/-/api/sessions`, and the `self` routes.

#### Scenario: Private forge
- **WHEN** a signed-in non-owner of a forge with `readers: "owners"` calls `GET /-/api/orgs`
- **THEN** the forge answers 403, and `GET /-/api/me` answers `rights: []`

### Requirement: Changes need an owner
Writes (extension `tool` and `act` calls) SHALL need an owner. Creating orgs, repositories, placements and subscribers SHALL need `admin` rights.

#### Scenario: Reader creates an org
- **WHEN** a reader who is not an owner calls `POST /-/api/orgs`
- **THEN** the forge answers 403 and creates nothing
