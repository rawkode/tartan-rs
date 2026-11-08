# kanban-extension Specification

## Purpose
`rawkode.kanban`: boards over an org's issues for the org, a repository or a project, each with its own columns; moving a card updates the issue.

## Requirements

### Requirement: Cards follow Issues' snapshots
An org installation SHALL keep one card per issue in the org from Issues' snapshots, keeping the highest version, including issues created before it was installed.

#### Scenario: Issue created before install
- **WHEN** an issue exists before Kanban is installed
- **THEN** it appears on the org's boards

### Requirement: Boards at three levels
A board SHALL have an id, a name, a field (an issue field or `state`), 1 to 12 columns of distinct values, and a scope: `org`, `repo` (a repository) or `project` (a repository and a project path). A board SHALL show the cards in its scope, a card without a value in the first column and a card whose value is no column's as unplaced. `list_boards` SHALL list the boards and `board` SHALL show one.

#### Scenario: Three levels
- **WHEN** an org has three issues, two in `api` and one of those in project `crates/lsp`
- **THEN** the org board shows three cards, the `api` board two and the project board one

### Requirement: Moving a card updates the issue
The `move` tool and the board's `move` action SHALL call Issues' `update` in the card's repository, setting the board's field (or opening and closing for a `state` board). A target that is not a column SHALL be refused with `invalid`; a card outside the board's scope with `not_found`.

#### Scenario: Move
- **WHEN** `acme/web#1` is moved to `done` on the org board
- **THEN** the issue's `status` in `acme/web` is `done` and the card is in `done`

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
