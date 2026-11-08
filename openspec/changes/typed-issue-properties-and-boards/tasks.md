# Tasks

## 1. Types and schemas in evaluation

- [ ] 1.1 Write the `tartan.dev/types` package (all property types and constraints) and verify with CUE tests: each type evaluates, a label for a missing option and a default that does not fit are invalid
- [ ] 1.2 Evaluate in two passes: pass one finds the named extensions and versions; pass two adds the types package, each extension's schema under `cue.mod/pkg`, a generated binding (settings unified with `#Settings`, required extensions as `#<alias>`) and org-wide settings; verify with unit tests over in-memory schemas, including a missing requirement and a conflicting org-wide setting
- [ ] 1.3 Document the types package (a new docs page) and schemas, requirements and org-wide settings on the Configuration page, and verify the docs build and lint

## 2. Packages and reconciliation

- [ ] 2.1 Accept `config.schema` and `config.requires` in manifests, store the schema with the package (refusing a missing file), and verify publishing tests cover both
- [ ] 2.2 Resolve schemas for evaluation (entry version, org installation version for contributions, latest published for org-wide settings) and verify a configuration with invalid settings is refused end to end
- [ ] 2.3 Let the org's `tartan` repository give version-less settings for repository extensions, store them on the org actor, include them in each repository's evaluation, and reapply every repository when they change; verify end to end that an org property reaches every repository's Issues

## 3. Issues 0.6.0

- [ ] 3.1 Rename fields to properties and add long text, person, URL and money (from monetary) with their checks and display, with a settings schema importing `tartan.dev/types`; verify with Issues' unit tests and the shared value vectors
- [ ] 3.2 Update the Issues docs page (properties, every type, org-wide and repository properties) and verify the docs build

## 4. Kanban 0.3.0

- [ ] 4.1 Add Kanban's schema requiring Issues: `columns` (a select reference, a subset, or `"state"`), `lanes` and `group` (select, multi-select, person, `"repository"`, `"project"`), checked against `#issues.properties`; verify the configuration is refused for a missing Issues, a non-select column and an unknown option
- [ ] 4.2 Build board views with lanes × columns × groups (multi-select under each value, "none" last, titles from labels) and verify with Kanban's unit tests
- [ ] 4.3 Move the Kanban end-to-end test to org-wide typed properties with lanes and groups, a repository's added property, and verify moves still update issues
- [ ] 4.4 Update the Kanban docs page (columns, lanes, groups, the dependency, a worked example) and verify the docs build

## 5. Verification

- [ ] 5.1 Run `scripts/e2e.sh` and verify every suite passes
