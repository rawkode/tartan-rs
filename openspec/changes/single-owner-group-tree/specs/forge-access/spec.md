## ADDED Requirements

### Requirement: Participants are set in the deployment configuration
The deployment configuration's `participants` SHALL decide which non-owners may participate: `"owners"` (the default) admits none, `"signed_in"` admits every signed-in person, and a list admits those DIDs. A participant SHALL also read, whatever `readers` says. Participating SHALL be limited to calls of tools whose manifest declares `access: "participate"`, which the forge SHALL run with the participant as the viewer and actor. A participant SHALL NOT create, change or place groups or repositories, install or configure extensions, call a tool declared `access: "write"`, or create access tokens beyond a reader's `read` tokens. At startup the forge SHALL log the participation mode in force.

#### Scenario: Upgraded forge stays closed
- **WHEN** a forge starts with a configuration that has `owners` but no `participants`
- **THEN** it logs that only owners participate, and a non-owner who signs in cannot open issues

#### Scenario: Visitor opens an issue
- **WHEN** `participants` is `"signed_in"` and a signed-in non-owner calls Issues' `create` tool
- **THEN** the issue is created with that person as its author

#### Scenario: Visitor comments
- **WHEN** `participants` is `"signed_in"` and a signed-in non-owner calls Issues' `comment` tool on an issue
- **THEN** the comment is added with that person as its author

#### Scenario: Visitor closes an issue
- **WHEN** a participant calls Issues' `close` tool, declared `access: "write"`
- **THEN** the forge answers 403 and nothing changes

#### Scenario: Visitor creates a repository
- **WHEN** a participant calls `POST /-/api/tree {"kind": "repo", "slug": "mine"}`
- **THEN** the forge answers 403 and nothing is created

#### Scenario: Listed participants
- **WHEN** `participants` is `["did:plc:bbbbbbbbbbbbbbbbbbbbbbbb"]` and another signed-in non-owner calls `create`
- **THEN** the forge answers 403

#### Scenario: Participant reads a private forge
- **WHEN** `readers` is `"owners"`, `participants` is `"signed_in"`, and a signed-in non-owner lists the tree
- **THEN** the tree is returned

### Requirement: Participation is rate-limited
Participant calls of `participate` tools SHALL be rate-limited per DID, and a call beyond the limit SHALL answer 429 with `Retry-After` without reaching the extension. Owners SHALL NOT be limited.

#### Scenario: Flood of issues
- **WHEN** a participant sends more `create` calls at once than the burst allows
- **THEN** the calls beyond the burst answer 429 with `Retry-After` and create no issue

### Requirement: Only owners make changes beyond participation
Writes (extension `tool` and `act` calls of tools declared `access: "write"`) SHALL need an owner, as SHALL every change to the tree and to extensions. Creating groups, repositories, placements and subscribers SHALL need `admin` rights. The only writes a non-owner may make are those of `participate` tools by a participant.

#### Scenario: Reader creates a group
- **WHEN** a reader who is not an owner calls `POST /-/api/tree`
- **THEN** the forge answers 403 and creates nothing

## MODIFIED Requirements

### Requirement: What callers may do
Anonymous callers SHALL get 401 from `/-/api` except `GET /-/api/me`. Readers (non-owners the configuration's `readers` admits) SHALL only read: `GET` routes, extension `render` calls, and calls of the tools an extension's manifest declares `access: "read"`, which the forge SHALL run read-only (no writes, no effects, and calls to other installations only of their read tools, also read-only). Participants SHALL do what readers do and call tools declared `access: "participate"`. Owners SHALL do everything their token's scopes allow. A tool's `access` SHALL be `read`, `participate` or `write`.

#### Scenario: Person who is not an owner
- **WHEN** a signed-in reader who is not an owner calls an Issues tool that creates an issue
- **THEN** the forge answers 403 and nothing changes

#### Scenario: Reading
- **WHEN** a signed-in reader who is not an owner lists a group's children
- **THEN** the list is returned

#### Scenario: Reader using a read tool
- **WHEN** a signed-in reader who is not an owner calls Issues' `list` tool or Kanban's `board` tool (declared `access: "read"`)
- **THEN** the answer is returned, and a call of a tool not declared `access: "read"` (such as Kanban's `move`) answers 403 with the read-only explanation

#### Scenario: Non-owner on a private forge
- **WHEN** `readers` is unset or `"owners"`, `participants` does not admit them, and a signed-in person who is not an owner lists a group's children
- **THEN** the forge answers 403, and `GET /-/api/me` answers their DID with `rights: []`

#### Scenario: Non-owner on an open forge
- **WHEN** `readers` is `"signed_in"` and a signed-in person who is not an owner lists a group's children
- **THEN** the list is returned

#### Scenario: Ending one's own session without rights
- **WHEN** a signed-in person with `rights: []` ends their CLI session with `DELETE /-/api/sessions/current`
- **THEN** the session ends

#### Scenario: Write token
- **WHEN** an owner's token with scope `write` but not `admin` creates a group
- **THEN** the forge answers 403

### Requirement: The web UI shows read-only access
For a principal whose rights from `GET /-/api/me` are empty, the web UI SHALL show a private-forge page instead of the forge. For one whose rights hold `read` but lack `write`, it SHALL hide or disable its write controls, except the controls of `participate` tools when its rights hold `participate`; show "Read-only: only the forge owner can make changes." (or, for a participant, "You can open issues and comment; only the forge owner can make other changes."); and show that message for a 403 from a write call instead of the response body. The owner-only controls (creating groups and repositories) SHALL show for owners only.

#### Scenario: Non-owner opens the issues page
- **WHEN** a signed-in person who is neither an owner nor a participant opens a repository's issues
- **THEN** there is no usable New issue button, the read-only notice is shown, and a forced write shows the read-only message rather than the raw 403 text

#### Scenario: Participant opens the issues page
- **WHEN** a participant opens a repository's issues
- **THEN** New issue and Comment work, no close, reopen or edit control shows, and the participant notice is shown

#### Scenario: Owner sees write controls
- **WHEN** an owner opens the same page
- **THEN** every control works and no read-only notice is shown

#### Scenario: Owner-only tree controls
- **WHEN** a participant opens a group
- **THEN** no control to create a group or repository shows, and an owner sees both

#### Scenario: Private forge page
- **WHEN** a signed-in person whose `GET /-/api/me` rights are empty opens any forge page
- **THEN** the UI shows "This forge is private; ask the owner for access." with their handle and a sign-out button, and no raw 403

### Requirement: Readers are set in the deployment configuration
The deployment configuration's `readers` SHALL decide which non-owners may read: `"owners"` (the default) admits none, `"signed_in"` admits every signed-in person, and a list admits those DIDs. Participants SHALL read too. At startup the forge SHALL log the mode in force. A sign-in SHALL record `person.signed_in` and make the DID known only when the DID may read or participate.

#### Scenario: Upgraded forge stays private
- **WHEN** a forge starts with a configuration that has `owners` but no `readers`
- **THEN** it logs that only owners can read, and a non-owner who signs in reads nothing

#### Scenario: Listed reader
- **WHEN** `readers` is `["did:plc:bbbbbbbbbbbbbbbbbbbbbbbb"]` and that DID signs in
- **THEN** it may read but not write, and another non-owner gets 403

#### Scenario: Stranger signs in to a private forge
- **WHEN** a DID with no read or participate right signs in
- **THEN** it gets a session, but no `person.signed_in` is recorded and the forge does not watch the DID

### Requirement: Session writes come from the forge's own pages
A request authenticated by the session cookie with a method other than `GET` or `HEAD` SHALL be refused with 403 unless its `Origin` header is the origin of `TARTAN_PUBLIC_URL`, the browser-facing origin that serves the web UI and proxies `/-/` to the gateway. Requests authenticated by a bearer token SHALL not need it.

#### Scenario: Cross-site form
- **WHEN** another site posts a form to `/-/api/tree` with the person's cookie
- **THEN** the forge answers 403

#### Scenario: Misconfigured public URL
- **WHEN** a form POST to `/-/auth/login` arrives with an `Origin` other than that of `TARTAN_PUBLIC_URL`
- **THEN** the forge answers 403 with a message naming the expected origin and logs a warning

#### Scenario: Web UI behind the proxy
- **WHEN** the web UI at `TARTAN_PUBLIC_URL` sends a write through its proxy to the gateway with the person's cookie
- **THEN** the `Origin` check passes

### Requirement: Other signed-in people
Signed-in non-owners whom neither `readers` nor `participants` admits SHALL get 403 except `GET /-/api/me`, which reports `rights: []`, their own sessions at `/-/api/sessions`, and the `self` routes.

#### Scenario: Private forge
- **WHEN** a signed-in non-owner of a forge with `readers: "owners"` and `participants: "owners"` calls `GET /-/api/tree`
- **THEN** the forge answers 403, and `GET /-/api/me` answers `rights: []`

## REMOVED Requirements

### Requirement: Changes need an owner
**Reason**: Restated with the participant exception and the tree API.
**Migration**: See "Only owners make changes beyond participation".
