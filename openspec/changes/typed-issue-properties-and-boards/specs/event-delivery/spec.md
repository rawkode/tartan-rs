# Spec Delta

## MODIFIED Requirements

### Requirement: A new recipient receives history
When an extension installation subscribes, the sources it covers SHALL be asked to deliver at once, so it receives every matching event already in their logs without waiting for a new one.

#### Scenario: Subscribing after the fact
- **WHEN** Kanban is installed on a repository whose Issues already emitted issue snapshots
- **THEN** it receives those snapshots promptly
