# Spec Delta

## ADDED Requirements

### Requirement: Boards come from settings
Boards SHALL be defined in Kanban's configuration: `boards.<id>` in the org's settings may have any scope, and `boards.<id>` contributed by a repository (`repos.<repo-id>.settings.boards`) are scoped to that repository, or to one of its projects when they set `project`. A contributed board's id SHALL be prefixed with the repository's path (`<org>/<repo>/<id>`). An invalid board SHALL be left out and listed with its problem by `list_boards`.

#### Scenario: Repository board
- **WHEN** `acme/api`'s `tartan.cue` contributes `boards: api: {field: "status", columns: …}`
- **THEN** `list_boards` includes `acme/api/api`, scoped to `acme/api`

#### Scenario: Project board
- **WHEN** `acme/api` contributes `boards: lsp: {project: "crates/lsp", …}`
- **THEN** the board `acme/api/lsp` shows only `acme/api` cards in project `crates/lsp`

#### Scenario: Invalid board
- **WHEN** a contributed board has no columns
- **THEN** it is not a board, and `list_boards` reports it with the problem

## MODIFIED Requirements

### Requirement: Boards at three levels
A board SHALL have an id, a name, a field (an issue field or `state`), 1 to 12 columns of distinct values, and a scope: `org`, `repo` (a repository) or `project` (a repository and a project path). A board SHALL show the cards in its scope, a card without a value in the first column and a card whose value is no column's as unplaced. `list_boards` SHALL list the boards and `board` SHALL show one.

#### Scenario: Three levels
- **WHEN** an org has three issues, two in `api` and one of those in project `crates/lsp`
- **THEN** the org board shows three cards, the `api` board two and the project board one
