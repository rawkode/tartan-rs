# Spec Delta

## Purpose
The `tartan.dev/types` CUE module: the typed property definitions that extension modules and repositories import.

## ADDED Requirements

### Requirement: The types module is a real CUE module
`tartan.dev/types` SHALL be a CUE module published to an OCI registry, so a repository depending on it evaluates natively with `cue eval`.

#### Scenario: Importing the types
- **WHEN** a repository's `cue.mod/module.cue` depends on `tartan.dev/types@v0` and its package `tartan` uses `types.#Select`
- **THEN** `cue export` with the registry configured evaluates, and so does the forge

### Requirement: Property types
The module SHALL define `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person` (a principal id), `#URL`, `#Money` and `#Property` for any of them. Each SHALL carry a `kind` naming its type and an optional `label`.

#### Scenario: Any property
- **WHEN** a property is `types.#Property & {kind: "money", currency: "GBP"}`
- **THEN** it evaluates as a `#Money` property

### Requirement: Property constraints
`#ShortText` and `#LongText` SHALL take `maxLength` (at most 200 and 20000), `#Number` `min` and `max`, `#Select` and `#MultiSelect` non-empty distinct `options` and `labels` keyed by option, and `#Money` a three-letter `currency`. A `default` SHALL fit its property.

#### Scenario: Select with labels
- **WHEN** a property is `types.#Select & {options: ["todo", "done"], labels: {todo: "To do"}}`
- **THEN** it evaluates, and a label for a value that is not an option is invalid

#### Scenario: Default must fit
- **WHEN** a select's `default` is not one of its options
- **THEN** the configuration is invalid
