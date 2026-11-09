# issues-extension Specification

## Purpose
`issues.tartan.rawkode.academy`: numbered issues per repository with typed properties from its configuration, emitting a snapshot of each issue on every write.

## Requirements

### Requirement: Issues with configured fields
Each repository installation SHALL keep issues numbered from 1, open or closed, optionally filed against a project (a root path). Its properties SHALL be its settings' `properties`, typed with `tartan.rawkode.academy/types` through Issues' module `tartan.rawkode.academy/issues`; a value that does not fit its property's type SHALL be refused with `invalid`.

#### Scenario: Create and list
- **WHEN** two issues are created and the issues are listed
- **THEN** they are numbered 1 and 2 and both are open

#### Scenario: Value of the wrong type
- **WHEN** an issue sets a `#URL` property to `not a url`
- **THEN** it is refused with `invalid`

### Requirement: Every write emits a snapshot
Every write SHALL emit `x.issues.tartan.rawkode.academy.issue.created` or `.updated` carrying the issue's number, title, project, state, stored `properties`, version and update time, without its body, so other extensions can keep a copy by keeping the highest version.

#### Scenario: Created
- **WHEN** two issues are created
- **THEN** two `x.issues.tartan.rawkode.academy.issue.created` events are recorded

### Requirement: Every property type is stored and shown
Issues SHALL store and show values of every type in `tartan.rawkode.academy/types`: short and long text (long text rendered as Markdown), numbers, dates, booleans, selects, multi-selects, persons (stored as DIDs and shown by handle), URLs and money (whole minor units in the property's currency).

#### Scenario: Long text
- **WHEN** an issue's `notes` (`#LongText`) is set to Markdown
- **THEN** `get` returns it unchanged and the issue page renders it as Markdown

#### Scenario: Person
- **WHEN** an issue's `assignee` (`#Person`) is set to a DID whose handle is `alice.example`
- **THEN** `get` returns the DID and the issue page shows `@alice.example`

#### Scenario: Person that is not a DID
- **WHEN** an issue's `assignee` is set to `admin`
- **THEN** it is refused with `invalid`

### Requirement: An unset property is absent
`null`, an empty string and an empty list SHALL mean no value: `update` SHALL clear the property, and `create` SHALL leave it unset, so an unset property is never stored.

#### Scenario: Created with an empty value
- **WHEN** an issue is created with `properties: {priority: null, notes: ""}`
- **THEN** neither property is stored, and a read shows only their defaults

### Requirement: Authors are the caller's principal
Issues SHALL record each issue's author from the call's viewer: the person's DID, or the agent with the DID it acts for, and SHALL show the author by handle, an agent as `@handle (agent)`.

#### Scenario: Created by a person
- **WHEN** a signed-in owner creates an issue
- **THEN** the issue's author is their DID and the page shows their handle

#### Scenario: Created by an agent
- **WHEN** an owner's token creates an issue
- **THEN** the author is the agent, on behalf of the owner's DID
