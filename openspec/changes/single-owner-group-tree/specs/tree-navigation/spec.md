## ADDED Requirements

### Requirement: A sidebar browses the tree to any depth
The web UI SHALL show a sidebar tree of the forge's top level. A group row SHALL expand and collapse in place to show its groups and repositories, to any depth, loading a group's children when it is first expanded. Group and repository rows SHALL be told apart by icon, the row for the open node SHALL be highlighted, and the ancestors of the open node SHALL be expanded when a page is opened by its URL. Expansion SHALL be kept per browser.

#### Scenario: Opening a deep repository
- **WHEN** a person opens `#/oss/devtools/editors/treesitter-kiln`
- **THEN** `oss`, `devtools` and `editors` are expanded, and the repository's row is highlighted

#### Scenario: Expanding a group
- **WHEN** a person expands `libraries`
- **THEN** its children appear beneath it, indented one level, without leaving the page

#### Scenario: Empty forge
- **WHEN** the forge has no groups or repositories
- **THEN** the sidebar says so, and an owner sees the controls to create the first group or repository

### Requirement: The sidebar shows open issues rolled up
Each row SHALL show the number of open issues in its node: a repository's own, a group's the sum of every repository below it, and the top row the total. The tree API SHALL return each child's count with its listing. Counts MAY lag changes by a few seconds.

#### Scenario: Rolled-up count
- **WHEN** `editors` holds `treesitter-kiln` with 0 open issues and `git-tools` with 2
- **THEN** `editors` shows 2, and every ancestor of it counts those 2

#### Scenario: Issue opened
- **WHEN** an issue is opened in a repository
- **THEN** the counts of the repository and every ancestor group rise by one

### Requirement: The header shows the path and jumps to nodes
The header SHALL show the open node's path as breadcrumbs, each ancestor linking to its group, and SHALL have a jump field that opens a node by typing its path or an abbreviation of it (`o/d/e`, matching segment prefixes in order), listing matches as the person types.

#### Scenario: Breadcrumb
- **WHEN** a person is on `oss/devtools/editors/treesitter-kiln`
- **THEN** the header shows `oss / devtools / editors / treesitter-kiln` and `devtools` links to that group

#### Scenario: Jump by abbreviation
- **WHEN** a person types `o/d/e` and chooses the match
- **THEN** the page opens `oss/devtools/editors`

### Requirement: The sidebar follows what the principal may see
The sidebar SHALL show the same tree to every principal that may read, SHALL show owner-only controls to owners only, and SHALL NOT be rendered for a principal with no rights (the private-forge page replaces it).

#### Scenario: Participant
- **WHEN** a participant opens the forge
- **THEN** the sidebar and counts show, and no create control does
