# extension-runtime Specification

## Purpose
Extensions are WebAssembly components of the `tartan:ext@0.1.0` world (`wit/tartan.wit`), run by the forge with their own database and limits.

## Requirements

### Requirement: Components of the tartan:ext world run unchanged
The host SHALL run any component of the `tartan:ext@0.1.0` world. Interfaces added to the world SHALL be additive, so components built before them keep running. A component that does not fit the world SHALL be refused.

#### Scenario: Issues runs as built
- **WHEN** the Issues component is called to create, list and render issues
- **THEN** each call succeeds

#### Scenario: Not a component
- **WHEN** bytes that are not a component are compiled
- **THEN** they are refused

### Requirement: Each call is isolated and limited
Each call SHALL run in a fresh instance with its own fuel, memory (128 MiB) and wall-clock (10 s) limits. A call that exceeds a limit SHALL fail without effects.

#### Scenario: Out of fuel
- **WHEN** a call is given almost no fuel
- **THEN** it traps and changes nothing

### Requirement: SQL reaches only the installation's own tables
An extension's SQL SHALL run against its installation's own database; statements touching the host's `_tartan_*` tables, `ATTACH`, `DETACH`, `PRAGMA` or `VACUUM` SHALL be refused with `denied`. During `render` and `provide-context`, only a single `SELECT` or `WITH` query SHALL be allowed and effects SHALL be refused. This guard prevents mistakes; it is not a sandbox.

#### Scenario: Host table
- **WHEN** an extension queries `_tartan_kv`
- **THEN** the call gets `denied`

### Requirement: Effects apply only after success
Events an extension emits and timers it sets SHALL be applied only when the call returns `Ok`. Emitted events SHALL appear as `x.<extension-id>.<type>` in the installation's log, with the installation, org, repository and path. `notify` and why-note contributions SHALL answer `unavailable` until they are implemented.

#### Scenario: Emitted event
- **WHEN** a successful call emits `seen`
- **THEN** the installation's log gains `x.<extension-id>.seen`

### Requirement: Migrations run once each
A package's migrations SHALL run in order, each once per installation, recorded by name.

#### Scenario: Applying twice
- **WHEN** the same migrations are applied again
- **THEN** nothing runs again

### Requirement: Calls carry the caller's principal
A `tool`, `render` or `act` call SHALL give the extension the request's principal as `viewer`: `{kind: user, id: <DID>}` for a person, or `{kind: agent, id: "token:<id>", on-behalf-of: <DID>}` for a token. The `actor` world types SHALL be unchanged, so existing components keep running.

#### Scenario: Person renders a view
- **WHEN** a signed-in person opens an Issues page
- **THEN** the render's viewer is `{kind: user, id: <their DID>}`

#### Scenario: Agent calls a tool
- **WHEN** an owner's token calls an Issues tool
- **THEN** the call's viewer is an agent on behalf of the owner's DID
