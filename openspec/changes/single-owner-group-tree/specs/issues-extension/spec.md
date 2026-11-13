## ADDED Requirements

### Requirement: People comment on issues
Issues SHALL have a `comment` tool, declared `access: "participate"`, that adds a comment (non-empty text, at most 65536 bytes) to an issue and records it with its author, time and the issue's number. `get` SHALL return an issue's comments in order. Comments SHALL be immutable and SHALL emit a snapshot event like every other write.

#### Scenario: Comment
- **WHEN** a person comments on issue 3
- **THEN** `get` for issue 3 lists the comment with its author and time, after earlier comments

#### Scenario: Empty comment
- **WHEN** a person comments with empty text
- **THEN** the tool fails with a validation error and nothing is stored

#### Scenario: Unknown issue
- **WHEN** a person comments on an issue that does not exist
- **THEN** the tool fails with a not-found error

### Requirement: Participants open issues with a title and a body only
`create` SHALL be declared `access: "participate"`. When the caller is not an owner, `create` SHALL accept only a title and a body and SHALL fail with `denied` if it is given property values. `list`, `get` and `properties` remain `read`; `close`, `reopen` and `update` remain `write`.

#### Scenario: Visitor sets a property
- **WHEN** a participant calls `create` with a title and the property `priority`
- **THEN** the call fails with `denied` and no issue is created

#### Scenario: Owner sets a property
- **WHEN** an owner calls `create` with a title and the property `priority`
- **THEN** the issue is created with that value
