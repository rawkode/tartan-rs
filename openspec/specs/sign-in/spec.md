# sign-in Specification

## Purpose
Signing in to the forge with an ATProto identity through ATProto OAuth, and the forge session that follows, so people use the web UI as themselves.

## Requirements

### Requirement: The forge publishes confidential client metadata
The forge SHALL serve its OAuth client metadata at `<TARTAN_PUBLIC_URL>/-/auth/client-metadata.json` (its `client_id`), where `TARTAN_PUBLIC_URL` is the browser-facing origin serving the web UI and proxying `/-/` to the gateway, with `application_type` `web`, the callback `/-/auth/callback` as its only redirect URI, `response_types` `["code"]`, `grant_types` `["authorization_code"]`, `dpop_bound_access_tokens` true, `private_key_jwt` with ES256, and `jwks_uri` `/-/auth/jwks.json`.

#### Scenario: Metadata matches its URL
- **WHEN** an authorization server fetches the `client_id` URL
- **THEN** it receives `application/json` whose `client_id` equals that URL exactly

### Requirement: Sign-in asks only to prove the DID
The client metadata `scope` SHALL be exactly `atproto`, and every sign-in authorization request SHALL ask for exactly `atproto`: the forge never writes to a person's repository. The forge SHALL NOT declare or request any `repo:`, `rpc:` or `transition:` scope for its web client.

#### Scenario: Sign-in asks only for atproto
- **WHEN** a person starts signing in
- **THEN** the PAR request body the authorization server receives has `scope` exactly `atproto`, and the client metadata declares exactly `atproto`

### Requirement: The forge must be reachable by authorization servers
`TARTAN_PUBLIC_URL` SHALL be reachable over public HTTPS by the authorization servers of everyone who signs in, since they fetch the client metadata and JWKS themselves. A forge that is not reachable SHALL still start, and be administered with operator-minted tokens.

#### Scenario: Private forge
- **WHEN** a forge on a private network is asked to sign someone in
- **THEN** the person's authorization server refuses the client and the sign-in page reports `client_rejected`

### Requirement: The client keys are the operator's
The forge SHALL load its ES256 client keys from the JWKS file at `TARTAN_OAUTH_KEYS_FILE`, sign client assertions with the first key, publish the public halves of all of them, and refuse to start without at least one valid key. Private key material SHALL never appear in responses, events or logs.

#### Scenario: Published JWKS
- **WHEN** anyone fetches `/-/auth/jwks.json`
- **THEN** each key carries only public parameters and no `d` member

#### Scenario: Key rotation
- **WHEN** the operator puts a new key first in the file and restarts
- **THEN** new sign-ins use the new key and the old key stays published until removed

### Requirement: Sign-in follows the ATProto OAuth profile
`POST /-/auth/login {handle}` (a handle or DID) SHALL resolve the person, their PDS and its authorization server, check the server's `issuer`, push the request with PAR, PKCE S256, a random single-use `state`, a fresh DPoP key and scope `atproto`, and redirect to the authorization endpoint. Pending requests SHALL expire after 10 minutes.

#### Scenario: Start
- **WHEN** a person submits `alice.example` on the sign-in page
- **THEN** the forge answers 303 to the authorization server with only `client_id` and `request_uri` in the URL

#### Scenario: Unknown handle
- **WHEN** the handle does not resolve
- **THEN** the sign-in page shows that the handle was not found and no request is stored

#### Scenario: Server unreachable
- **WHEN** the PLC directory, the person's PDS or its authorization server cannot be reached
- **THEN** the sign-in page says the person's server could not be reached, naming the host, not that the handle was not found, and no request is stored

### Requirement: Sign-in failures are explained
Every sign-in failure SHALL redirect to the sign-in page with a problem code and the original `return_to`, never a raw 500. A callback carrying `error` SHALL show that sign-in was cancelled, with `error_description` as text. An expired `state` SHALL show that sign-in took too long. An OAuth client error from PAR or the token endpoint SHALL give `client_rejected` with the server's message escaped, telling the person to inform the operator; `server_unreachable` is only for connection failures.

#### Scenario: Consent denied
- **WHEN** the callback arrives with `error=access_denied` and no `code`
- **THEN** the pending request is consumed, no session is created, and the sign-in page says sign-in was cancelled

#### Scenario: Client refused
- **WHEN** the authorization server answers PAR with `invalid_client_metadata` because it cannot fetch the client metadata
- **THEN** the sign-in page shows `client_rejected` with the server's message as text and says the forge's operator should be told, not that the person's server is unreachable

#### Scenario: Too slow
- **WHEN** the callback arrives 11 minutes after sign-in started
- **THEN** the sign-in page says sign-in took too long and offers to try again

### Requirement: Inactive accounts cannot sign in
Before creating a session, the callback SHALL refresh the DID's account status from its verified PLC log and PDS. If the status is inactive, the forge SHALL create no session, record no `person.signed_in`, and redirect to the sign-in page with problem `account_inactive` and the status, keeping `return_to`, and SHALL word the refusal by status. A failed status fetch SHALL keep the stored status.

#### Scenario: Deactivated account
- **WHEN** the DID's PDS answers `getRepoStatus` with `active: false` and `status: deactivated` at the callback
- **THEN** no session is created, and the sign-in page shows `account_inactive`, saying the account at that PDS is deactivated and to reactivate it there and sign in again

#### Scenario: Taken-down account
- **WHEN** the DID's PDS reports the account `takendown` at the callback
- **THEN** no session is created, and the sign-in page says to contact the hosting provider, not to reactivate the account

#### Scenario: Stale inactive status
- **WHEN** the forge stored `deactivated` for a DID whose PDS now reports it active, and that DID signs in
- **THEN** the status becomes `active`, the session is created and the person is not sent back to the sign-in page

### Requirement: Failure details reach only the browser that ran the flow
Problem redirects SHALL carry only the problem code, a known account status, an outcome id and the validated `return_to`, never a host or a server's text. The forge SHALL keep hosts and server messages for at most 10 minutes, bound to an outcome cookie set with the redirect, and SHALL reveal them once through `POST /-/auth/outcome`, only to that browser. Any other browser SHALL see the generic message for the code.

#### Scenario: Crafted problem link
- **WHEN** someone opens `/#/-/sign-in?problem=account_inactive&status=deactivated&host=evil.example&error_description=…`
- **THEN** the page shows the fixed text for a deactivated account and nothing from `host` or `error_description`

#### Scenario: Details for the browser that ran the flow
- **WHEN** a person's sign-in fails with `client_rejected`
- **THEN** their sign-in page shows the authorization server's host and message once, read with their outcome cookie

#### Scenario: Real outcome link opened elsewhere
- **WHEN** that redirect URL, outcome id included, is opened in another browser
- **THEN** the page shows the generic `client_rejected` message and none of the server's text

### Requirement: Pending requests are bound to the browser
`POST /-/auth/login` SHALL set a pre-auth cookie (`__Host-tartan_oauth`; `tartan_oauth` in development over HTTP), HttpOnly, SameSite=Lax, Path=/, lasting at most 10 minutes, holding 32 random bytes whose SHA-256 the pending request stores. The callback SHALL fail unless that cookie matches. Every callback SHALL clear the cookie.

#### Scenario: Callback opened in another browser
- **WHEN** a callback carrying a valid `state` arrives from a browser without the matching pre-auth cookie
- **THEN** it fails and no session is created

### Requirement: Sign-in capacity is limited per client
The forge SHALL derive the client address from the peer, or, only when the peer is in `TARTAN_TRUSTED_PROXIES`, from the right-most `X-Forwarded-For` address that is not a trusted proxy. It SHALL rate-limit sign-in and handle resolution per address and allow 20 pending sign-ins per address and 10,000 in all. A refused sign-in SHALL redirect to the sign-in page with `rate_limited` (per address) or `busy` (global), keeping `return_to`.

#### Scenario: Spoofed header
- **WHEN** a client that is not a trusted proxy sends `X-Forwarded-For` with a different address
- **THEN** the header is ignored and the client is limited by its own address

#### Scenario: One client at its cap
- **WHEN** one client address has 20 sign-ins pending
- **THEN** its next sign-in redirects to the sign-in page with problem `rate_limited` and its `return_to`, saying to try again in a few minutes, and a sign-in from another address behind the same trusted proxy succeeds

#### Scenario: Forge-wide cap
- **WHEN** 10,000 sign-ins are pending across all addresses
- **THEN** the next sign-in redirects to the sign-in page with problem `busy`, not a bare 503

### Requirement: The callback proves the DID
`GET /-/auth/callback` SHALL accept only a pending `state` from the browser that started it, SHALL check `iss` against the stored issuer, exchange the code with DPoP and a client assertion, and require the token's `sub` to be the expected DID and its scope to include `atproto`. Any failure SHALL create no session.

#### Scenario: Wrong subject
- **WHEN** the token response's `sub` is a different DID from the one the sign-in started with
- **THEN** the callback fails and no session is created

#### Scenario: Replayed state
- **WHEN** a callback arrives with a `state` that was already used
- **THEN** it fails and no session is created

### Requirement: The forge keeps no OAuth tokens after sign-in
After a successful callback the forge SHALL discard the access and refresh tokens and the DPoP key, revoking the tokens when the authorization server advertises a revocation endpoint. Sign-in only proves the DID; the forge session that follows does not depend on the OAuth session.

#### Scenario: Revoking the app at the PDS
- **WHEN** a signed-in person revokes the forge in their PDS's settings
- **THEN** their forge session continues until it expires or they sign out

### Requirement: Forge sessions are cookies
A successful sign-in SHALL set an HttpOnly, SameSite=Lax cookie (`__Host-tartan_session`, Secure; `tartan_session` only in development over HTTP) holding 32 random bytes, of which the forge stores only the SHA-256. A session SHALL expire 30 days after sign-in or 7 days after its last use, whichever is first.

#### Scenario: Idle session
- **WHEN** a session has not been used for 8 days
- **THEN** a request with it is treated as anonymous

### Requirement: Sign-in returns to where it started
The sign-in page SHALL take a `return_to` path on `TARTAN_PUBLIC_URL`, including a fragment route such as `/#/acme/api/issues`. The callback SHALL redirect there only when it starts with a single `/`, not `//` or `/\`, has no control characters and parses as a path-absolute reference; otherwise it SHALL redirect to `/`.

#### Scenario: Open redirect
- **WHEN** a sign-in starts with `return_to=//evil.example/` or `return_to=/\evil.example`
- **THEN** the callback redirects to `/`

#### Scenario: Fragment route with a query
- **WHEN** a sign-in started from `/#/acme/api/issues?x=1` fails
- **THEN** the problem redirect carries `return_to` percent-encoded, and "try again" returns to `/#/acme/api/issues?x=1`

#### Scenario: Back to the page in development
- **WHEN** a person signs in from `/#/acme/api/issues` in the web UI of a development forge and then creates an issue
- **THEN** the browser lands on `/#/acme/api/issues` and the issue creation is accepted, because its `Origin` is `TARTAN_PUBLIC_URL`

### Requirement: Signing out ends the session
`POST /-/auth/logout` SHALL delete the session and clear the cookie. A person SHALL be able to list their own sessions and end any of them at `/-/api/sessions`.

#### Scenario: Sign out
- **WHEN** a signed-in person signs out
- **THEN** later requests with the old cookie are treated as anonymous

### Requirement: Sign-ins are recorded
A successful sign-in by a DID that may read (an owner or a reader) SHALL record `person.signed_in {did, handle}` on the `identity` source, and its first sign-in SHALL add it to the people the forge knows. A sign-in by a DID that may not read SHALL record neither.

#### Scenario: First sign-in
- **WHEN** a DID that may read signs in for the first time
- **THEN** `person.signed_in` is recorded and the forge starts watching the DID's identity and account status

### Requirement: Development sign-in clients
A `dev-identity` build SHALL, only when switched on, sign in as ATProto's `http://localhost` client (`TARTAN_DEV_LOCALHOST_CLIENT=1`) or as a public native client hosted at `TARTAN_DEV_CLIENT_METADATA_URL`. Either SHALL require `TARTAN_PUBLIC_URL` to be `http://127.0.0.1:<port>`, need no key file, and work with or without `TARTAN_DEV_IDENTITY`; a release build SHALL refuse to start with either set.

#### Scenario: Local forge signs in as a real DID
- **WHEN** a `dev-identity` forge at `http://127.0.0.1:4321` runs with `TARTAN_DEV_LOCALHOST_CLIENT=1` and without `TARTAN_DEV_IDENTITY`, and a person whose authorization server accepts the localhost client signs in
- **THEN** the PAR carries the `http://localhost?…` client id and no client assertion, and the person is redirected to their authorization server

#### Scenario: Server that accepts only https client ids
- **WHEN** the person's authorization server refuses the localhost client and the forge instead runs with `TARTAN_DEV_CLIENT_METADATA_URL` naming a hosted copy of its native client document
- **THEN** sign-in uses that `https` client id with the loopback redirect `http://127.0.0.1:4321/-/auth/callback`

#### Scenario: Release build
- **WHEN** a build without `dev-identity` starts with `TARTAN_DEV_LOCALHOST_CLIENT` or `TARTAN_DEV_CLIENT_METADATA_URL` set
- **THEN** it exits with an error naming the variable
