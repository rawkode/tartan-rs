# Spec Delta

## MODIFIED Requirements

### Requirement: Installations come only from configuration
An installation SHALL exist only because a repository's committed configuration names it (see `config-reconciliation`). `GET /-/api/orgs/<org>/repos/<repo>/extensions` SHALL list the repository's enabled installations.

#### Scenario: Listing
- **WHEN** a repository's configuration installs Issues
- **THEN** `GET …/extensions` lists `rawkode.issues` with its version
