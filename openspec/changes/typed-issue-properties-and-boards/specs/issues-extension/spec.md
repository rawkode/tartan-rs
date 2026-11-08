# Spec Delta

## MODIFIED Requirements

### Requirement: Issues with configured fields
Each repository installation SHALL keep issues numbered from 1, open or closed, optionally filed against a project (a root path). Its properties SHALL be the org-wide Issues properties plus the repository's own (`settings.properties`), each typed with `tartan.dev/types`; a value that does not fit its property's type SHALL be refused with `invalid`.

#### Scenario: Create and list
- **WHEN** two issues are created and the issues are listed
- **THEN** they are numbered 1 and 2 and both are open

#### Scenario: Value of the wrong type
- **WHEN** an issue sets a `#URL` property to `not a url`
- **THEN** it is refused with `invalid`

## ADDED Requirements

### Requirement: Every property type is stored and shown
Issues SHALL store and show values of every type in `tartan.dev/types`: short and long text (long text rendered as Markdown), numbers, dates, booleans, selects, multi-selects, persons, URLs and money (whole minor units in the property's currency).

#### Scenario: Long text
- **WHEN** an issue's `notes` (`#LongText`) is set to Markdown
- **THEN** `get` returns it unchanged and the issue page renders it as Markdown
