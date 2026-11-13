## MODIFIED Requirements

### Requirement: A forge id separates forges sharing a Rivet namespace
The forge's singleton actors (the forge actor, its subscribers and the `identity` actor) SHALL be keyed by the forge id (`TARTAN_FORGE_ID`, default `forge`), so several forges can share one Rivet namespace.

#### Scenario: Two forges, one engine
- **WHEN** two forges with different ids run against the same Rivet engine
- **THEN** neither sees the other's groups, repositories or subscribers

#### Scenario: Separate identity state
- **WHEN** two forges with different ids run against the same Rivet engine
- **THEN** a session or token created on one is not accepted by the other

## REMOVED Requirements

### Requirement: Slugs name orgs and repositories
**Reason**: Orgs are removed; slugs now name groups and repositories in `forge-tree`.
**Migration**: See `forge-tree`'s "Slugs name groups and repositories".

### Requirement: Creating an org or repository is idempotent
**Reason**: Orgs are removed; creation is path-addressed under `/-/api/tree` in `forge-tree`.
**Migration**: Use `POST /-/api/tree[/<group path>]` with `{"kind", "slug"}`; an org becomes a top-level group.
