# Tasks

## 1. Native evaluation and a registry

- [ ] 1.1 Evaluate the configuration exactly as committed (no added schema or module file, `CUE_REGISTRY` passed through) and check the exported value's shape in Rust; verify with unit tests: valid entries, missing or invalid versions, unknown keys, a configuration without `cue.mod`
- [ ] 1.2 Run an OCI registry (CNCF `distribution`) in `scripts/dev.sh` and `scripts/e2e.sh`, add `scripts/publish-cue-modules.sh` (`cue mod publish` to the registry in `CUE_REGISTRY`), and verify a module published by it imports in a forge evaluation test
- [ ] 1.3 Document native evaluation, registries and modules on the Configuration and Running locally pages, and verify the docs build and lint

## 2. Repository only

- [ ] 2.1 Remove org installations, the org's configuration repository, contributions, org recipient scope, org `FlushTree` and the org extension routes; require `storage.scope: "repo"`; verify the forge, extension and subscription tests pass without them
- [ ] 2.2 Accept `config.module` and `requires` in manifests and refuse a configuration installing a package whose requirements it does not install; verify end to end
- [ ] 2.3 Keep calls and subscriptions inside the caller's repository (including events of other extensions on it) and verify with the subscription test
- [ ] 2.4 Remove org configuration from the docs (Configuration, Extensions, Events, Architecture) and verify the docs build

## 3. The types module and Issues 0.6.0

- [ ] 3.1 Write the `tartan.dev/types` module in `cue/types` and verify with `cue vet` tests: each type evaluates; labels for missing options and defaults that do not fit are invalid
- [ ] 3.2 Issues 0.6.0: properties instead of fields; long text, person, URL and money; its module `tartan.dev/ext/issues` with `#Extension`; verify with Issues' unit tests and the value vectors
- [ ] 3.3 Write a docs page for the types module and update the Issues page, and verify the docs build

## 4. Kanban 0.3.0

- [ ] 4.1 Kanban's module `tartan.dev/ext/kanban`: imports Issues' module, `#issues`, boards with `columns` (a property, a subset, or `"state"`), `lanes` and `group`; verify with `cue vet` tests: a lookalike property, a non-select column and an unknown option fail
- [ ] 4.2 Repository-scoped Kanban with board views as lanes × columns × groups (multi-select under each value, "none" last, labels as titles); verify with Kanban's unit tests
- [ ] 4.3 Move the Kanban end-to-end test to a repository configuration importing the published modules, with lanes, groups and a project board, and verify moves still update issues
- [ ] 4.4 Update the Kanban docs page with a worked example and verify the docs build

## 5. Verification

- [ ] 5.1 Run `scripts/e2e.sh` and verify every suite passes
