# kanban-extension Specification

## Purpose
`rawkode.kanban`: boards over a repository's issues, or one project's, with columns, lanes and groups from Issues' properties; moving a card updates the issue.

## Requirements

### Requirement: Cards follow Issues' snapshots
A repository installation SHALL keep one card per issue of its repository from Issues' snapshots, keeping the highest version, including issues created before it was installed.

#### Scenario: Issue created before install
- **WHEN** an issue exists before Kanban is installed
- **THEN** it appears on the repository's boards

### Requirement: Moving a card updates the issue
The `move` tool and the board's `move` action SHALL call Issues' `update` in the repository, setting the board's column property (or opening and closing for a `state` board), and SHALL keep the card as the issue `update` returns, unless a newer snapshot is already kept; Issues' snapshot of that version SHALL replace it. A target that is not a column SHALL be refused with `invalid`; a card outside the board's project with `not_found`.

#### Scenario: Move
- **WHEN** `acme/api#1` is moved to `done` on a board whose columns are `status`
- **THEN** the issue's `status` is `done` and the card is in `done`

#### Scenario: Moved after a change Kanban has not seen
- **WHEN** an issue is retitled and its card is moved before Kanban receives the retitled snapshot
- **THEN** the card shows the new title

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

### Requirement: Kanban requires Issues
Kanban's module SHALL import Issues' module and take the repository's Issues entry as `#issues`; Kanban's manifest SHALL require `rawkode.issues`, and installing Kanban without Issues SHALL refuse the configuration.

#### Scenario: No Issues
- **WHEN** a repository installs Kanban without Issues
- **THEN** the configuration is refused

### Requirement: Columns reference a select property
A board's `columns` SHALL be one of `#issues`' `#Select` properties — all its options in order, or `{property, values}` for a subset in a chosen order — or `"state"` for open and closed. Anything else, or values that are not the property's options, SHALL fail evaluation. Column titles SHALL be the property's labels, falling back to the values.

#### Scenario: Columns from a property
- **WHEN** a board sets `columns: props.status`, with options `todo`, `doing`, `done` and the label `To do`
- **THEN** the board has the columns To do, doing and done

#### Scenario: A lookalike property
- **WHEN** a board's `columns` is a `#Select` written inline whose definition differs from every Issues select property
- **THEN** the configuration fails evaluation

#### Scenario: An identical inline select
- **WHEN** a board's `columns` is a `#Select` written inline exactly as Issues' `status`
- **THEN** CUE, which compares values rather than references, takes it as `status`

#### Scenario: A subset value that is not an option
- **WHEN** a board sets `columns: {property: props.status, values: ["blocked", "todo"]}`
- **THEN** the configuration fails evaluation

### Requirement: Cards are placed by their column value
An issue that never set the column property SHALL have the property's default as its value, as Issues shows it. A card without a value or default SHALL be in the first column, and a card whose value is no column's SHALL be unplaced.

#### Scenario: A default that is not a column
- **WHEN** `status` defaults to `todo`, a board's columns are `{property: props.status, values: ["doing", "done"]}`, and an issue never set its status
- **THEN** its card is unplaced

#### Scenario: No value and no default
- **WHEN** a board's columns are `props.priority`, which has no default, and an issue never set its priority
- **THEN** its card is in the first column

### Requirement: Boards use the installed Issues' properties
Kanban SHALL check each board's `columns`, `lanes` and `group` properties against the properties the repository's installed Issues lists: a property Issues does not have, or one whose definition differs from Issues' property of that name, SHALL make the board invalid, listed with its problem by `list_boards`, and the board SHALL not be shown or moved on.

#### Scenario: Bound to another Issues entry
- **WHEN** a repository binds `#issues` to an Issues entry of its own making with a property `phase` and a board uses it
- **THEN** `list_boards` lists the board as invalid because Issues has no property `phase`

### Requirement: Swim lanes and groups
A board MAY set `lanes` (rows) and `group` (headed groups within each cell), each one of `#issues`' `#Select`, `#MultiSelect` or `#Person` properties, or `"project"`. A card SHALL appear in the lane and group of its value — under each value for a multi-select — and under a final "none" lane or group without one.

#### Scenario: Lanes by priority
- **WHEN** a board sets `lanes: props.priority` and the repository has one `p1` issue and one with no priority
- **THEN** the board has a `p1` lane with one card and a "none" lane with the other

#### Scenario: Groups by project
- **WHEN** a board sets `group: "project"`
- **THEN** each cell groups its cards under their projects, and cards without a project under "none"
