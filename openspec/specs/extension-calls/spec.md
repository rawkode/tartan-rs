# extension-calls Specification

## Purpose
An installation calls another installation's tools through the `calls` import, when its manifest permits it, so extensions compose without sharing data.

## Requirements

### Requirement: Calls are permitted by the manifest
A call SHALL succeed only if the caller's manifest `permissions.calls` names the target extension and tool; otherwise it SHALL be refused with `denied`. Calls SHALL be refused during `render` and `provide-context`.

#### Scenario: Missing permission
- **WHEN** a Kanban without `permissions.calls` moves a card
- **THEN** the call is refused with `denied` (the API answers 403) and the issue is unchanged

### Requirement: Calls stay inside the org
The target SHALL be a repository installation in the caller's org; a repository's installation SHALL only call into its own repository. The call SHALL act as the principal of the calling call, and the target's errors SHALL be returned to the caller.

#### Scenario: Permitted call
- **WHEN** Kanban, installed on `acme`, calls Issues' `update` on `acme/web`
- **THEN** the issue in `acme/web` is updated
