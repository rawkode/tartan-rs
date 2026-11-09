# Spec Delta

## Purpose

Who is calling the forge (a signed-in person, an agent acting for one, or nobody), who the forge's owners are, and what each caller may do.

## ADDED Requirements

### Requirement: Every request has a principal
The gateway SHALL resolve each request to a principal from a `tartan_pat_` bearer token, else from the session cookie, else anonymous. An invalid, expired or revoked bearer token SHALL be refused with 401 even when a valid cookie is present.

#### Scenario: Bad token with a good cookie
- **WHEN** a request carries a revoked token and a valid session cookie
- **THEN** it is refused with 401

### Requirement: Owners are the DIDs in the deployment configuration
A principal SHALL be an owner when its DID, or the DID it acts on behalf of, is listed in the deployment configuration's `owners`. Owners SHALL change only when the forge restarts with a new configuration.

#### Scenario: Owner signs in
- **WHEN** a person whose DID is in `owners` signs in
- **THEN** `GET /-/api/me` answers their DID, handle and `owner: true`

### Requirement: What callers may do
Anonymous callers SHALL get 401 from `/-/api` except `GET /-/api/me`. Other people SHALL only read: `GET` routes and extension `render` calls. Writes (extension `tool` and `act` calls) SHALL need an owner. Creating orgs and repositories, placements and subscribers SHALL need an owner with `admin` rights.

#### Scenario: Person who is not an owner
- **WHEN** a signed-in person who is not an owner calls an Issues tool that creates an issue
- **THEN** the forge answers 403 and nothing changes

#### Scenario: Reading
- **WHEN** a signed-in person who is not an owner lists an org's repositories
- **THEN** the list is returned

#### Scenario: Write token
- **WHEN** an owner's token with scope `write` but not `admin` creates an org
- **THEN** the forge answers 403

### Requirement: Inactive accounts are refused
The forge SHALL refuse sessions and tokens of a DID whose verified account status is inactive (any status but active, throttled or desynchronized), within 30 seconds of learning it. Status SHALL come only from the DID's verified PLC log and `com.atproto.sync.getRepoStatus` on its verified PDS, refreshed on Jetstream identity or account events, at sign-in and at least every 6 hours; a Jetstream event's payload SHALL never set it, and a failed fetch SHALL keep the last status.

#### Scenario: Takendown signer
- **WHEN** a person's PDS reports their account `takendown`
- **THEN** their session and tokens are refused, and their new commits are `unknown_key` with the account status as detail

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
