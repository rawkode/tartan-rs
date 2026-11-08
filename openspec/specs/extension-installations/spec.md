# extension-installations Specification

## Purpose
An installation is a published package installed on an org or a repository, with its own actor and database, called over the API.

## Requirements

### Requirement: Installing through the API
`POST /-/api/orgs/<org>/extensions` and `POST /-/api/orgs/<org>/repos/<repo>/extensions {"package", "version", "config"}` SHALL install a published package of the matching scope: the installation's migrations run, `init` is called, its subscriptions are registered and `extension.installed` is recorded. Installing the same extension again SHALL return the existing installation.

#### Scenario: Install on a repository
- **WHEN** Issues is installed on `acme/api`
- **THEN** the API answers 201 and `GET …/extensions` lists it

### Requirement: Calling an installation
`POST …/extensions/<id>/tools/<tool>`, `…/render/<slot>` and `…/actions/<action>` SHALL call the installation, acting as the caller. Extension errors SHALL map to HTTP statuses: `denied` 403, `not_found` 404, `invalid` 400, `conflict` 409, `unavailable` 503, otherwise 502, with the extension's message and kind.

#### Scenario: Tool call
- **WHEN** Issues' `create` tool is called with a title
- **THEN** the issue is created in the installation's database and returned

#### Scenario: Extension refusal
- **WHEN** Issues' `create` is called without a title
- **THEN** the API answers 400 with kind `invalid`

#### Scenario: Not installed
- **WHEN** a tool of an extension that is not installed on the repository is called
- **THEN** the API answers 404
