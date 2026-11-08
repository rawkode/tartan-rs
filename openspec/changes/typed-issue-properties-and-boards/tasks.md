# Tasks

## 1. The forge as a CUE registry

- [ ] 1.1 Read the deployment configuration (CUE package `forge` at `TARTAN_CONFIG`: `modules`, `sources`) and verify with unit tests: valid lists, an unknown source, an unversioned module
- [ ] 1.2 Serve the OCI read API at `/v2/` from the object store (manifests, blobs, tag lists; writes answer 405) and verify with the `cue` CLI fetching a module from a test forge
- [ ] 1.3 Sync listed modules at startup with `oci-client` (idempotent, digest mismatches refused, unreachable sources reported) and register the packages inside them (manifest, version match, migrations, component); remove `POST /-/api/packages`; verify end to end against a test-only `distribution` source registry
- [ ] 1.4 Evaluate configuration exactly as committed against the forge's registry (no added files) and check the exported value's shape in Rust; verify with unit tests and an end-to-end refusal for an unlisted module
- [ ] 1.5 Make `scripts/build-extension.sh` build an extension module directory and publish it with `cue mod publish`; document the deployment configuration, the registry and authoring an extension module (Running locally, Deploying, Configuration, Extensions), and verify the docs build and lint

## 2. Repository only

- [ ] 2.1 Remove org installations, the org's configuration repository, contributions, org recipient scope, org `FlushTree` and the org extension routes; require `storage.scope: "repo"`; verify the forge, extension and subscription tests pass without them
- [ ] 2.2 Accept `requires` in manifests and refuse a configuration installing a package whose requirements it does not install; verify end to end
- [ ] 2.3 Keep calls and subscriptions inside the caller's repository (including events of other extensions on it) and verify with the subscription test
- [ ] 2.4 Remove org configuration from the docs (Configuration, Extensions, Events, Architecture) and verify the docs build

## 3. The types module and Issues 0.6.0

- [ ] 3.1 Write the `tartan.dev/types` module in `cue/types` and verify with `cue vet` tests: each type evaluates; labels for missing options and defaults that do not fit are invalid
- [ ] 3.2 Issues 0.6.0: properties instead of fields; long text, person, URL and money; its module `github.com/rawkode/tartan-ext/issues` with `#Extension` and the package inside it; verify with Issues' unit tests and the value vectors
- [ ] 3.3 Write a docs page for the types module and update the Issues page, and verify the docs build

## 4. Kanban 0.3.0

- [ ] 4.1 Kanban's module `github.com/rawkode/tartan-ext/kanban`: imports Issues' module, `#issues`, boards with `columns` (a property, a subset, or `"state"`), `lanes` and `group`; verify with `cue vet` tests: a lookalike property, a non-select column and an unknown option fail
- [ ] 4.2 Repository-scoped Kanban with board views as lanes × columns × groups (multi-select under each value, "none" last, labels as titles); verify with Kanban's unit tests
- [ ] 4.3 Move the Kanban end-to-end test to a repository configuration importing the modules the forge serves, with lanes, groups and a project board, and verify moves still update issues
- [ ] 4.4 Update the Kanban docs page with a worked example and verify the docs build

## 5. Verification

- [ ] 5.1 Run `scripts/e2e.sh` and verify every suite passes
