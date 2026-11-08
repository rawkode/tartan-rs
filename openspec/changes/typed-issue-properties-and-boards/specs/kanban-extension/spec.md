# Spec Delta

## MODIFIED Requirements

### Requirement: Cards follow Issues' snapshots
A repository installation SHALL keep one card per issue of its repository from Issues' snapshots, keeping the highest version, including issues created before it was installed.

#### Scenario: Issue created before install
- **WHEN** an issue exists before Kanban is installed
- **THEN** it appears on the repository's boards

### Requirement: Boards come from settings
Boards SHALL be defined in Kanban's settings (`boards.<id>`): a board shows the repository's issues, or one project's when it sets `project`. A board that does not fit Kanban's module SHALL make the configuration invalid.

#### Scenario: Repository board
- **WHEN** a repository's configuration sets `boards: api: {columns: props.status}`
- **THEN** `list_boards` includes `api`, showing every issue of the repository

#### Scenario: Project board
- **WHEN** it sets `boards: lsp: {project: "crates/lsp", columns: props.status}`
- **THEN** the board `lsp` shows only the issues in project `crates/lsp`

#### Scenario: Invalid board
- **WHEN** a board has no columns
- **THEN** the repository's configuration is refused

## REMOVED Requirements

### Requirement: Boards at three levels
**Reason**: Kanban installs on a repository; org boards across repositories cannot be evaluated natively.
**Migration**: Define boards in each repository's configuration, for the repository or one of its projects.

## ADDED Requirements

### Requirement: Kanban requires Issues
Kanban's module SHALL import Issues' module and take the repository's Issues entry as `#issues`; Kanban's manifest SHALL require `rawkode.issues`, and installing Kanban without Issues SHALL refuse the configuration.

#### Scenario: No Issues
- **WHEN** a repository installs Kanban without Issues
- **THEN** the configuration is refused

### Requirement: Columns reference a select property
A board's `columns` SHALL be one of `#issues`' `#Select` properties — all its options in order, or `{property, values}` for a subset in a chosen order — or `"state"` for open and closed. Anything else, or values that are not the property's options, SHALL fail evaluation. Column titles SHALL be the property's labels, falling back to the values. A board SHALL show a card without a value in the first column and a card whose value is no column's as unplaced.

#### Scenario: Columns from a property
- **WHEN** a board sets `columns: props.status`, with options `todo`, `doing`, `done` and the label `To do`
- **THEN** the board has the columns To do, doing and done

#### Scenario: A lookalike property
- **WHEN** a board's `columns` is a `#Select` written inline rather than Issues' property
- **THEN** the configuration fails evaluation

### Requirement: Swim lanes and groups
A board MAY set `lanes` (rows) and `group` (headed groups within each cell), each one of `#issues`' `#Select`, `#MultiSelect` or `#Person` properties, or `"project"`. A card SHALL appear in the lane and group of its value — under each value for a multi-select — and under a final "none" lane or group without one.

#### Scenario: Lanes by priority
- **WHEN** a board sets `lanes: props.priority` and the repository has one `p1` issue and one with no priority
- **THEN** the board has a `p1` lane with one card and a "none" lane with the other

#### Scenario: Groups by project
- **WHEN** a board sets `group: "project"`
- **THEN** each cell groups its cards under their projects, and cards without a project under "none"
