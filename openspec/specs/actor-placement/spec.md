# actor-placement Specification

## Purpose
A placement directory decides which Rivet pool hosts an org's or repository's actors, so a large one can run on its own machines while staying part of the forge.

## Requirements

### Requirement: Placements name pools for new actors
`PUT /-/api/placements {"target": "org/<org>" | "repo/<org>/<repo>", "pool": …}` SHALL record the pool new actors for that target are created in; a repository without its own entry SHALL use its org's. Actors that already exist SHALL stay where they are. Any other target SHALL be refused with 400.

#### Scenario: Org on dedicated machines
- **WHEN** `org/big` is placed in pool `dedicated` and then the org `big` and its repository `monorepo` are created
- **THEN** both actors run in `dedicated`, and other orgs stay in the default pool

#### Scenario: Invalid target
- **WHEN** a placement targets `nonsense`
- **THEN** the API answers 400
