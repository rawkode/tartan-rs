# Spec Delta

## MODIFIED Requirements

### Requirement: Boards at three levels
A board SHALL have an id, a name, columns, and a scope: `org`, `repo` (a repository) or `project` (a repository and a project path). A board SHALL show the cards in its scope, a card without a column value in the first column and a card whose value is no column's as unplaced. `list_boards` SHALL list the boards and `board` SHALL show one.

#### Scenario: Three levels
- **WHEN** an org has three issues, two in `api` and one of those in project `crates/lsp`
- **THEN** the org board shows three cards, the `api` board two and the project board one

### Requirement: Boards come from settings
Boards SHALL be defined in Kanban's configuration: `boards.<id>` in the org's settings may have any scope, and `boards.<id>` contributed by a repository are scoped to that repository, or to one of its projects when they set `project`. A contributed board's id SHALL be prefixed with the repository's path (`<org>/<repo>/<id>`). A board that does not fit Kanban's schema SHALL make the configuration invalid.

#### Scenario: Repository board
- **WHEN** `acme/api`'s configuration contributes `boards: api: {columns: issues.status}`
- **THEN** `list_boards` includes `acme/api/api`, scoped to `acme/api`

#### Scenario: Project board
- **WHEN** `acme/api` contributes `boards: lsp: {project: "crates/lsp", columns: issues.status}`
- **THEN** the board `acme/api/lsp` shows only `acme/api` cards in project `crates/lsp`

#### Scenario: Invalid board
- **WHEN** a contributed board has no columns
- **THEN** the repository's configuration is refused

## ADDED Requirements

### Requirement: Kanban requires Issues
Kanban's settings SHALL require Issues' settings: configuring Kanban where Issues properties are not configured SHALL make the configuration invalid.

#### Scenario: No Issues
- **WHEN** the org installs Kanban without Issues properties
- **THEN** the configuration is refused

### Requirement: Columns reference a select property
A board's `columns` SHALL be a reference to an Issues `#Select` property — all its options in order, or `{property, values}` for a subset in a chosen order — or `"state"` for open and closed. A reference that is not an Issues select property, or values that are not its options, SHALL make the configuration invalid. Column titles SHALL be the property's option labels, falling back to the values.

#### Scenario: Columns from a property
- **WHEN** a board sets `columns: issues.status`, with options `todo`, `doing`, `done` and the label `To do`
- **THEN** the board has the columns To do, doing and done

#### Scenario: Not a select
- **WHEN** a board's `columns` refers to a `#ShortText` property
- **THEN** the configuration is invalid

### Requirement: Swim lanes and groups
A board MAY set `lanes` (rows) and `group` (headed groups within each cell), each a reference to an Issues `#Select`, `#MultiSelect` or `#Person` property, or `"repository"` or `"project"`. A card SHALL appear in the lane and group of its value — under each value for a multi-select — and under a final "none" lane or group without one.

#### Scenario: Lanes by priority
- **WHEN** a board sets `lanes: issues.priority` and an org has one `p1` issue and one with no priority
- **THEN** the board has a `p1` lane with one card and a "none" lane with the other

#### Scenario: Groups by repository
- **WHEN** a board sets `group: "repository"`
- **THEN** each cell groups its cards under their repositories' paths
