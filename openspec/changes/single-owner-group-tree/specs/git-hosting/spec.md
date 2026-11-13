## MODIFIED Requirements

### Requirement: Git over smart HTTP
Clients SHALL clone, fetch and push over smart HTTP at the repository's clone URL, `<TARTAN_PUBLIC_URL>/<path>.git`, which the gateway forwards to the Git host. A push SHALL send the Git host token as `Authorization: Bearer <token>` (for example with `git -c http.extraHeader=…`); anonymous reads are allowed.

#### Scenario: Round trip
- **WHEN** a client pushes `main`, creates and deletes a branch, and clones
- **THEN** the clone's `HEAD` is the last pushed commit
