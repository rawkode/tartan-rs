# Spec Delta

## MODIFIED Requirements

### Requirement: A push to main applies the repository's configuration
When a repository's `refs/heads/main` moves to a commit, the forge SHALL load the repository's configuration package (see `repo-config`) at that commit and apply it to the repository: each entry with a `version` installs that version of a `repo`-scoped package with its settings as configuration. When several pushes arrive, the latest commit's configuration SHALL win.

#### Scenario: Installing by push
- **WHEN** a push to `acme/api`'s `main` adds `extensions: "rawkode.issues": {version: "0.5.0", settings: fields: …}` to the root package `tartan`
- **THEN** Issues 0.5.0 is installed on `acme/api` with those fields, and the repository's log gains `extension.installed` and `config.applied`

#### Scenario: Pushing another branch
- **WHEN** a push changes the configuration only on `refs/heads/topic`
- **THEN** nothing is applied
