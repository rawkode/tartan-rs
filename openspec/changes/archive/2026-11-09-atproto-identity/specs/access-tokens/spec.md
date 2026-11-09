# Spec Delta

## Purpose

Scoped, expiring, revocable bearer tokens through which agents and automation act on behalf of a person's DID, never with more rights than that person.

## ADDED Requirements

### Requirement: A person creates tokens for themselves
A person signed in through the browser or `tt login` SHALL create a token with `POST /-/api/tokens {name, scopes, repos?, expires_in_days}`; the answer SHALL show the secret (`tartan_pat_` and 43 base64url characters) once. A token SHALL NOT create tokens. A DID SHALL hold at most 50 unexpired, unrevoked tokens.

#### Scenario: Create
- **WHEN** a signed-in person creates a token named `ci` with scope `read`
- **THEN** the answer carries the secret, and later reads of the token show its name, scopes and expiry but not the secret

#### Scenario: From the CLI
- **WHEN** a person signed in with `tt login` runs `tt token create`
- **THEN** the token is created for their DID

#### Scenario: Token creating a token
- **WHEN** a request authenticated by a token calls `POST /-/api/tokens`
- **THEN** it is refused with 403

### Requirement: Only a hash of the secret is stored
The forge SHALL store only the SHA-256 of a token's secret, and SHALL never write the secret to events, logs or problem details.

#### Scenario: Event payload
- **WHEN** a token is created
- **THEN** `token.created {id, did, name, scopes, repos, expires_at}` is recorded on the `identity` source without the secret

### Requirement: Tokens have scopes, an optional repository list and an expiry
Scopes SHALL be any of `read`, `write` and `admin`. `repos`, when given, SHALL list `org/repo` or `org/*`, and the token SHALL be refused with 403 on every route not addressed to a listed repository or, for reads, its org, including `POST /orgs`, `PUT /placements` and `/subscribers/**`, except `GET /-/api/me` and `GET /-/api/people`. The expiry SHALL be 1 to 365 days. Unknown scopes or malformed entries SHALL be refused with 400.

#### Scenario: Repository-limited token
- **WHEN** a token limited to `acme/web` reads `acme/api`'s extensions
- **THEN** it is refused with 403

#### Scenario: Repo-limited admin token creates an org
- **WHEN** an owner's `admin` token limited to `acme/web` calls `POST /orgs`, `PUT /placements` or reads a subscriber's event stream
- **THEN** each is refused with 403

#### Scenario: Repo-limited token asks who it is
- **WHEN** a token limited to `acme/web` calls `GET /-/api/me`
- **THEN** it answers the token's principal

#### Scenario: Too long
- **WHEN** a token is requested with `expires_in_days: 400`
- **THEN** it is refused with 400

### Requirement: A token acts as an agent on behalf of its DID
A request with `Authorization: Bearer tartan_pat_…` SHALL act as the principal `{kind: agent, id: "token:<id>", on_behalf_of: <DID>}`, with the rights of its scopes intersected with the rights its DID has at the time of the request.

#### Scenario: Owner removed
- **WHEN** a DID with an `admin` token is removed from `owners` and the forge restarts
- **THEN** the token can no longer create orgs

### Requirement: Tokens are revocable
The token's DID or any owner SHALL revoke it with `DELETE /-/api/tokens/<id>`, recording `token.revoked {id, did, by}`. A revoked or expired token SHALL be refused with 401 within 30 seconds of revocation in every gateway process.

#### Scenario: Revoked
- **WHEN** a token is revoked and used 31 seconds later
- **THEN** the request is refused with 401

### Requirement: Operators can mint tokens
`tt admin token create --did <DID> --name <name> --scopes <list> --expires-in-days <n>` SHALL create a token for a DID listed in `owners` and print its secret, so scripts can act before anyone signs in. It SHALL refuse a DID that is not an owner.

#### Scenario: Seeding the demo
- **WHEN** `scripts/demo.sh` mints an `admin` token for the development owner with `tt admin token create`
- **THEN** its seeding requests succeed with that token
