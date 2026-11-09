# Spec Delta

## ADDED Requirements

### Requirement: The configuration may require signatures
The exported value MAY carry a top-level `signing` with the shape of `tartan.rawkode.academy/types`' `#Signing`. The forge SHALL check it with the rest of the configuration: a rule without `refs`, a ref that is not a full ref name, a signer that is not a DID, or any other key in `signing` or in a rule SHALL make the configuration invalid.

#### Scenario: Valid rule
- **WHEN** the root package sets `signing: require: [{refs: ["refs/heads/main", "refs/tags/**"], signers: ["did:plc:abcdefghijklmnopqrstuvwx"]}]`
- **THEN** the configuration is valid and, once applied, is the rule in force

#### Scenario: Typo
- **WHEN** the root package sets `signing: requires: [...]`
- **THEN** the configuration is invalid, naming `signing.requires`

### Requirement: The rule in force is the applied one
The signing rule in force for a repository SHALL be the `signing` of its last applied configuration, or none. A configuration that is refused SHALL NOT change the rule in force.

#### Scenario: Refused configuration
- **WHEN** a push to `main` adds a signing rule but its configuration is refused for another reason
- **THEN** no signing rule is in force
