## RENAMED Requirements

- FROM: `### Requirement: Calls stay inside the org`
- TO: `### Requirement: Calls stay inside the repository`

## MODIFIED Requirements

### Requirement: Calls stay inside the repository
The target SHALL be an installation on the caller's repository. The call SHALL act as the principal of the calling call, and the target's errors SHALL be returned to the caller.

#### Scenario: Permitted call
- **WHEN** Kanban on `acme/web` calls Issues' `update` on `acme/web`
- **THEN** the issue in `acme/web` is updated

#### Scenario: Another repository
- **WHEN** an installation on `acme/web` calls into `acme/api`, or into `acme/platform/api`
- **THEN** the call is refused with `denied`
