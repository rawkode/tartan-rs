# config-types Specification

## Purpose
The `tartan.rawkode.academy/types` CUE module: the typed property definitions that extension modules and repositories import.

## Requirements

### Requirement: The types module is a real CUE module
`tartan.rawkode.academy/types` SHALL be a CUE module published to an OCI registry, so a repository depending on it evaluates natively with `cue eval`.

#### Scenario: Importing the types
- **WHEN** a repository's `cue.mod/module.cue` depends on `tartan.rawkode.academy/types@v0` and its package `tartan` uses `types.#Select`
- **THEN** `cue export` with the registry configured evaluates, and so does the forge

### Requirement: Property types
The module SHALL define `#ShortText`, `#LongText` (Markdown), `#Number`, `#Date`, `#Boolean`, `#Select`, `#MultiSelect`, `#Person` (a person's DID), `#URL`, `#Money` and `#Property` for any of them. Each SHALL carry a `kind` naming its type and an optional `label`.

#### Scenario: Any property
- **WHEN** a property is `types.#Property & {kind: "money", currency: "GBP"}`
- **THEN** it evaluates as a `#Money` property

#### Scenario: Person default is a DID
- **WHEN** a property is `types.#Person & {default: "did:plc:abcdefghijklmnopqrstuvwx"}`
- **THEN** it evaluates, and `default: "usr_dev"` or a handle is invalid

### Requirement: Property constraints
`#ShortText` and `#LongText` SHALL take `maxLength` (at most 200 and 20000), `#Number` `min` and `max`, `#Select` and `#MultiSelect` non-empty distinct `options` and `labels` keyed by option, and `#Money` a three-letter `currency`. A `default` SHALL fit its property.

#### Scenario: Select with labels
- **WHEN** a property is `types.#Select & {options: ["todo", "done"], labels: {todo: "To do"}}`
- **THEN** it evaluates, and a label for a value that is not an option is invalid

#### Scenario: Default must fit
- **WHEN** a select's `default` is not one of its options
- **THEN** the configuration is invalid

### Requirement: The DID type
The module SHALL define `#DID`: a `did:plc:` DID of 24 base32 characters (`a-z`, `2-7`), or a `did:web:` DID naming a hostname and no path; only `localhost` may carry a `%3A`-encoded port. `#Person`'s `default` SHALL be a `#DID`.

#### Scenario: did:web with a port
- **WHEN** a value is `types.#DID & "did:web:localhost%3A9400"`
- **THEN** it evaluates

#### Scenario: Not a DID
- **WHEN** a value is `types.#DID & "alice.bsky.social"`
- **THEN** the configuration is invalid

### Requirement: The signing rule type
The module SHALL define `#Signing`: `require`, a list of rules each with non-empty `refs` (full ref names; `*` matches within one path segment and a final `/**` any number of segments) and optional non-empty `signers` (`#DID`s).

#### Scenario: Protected main
- **WHEN** a repository sets `signing: types.#Signing & {require: [{refs: ["refs/heads/main"]}]}`
- **THEN** it evaluates

#### Scenario: Bad pattern
- **WHEN** a rule's `refs` contains `main`
- **THEN** the configuration is invalid because it is not a full ref name

### Requirement: The DID change is a new version
The DID types SHALL be published as `tartan.rawkode.academy/types@v0.2.0`; `v0.1.0` SHALL stay published and unchanged, so repositories pinned to it keep evaluating.

#### Scenario: Pinned to the old version
- **WHEN** a repository depends on `tartan.rawkode.academy/types@v0.1.0` and the forge lists both versions
- **THEN** its configuration evaluates as before
