# Spec Delta

## Purpose
The forge's CUE types package, `tartan.dev/types`, which configurations and extension schemas import to declare typed properties.

## ADDED Requirements

### Requirement: The types package is always available
Every configuration evaluation SHALL be able to `import "tartan.dev/types"` without declaring a dependency or reaching a registry.

#### Scenario: Importing the types
- **WHEN** a repository's package `tartan` imports `tartan.dev/types` and uses `types.#Select`
- **THEN** the configuration evaluates

### Requirement: Property types
The package SHALL define `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person` (a principal id), `#URL`, `#Money` and `#Property` for any of them. Each SHALL carry a `kind` naming its type and an optional `label`.

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
