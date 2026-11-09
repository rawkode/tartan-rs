# Spec Delta

## ADDED Requirements

### Requirement: Calls carry the caller's principal
A `tool`, `render` or `act` call SHALL give the extension the request's principal as `viewer`: `{kind: user, id: <DID>}` for a person, or `{kind: agent, id: "token:<id>", on-behalf-of: <DID>}` for a token. The `actor` world types SHALL be unchanged, so existing components keep running.

#### Scenario: Person renders a view
- **WHEN** a signed-in person opens an Issues page
- **THEN** the render's viewer is `{kind: user, id: <their DID>}`

#### Scenario: Agent calls a tool
- **WHEN** an owner's token calls an Issues tool
- **THEN** the call's viewer is an agent on behalf of the owner's DID
