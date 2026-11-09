# Spec Delta

## REMOVED Requirements

### Requirement: The forge API requires the admin token
**Reason**: The shared admin token and the single `admin` principal are replaced by ATProto sign-in, access tokens and owners listed in the deployment configuration.
**Migration**: List administrators' DIDs in `owners` in the deployment configuration (package `forge`), remove `TARTAN_ADMIN_TOKEN`, sign in through the web UI, and give scripts a token from `tartan token create` or the access-token settings. See the `forge-access`, `sign-in` and `access-tokens` capabilities.

## MODIFIED Requirements

### Requirement: A forge id separates forges sharing a Rivet namespace
The forge's singleton actors (the forge actor, its subscribers and the `identity` actor) SHALL be keyed by the forge id (`TARTAN_FORGE_ID`, default `forge`), so several forges can share one Rivet namespace.

#### Scenario: Two forges, one engine
- **WHEN** two forges with different ids run against the same Rivet engine
- **THEN** neither sees the other's orgs or subscribers

#### Scenario: Separate identity state
- **WHEN** two forges with different ids run against the same Rivet engine
- **THEN** a session or token created on one is not accepted by the other
