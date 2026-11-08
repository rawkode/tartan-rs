# Spec Delta

## ADDED Requirements

### Requirement: A package may ship a settings schema and requirements
A manifest MAY name a settings schema (`config.schema`, a CUE file of package `settings` defining `#Settings`) and the extensions its settings require (`config.requires`, alias to extension id). Publishing SHALL store the schema with the package and SHALL refuse a manifest whose schema file is missing.

#### Scenario: Schema stored
- **WHEN** Issues is published with `config.schema: "config/settings.cue"`
- **THEN** configurations naming that version are checked against it
