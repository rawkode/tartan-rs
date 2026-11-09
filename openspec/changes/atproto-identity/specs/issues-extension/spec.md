# Spec Delta

## MODIFIED Requirements

### Requirement: Every property type is stored and shown
Issues SHALL store and show values of every type in `tartan.dev/types`: short and long text (long text rendered as Markdown), numbers, dates, booleans, selects, multi-selects, persons (stored as DIDs and shown by handle), URLs and money (whole minor units in the property's currency).

#### Scenario: Long text
- **WHEN** an issue's `notes` (`#LongText`) is set to Markdown
- **THEN** `get` returns it unchanged and the issue page renders it as Markdown

#### Scenario: Person
- **WHEN** an issue's `assignee` (`#Person`) is set to a DID whose handle is `alice.example`
- **THEN** `get` returns the DID and the issue page shows `@alice.example`

#### Scenario: Person that is not a DID
- **WHEN** an issue's `assignee` is set to `admin`
- **THEN** it is refused with `invalid`

## ADDED Requirements

### Requirement: Authors are the caller's principal
Issues SHALL record each issue's author from the call's viewer: the person's DID, or the agent with the DID it acts for, and SHALL show the author by handle, an agent as `@handle (agent)`.

#### Scenario: Created by a person
- **WHEN** a signed-in owner creates an issue
- **THEN** the issue's author is their DID and the page shows their handle

#### Scenario: Created by an agent
- **WHEN** an owner's token creates an issue
- **THEN** the author is the agent, on behalf of the owner's DID
