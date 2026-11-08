# Tasks

## 1. Documentation site

- [x] 1.1 Scaffold Nimbus in `docs/` with `npx @cloudflare/create-nimbus-docs`, set the site name and navigation, and verify `npm run build` in `docs/` succeeds
- [x] 1.2 Write pages for what exists — architecture, running locally (`scripts/dev.sh`, `scripts/e2e.sh`), events and subscribers, Git hosting, placement, extensions (packages, the `tartan:ext` world, limits, events, calls), Issues and Kanban — and verify the build and that every page appears in the navigation

## 2. Reading configuration

- [ ] 2.1 Add `read_file` to `GitHost`, implement it for walgit's blob API and for the test hosts, and verify it returns `None` for a missing file in a unit or end-to-end test

## 3. Applying a configuration

- [ ] 3.1 Make the extension actor apply a spec (install, upgrade with new migrations and `init`, configuration change) and add `Disable`; verify a disabled installation's calls answer 404 and re-enabling keeps its data
- [ ] 3.2 Reconcile an actor's installations with a configuration all or nothing, recording `extension.*`, `config.applied` and `config.refused`; verify with the scenarios in `specs/config-reconciliation`
- [ ] 3.3 Reconcile a repository on pushes to `main` (latest commit wins, other branches ignored) and verify a pushed `tartan.cue` installs, reconfigures and uninstalls Issues
- [ ] 3.4 Forward the `tartan` repository's configuration to the org actor and install org extensions from it; verify a pushed `acme/tartan` installs Kanban and refuses an entry without a version
- [ ] 3.5 Store repository contributions on the org actor and fold them into org installations' configuration as `repos`; verify Kanban's configuration carries a repository's contributed settings
- [ ] 3.6 Remove the install routes from the gateway and verify `POST …/extensions` answers 405
- [ ] 3.7 Document `tartan.cue`, the org's `tartan` repository, contributions, and the `config.*`/`extension.*` events in `docs/`, and verify the docs build

## 4. Kanban boards in configuration

- [ ] 4.1 Read boards from Kanban's configuration (org boards and prefixed repository contributions, project boards), remove `create_board` and `delete_board`, report invalid boards in `list_boards`, and verify with Kanban's unit tests
- [ ] 4.2 Move the Kanban end-to-end test to pushed `tartan.cue` for Issues fields, org boards and repository and project boards, and verify moves and the permission refusal still pass
- [ ] 4.3 Document Kanban's board settings with a worked `tartan.cue` example in `docs/` and verify the docs build

## 5. Tests and verification

- [ ] 5.1 Move the extension and subscription end-to-end tests to configuration pushed through a test Git host, and verify they pass
- [ ] 5.2 Extend the git end-to-end test to push a `tartan.cue` through walgit and verify Issues is installed from it
- [ ] 5.3 Run `scripts/e2e.sh` and verify every suite passes
