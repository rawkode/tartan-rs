# forge-hierarchy Specification

## Purpose
Orgs and repositories: how they are named, created and found, and who may call the forge API.

## Requirements

### Requirement: Slugs name orgs and repositories
An org or repository slug SHALL be 1 to 64 lowercase letters, digits and `-`, and SHALL NOT start or end with `-`.

#### Scenario: Invalid slug
- **WHEN** an org is created with the slug `Not Valid`
- **THEN** the API answers 400 and nothing is created

### Requirement: Creating an org or repository is idempotent
Creating an org, or a repository in an org, SHALL return the existing one when the slug is already taken, and SHALL create the repository on the Git host before the forge reports it.

#### Scenario: Org created twice
- **WHEN** `POST /-/api/orgs {"slug": "acme"}` is sent twice
- **THEN** both answers carry the same org id

#### Scenario: Repository created on the Git host
- **WHEN** `POST /-/api/orgs/acme/repos {"slug": "api"}` succeeds
- **THEN** the Git host has the repository `acme/api` and the answer carries its clone URL

#### Scenario: Unknown repository
- **WHEN** a client asks for `acme/nope`
- **THEN** the API answers 404

### Requirement: The forge API requires the admin token
Every `/-/api` route SHALL require `Authorization: Bearer <admin token>` and SHALL answer 401 without it. Until identities exist, every API call acts as the principal `admin`.

#### Scenario: Missing token
- **WHEN** a request to `/-/api/orgs` has no bearer token
- **THEN** the API answers 401

### Requirement: A forge id separates forges sharing a Rivet namespace
The forge's singleton actors (the forge actor and its subscribers) SHALL be keyed by the forge id (`TARTAN_FORGE_ID`, default `forge`), so several forges can share one Rivet namespace.

#### Scenario: Two forges, one engine
- **WHEN** two forges with different ids run against the same Rivet engine
- **THEN** neither sees the other's orgs or subscribers
