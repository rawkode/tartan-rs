## MODIFIED Requirements

### Requirement: Installations come only from configuration
An installation SHALL exist only because a repository's committed configuration names it (see `config-reconciliation`). `GET /-/api/tree/<repo path>/-/extensions` SHALL list the repository's enabled installations.

#### Scenario: Listing
- **WHEN** a repository's configuration installs Issues
- **THEN** `GET …/-/extensions` lists `issues.tartan.rawkode.academy` with its version
