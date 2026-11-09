# Spec Delta

## MODIFIED Requirements

### Requirement: Swim lanes and groups
A board MAY set `lanes` (rows) and `group` (headed groups within each cell), each one of `#issues`' `#Select`, `#MultiSelect` or `#Person` properties, or `"project"`. A card SHALL appear in the lane and group of its value — under each value for a multi-select — and under a final "none" lane or group without one. Person lanes and groups SHALL be keyed by DID and labelled by handle.

#### Scenario: Lanes by priority
- **WHEN** a board sets `lanes: props.priority` and the repository has one `p1` issue and one with no priority
- **THEN** the board has a `p1` lane with one card and a "none" lane with the other

#### Scenario: Groups by project
- **WHEN** a board sets `group: "project"`
- **THEN** each cell groups its cards under their projects, and cards without a project under "none"

#### Scenario: Lanes by assignee
- **WHEN** a board sets `lanes: props.assignee` and one issue is assigned to the DID of `alice.example`
- **THEN** the board has a lane labelled `@alice.example` holding that card
