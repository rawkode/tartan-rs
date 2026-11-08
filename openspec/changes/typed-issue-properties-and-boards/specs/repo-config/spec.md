# Spec Delta

## ADDED Requirements

### Requirement: Extension settings are typed
A configuration SHALL be evaluated with the forge's types package and the settings schemas of the extensions it names (see `config-types` and `extension-config-schemas`), so references between settings — such as a board's columns referring to an issue property — are checked when it is evaluated.

#### Scenario: Reference to a missing property
- **WHEN** a board's `columns` refers to `issues.stauts`, which is not a property
- **THEN** the configuration is invalid
