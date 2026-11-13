## ADDED Requirements

### Requirement: Placements name pools for groups and repositories
`PUT /-/api/placements {"target": "group/<path>" | "repo/<path>", "pool": …}` SHALL record the pool new actors for that target are created in; a group or repository without its own entry SHALL use that of its nearest ancestor group that has one, else the default pool. Actors that already exist SHALL stay where they are. Any other target SHALL be refused with 400.

#### Scenario: Group on dedicated machines
- **WHEN** `group/big` is placed in pool `dedicated` and then the group `big`, its group `team` and the repository `big/team/monorepo` are created
- **THEN** all three actors run in `dedicated`, and other groups stay in the default pool

#### Scenario: Nearest ancestor wins
- **WHEN** `group/big` is placed in `dedicated`, `group/big/team` in `other`, and `big/team/monorepo` is then created
- **THEN** the repository's actor runs in `other`

#### Scenario: Invalid target
- **WHEN** a placement targets `nonsense`
- **THEN** it is refused with 400

## REMOVED Requirements

### Requirement: Placements name pools for new actors
**Reason**: Orgs are removed, so the requirement is restated for the tree.
**Migration**: See "Placements name pools for groups and repositories".
