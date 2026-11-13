# Tasks

## 1. Group actor and tree model

- [ ] 1.1 Add `Slug` path parsing (`NodePath`: segments, 1024-byte limit, `/-/` split) in `tartan-forge/src/slug.rs` with unit tests for valid, empty, too-long and `-` cases
- [ ] 1.2 Replace `actors/org.rs` with `actors/group.rs` (Rust type `Group`, Rivet actor name still `org`) (`children` table, `CreateChild`, `ResolveChild`, `ListChildren`, `group.ready`) and verify its unit tests: sibling name collision across kinds answers 409, repeat create returns the same id
- [ ] 1.3 Drop the `orgs` table from `actors/forge.rs`; record the root group id; move placement resolution to `group/<path>` and `repo/<path>` with nearest-ancestor lookup and verify the placement tests, including the ancestor-wins scenario
- [ ] 1.4 Change `actors/repo.rs` to store `parent_id`, `path`, `git_owner` and `git_name`; build events from the path; verify `repo.created` carries `path` and `parent_id`
- [ ] 1.5 Update `lib.rs`, `clients.rs` and `envelope.rs` for the `group:<id>` source and handles

## 2. Git host locations and path clone URLs

- [ ] 2.1 Change `GitHost` (`services.rs`, `walgit.rs`) to take a location and create new repositories as `tartan/<repository id>`; verify with a unit test against a fake host
- [ ] 2.2 Map `/-/hooks/git-host` events from `owner/name` to a repository through a `git_locations` index; verify unknown locations are still skipped and repeats are idempotent
- [ ] 2.3 Add the gateway Git proxy (root fallback that does not shadow `/-/` or `/v2/`; only the three endpoints; forward query, Content-Type, Content-Encoding, Git-Protocol, Accept; drop Cookie; no timeout; `reqwest` `stream` feature; never log Authorization; `TARTAN_PUBLIC_URL` in `Services` for clone URLs) for `<path>.git/{info/refs,git-upload-pack,git-receive-pack}`, streaming and forwarding `Authorization`; verify an e2e clone, push, branch create and delete at a nested path, a 404 for a group path, and a push of at least 50 MB
- [ ] 2.4 Make the web dev server, `scripts/dev.sh`, `scripts/demo.sh` and the documented production proxy forward `*.git/*` to the gateway; verify `git clone http://127.0.0.1:<web port>/acme/api.git` against the demo

## 3. Migration

- [ ] 3.0 Add the `legacy-org-seed` test-support feature that builds an org-based forge (old schemas, orgs, repos, pending rows, placements, a subscriber cursor) and the migration fault-injection point; verify a seeded forge reads back as the old model
- [ ] 3.1 Implement `Group::Adopt`, the repository back-fill and the forge's startup migration with the source rename; fill root `children` and `git_locations`, rewrite `org/<org>` placements and subscriber cursors, add the `ALTER TABLE` steps, and gate the gateway and webhook (503) until complete; verify with an e2e test that upgrades a forge holding `acme/api` with issues, a signed commit, a placement and a subscriber cursor: the group's `ReadEvents` returns the pre-upgrade history, the pre-upgrade subscriber receives the next group event without redelivery or a gap, a push and clone of `acme/api` after migration reach its log, and a second start migrates nothing
- [ ] 3.2 Verify crash safety with the fault-injection point between adopting groups and dropping `orgs`: restart completes the migration without duplicate events, and a webhook sent while migration is incomplete answers 503 and is not lost

## 4. Tree API and routes

- [ ] 4.0 Add the shared path-and-resource grammar parser used by `authenticate` and the dispatcher, derive the access class from method and parsed resource (never the route template), cache path → (kind, id, pool); port the `route_classes` and `repository_limits` unit tests to it, including percent-encoded `%2F-%2F`, and verify that a write-scoped token without admin cannot create a group and that signature recheck still reaches the uniform 404
- [ ] 4.1 Add `/-/api/tree` handlers in `tartan/src/gateway.rs` (list, read, create) with the `/-/` resource dispatch; verify with `tests/forge.rs` against every scenario of `forge-tree`, including `?flat=1`
- [ ] 4.2 Move extension, signature, proof and config-status routes onto the dispatcher and delete the `/orgs/...` routes; verify the existing extension and signature tests pass at tree paths
- [ ] 4.3 Make the signature-recheck 404 identical for unknown groups, repositories and malformed paths; verify with a probing test
- [ ] 4.4 Update `access.rs` token limits to repository paths and `group/*` prefixes; verify the group-limited token scenarios and the 400 for malformed entries

## 5. Participants

- [ ] 5.1 Add `participants` to `tartan-config/src/deployment.rs` (it refuses unknown keys), carry it in `GatewayState` and `Services` so the identity actor's sign-in rule and the stranger cap treat participants as readers, log the mode at startup, and verify the upgraded-forge-stays-closed scenario and a participant's sign-in recording `person.signed_in`
- [ ] 5.2 Replace `read_only: bool` in `actors/extension.rs` with an access level checked at every nested call (missing or unknown `access` is `write`), add `participate` to the SDK and the WIT doc comments, pass the role in `slot-context.extra`, and verify a participant is refused on a `write` tool with 403 and that a participate tool calling another installation's write tool is refused with `denied`
- [ ] 5.3 Resolve a tool's declared access in the gateway (cached by package version) and add the per-DID and forge-wide participation limits for `participate` calls only; verify the 429 scenario, that `list`/`get` never spend the budget, and that owners are never limited
- [ ] 5.3a Restrict token creation for participants who are not readers, keep non-owner token scopes at `read`, and update `valid_repo_entry` for top-level paths, `group/*` at any depth and legacy `org/*`; verify with token tests
- [ ] 5.4 Update `tests/access.rs` (including `Participant reads a private forge` and nested-repository `Another repository` denial in extension calls, and the two-forges-one-engine scenario) for orgs removal and add participant scenarios (open issue, comment, close refused, create group refused, listed participants)

## 6. Issues extension

- [x] 6.1 Add the `comment` tool, a comments table migration and comments in `get`; declare `create` and `comment` as `participate` in `tartan.json`; verify the extension's unit tests for comment, empty text and unknown issue
- [x] 6.2 Make `create` refuse property values from non-owners with `denied`; verify both scenarios of "Participants open issues with a title and a body only"
- [ ] 6.3 Rebuild and republish the module (bump its version) and verify the demo configuration still resolves it

## 7. tt

- [ ] 7.1 Make `tt sign recheck --repo` and the other path arguments in `cli.rs` and `signcmd.rs` take a tree path; verify with a nested-path test against the tree API
- [ ] 7.2 Update tt output that names orgs, and verify `tt` tests and `tt --help` text

## 8. Web UI

- [x] 8.1 Change `router.ts`, `api.ts`, `repo-context.ts` and the views to path routing (`#/<path>`, `#/<repo path>/-/<page>`) with group listings and breadcrumbs; verify with vitest router and API tests (including the mock API in `web/src/lib/sandbox.ts`)
- [x] 8.1a Build the sidebar tree (lazy expansion, icons, active highlight, ancestors expanded from the URL, per-browser expansion) and the jump field; verify with vitest component tests for the `tree-navigation` expansion, breadcrumb and jump scenarios
- [ ] 8.1b Add rolled-up open-issue counters (repository count set from Issues snapshot events delivered as a recipient; per-child `(count, version)` rows summed in each group and sent upward through the outbox; seeding by recounting migrated repositories; counts in listings; `GET /-/api/tree?flat=1`); verify the rolled-up and issue-opened scenarios in `tests/forge.rs`
- [x] 8.2 Show create-group and create-repository controls to owners only; verify with view tests for owner, participant and reader
- [x] 8.3 Show participants the New issue and Comment controls and the participant notice, and keep the read-only and private-forge pages; verify with `SignInView` and issues view tests
- [ ] 8.4 Run the web unit tests (`npm --prefix web test`) and a browser check against `scripts/demo.sh`

## 9. Demo, scripts and docs

- [ ] 9.1 Reseed `scripts/demo.sh` and `scripts/dev.sh` with a top-level repository, a group `acme` and a nested `acme/platform/api`, and sign in a participant (`@bob.test`) who can comment; verify by running the demo
- [ ] 9.2 Update the Nimbus docs in `docs/`: tree and paths, the `/-/api/tree` reference, `participants` and `readers`, token repository limits, clone URLs, the upgrade (snapshot first, orgs become groups, event renames); verify `docs/` builds and every documented command runs as written
- [ ] 9.3 Update `README.md` for groups and participants

## 10. Integration

- [ ] 10.0 Add delta specs for the main specs this change leaves with org wording (access-tokens 'A token acts as an agent…', extension-runtime's events requirement, the forge-hierarchy purpose, `extension.installed` payload) and split the three requirements over 500 characters; verify `openspec validate single-owner-group-tree --strict` has no warnings

- [ ] 10.1 Run `WALGIT_BIN=… scripts/e2e.sh` (fmt, clippy `-D warnings`, every test) and verify it passes

## Workflow follow-up

- Archive the change once reviewed, applying its specs to `openspec/specs`.
- Consider a change for moving, renaming and deleting nodes.
