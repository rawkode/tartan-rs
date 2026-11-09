# repo-config Specification

## Purpose
A repository's root CUE package `tartan` declares the extensions it installs and their settings.

## Requirements

### Requirement: The configuration is the root package tartan
A repository's configuration SHALL be every root `*.cue` file declaring `package tartan`, evaluated together. Root files of other packages SHALL be ignored, and a root with no `package tartan` file SHALL be the empty configuration.

#### Scenario: Several files
- **WHEN** `issues.cue` and `kanban.cue` in the root both declare `package tartan`, one for each extension
- **THEN** the configuration is both files together

#### Scenario: Another package in the root
- **WHEN** the root also holds `env.cue` declaring `package cuenv`
- **THEN** it is not part of the configuration

### Requirement: Modules and imports
The repository SHALL be a CUE module, and its `cue.mod/` SHALL be used as committed: imports of packages inside the module and files embedded with `@embed` SHALL be loaded from the same commit, and dependencies on other modules SHALL resolve through the forge's CUE registry configuration. A root package `tartan` without `cue.mod/module.cue` SHALL be refused: the forge adds no module of its own.

#### Scenario: Importing a package of the repository
- **WHEN** the root package imports `example.com/repo/config/fields` and `config/fields/` defines the field definitions
- **THEN** the configuration uses them

#### Scenario: Embedding a file
- **WHEN** the root package embeds `config/properties.json` with `@embed(file="config/properties.json")`
- **THEN** the configuration uses the file's contents, as `cue export` does

#### Scenario: No module
- **WHEN** the root holds `tartan.cue` declaring `package tartan` but no `cue.mod/module.cue`
- **THEN** the configuration is refused with a problem naming `cue.mod/module.cue`

### Requirement: Loading is bounded
Loading a configuration SHALL read at most 500 files, 64 directories and 4 MiB, embedded files included; a configuration needing more SHALL be invalid.

#### Scenario: Too large
- **WHEN** the root package and its imports exceed 4 MiB
- **THEN** the configuration is refused with a problem naming the limit

### Requirement: The exported configuration is checked
After evaluation, the forge SHALL check the exported value: `extensions` maps an extension id to an entry with a semver `version` and object `settings`. Any other key in an entry, an invalid id, a missing or invalid version, a conflict or a syntax error SHALL make the configuration invalid.

#### Scenario: Install
- **WHEN** the root package `tartan` sets `extensions: "issues.tartan.rawkode.academy": issues.#Extension & {settings: …}`
- **THEN** the configuration installs Issues at the module's version with those settings

#### Scenario: Invalid entry
- **WHEN** an entry has `version: "latest"`, no version, or an unknown key
- **THEN** the configuration is invalid

### Requirement: Values are concrete
As with `cue export`, a value that is not concrete — a type, a constraint, a disjunction without a default, or a required field never given — SHALL make the configuration invalid, with a problem naming its path. Because the forge's evaluator exports such a value as `null`, an explicit `null` anywhere in the package SHALL make it invalid too.

#### Scenario: A setting left as a type
- **WHEN** an entry's settings set `colour: string`
- **THEN** the configuration is refused, naming `settings.colour`, and nothing changes

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
