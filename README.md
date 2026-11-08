# Tartan

An event-driven Git forge. The backend is Rust on [Rivet Actors](https://rivet.dev); Git hosting and repository storage
are [walgit](https://github.com/tobi/walgit), which keeps repositories in an S3-compatible object store
([RustFS](https://rustfs.com) in development). The web interface (`web/`) is Astro with Vue single-file components and
Panda CSS.

## Architecture

```
git CLI ──► walgit ──► object store (packs, WAL, CAS manifest)
               │ ref events (signed webhook, at least once)
               ▼
browser ──► tartan gateway ──► actors on Rivet, each with its own SQLite
                                 forge ─ org ─ repo        subscribers
                                   │      │     │              ▲
                                   └──────┴─────┴── outbox ────┘  (events, in order per source)
```

- **Tiers are actors.** `forge` (one per forge id), `org` and `repo` actors each own a Rivet SQLite database.
- **Everything is an event.** Every change appends an event to the changing actor's log in the same transaction. Each
  actor's outbox delivers events matching a subscriber's pattern (`repo.*`, `ref.>`, `>`) to the subscriber actor,
  in order per source and at least once; subscribers deduplicate on `(source, seq)`.
- **Git is walgit's job.** Tartan creates repositories through walgit's API and turns walgit's ref events into
  `push.accepted` and `ref.updated` events on the repository actor. Git clients send their token up front:
  `git -c http.extraHeader="Authorization: Bearer <token>" push …`. Locally walgit's event bridge wakes on a 1 s
  sweep; in production, point the object store's bucket notifications at walgit's `/_events/notify`.
- **Extensions.** Packages arrive as CUE modules that the deployment configuration lists and the forge syncs into its
  own registry. Extensions are installed and configured only by the CUE package `tartan` committed at a repository's
  root, applied on each push to `main`; `…/extensions/<id>/tools/<tool>`,
  `…/render/<slot>` and `…/actions/<action>` call an installation. Every API call acts as `admin` until the
  gateway has identities.
- **Docs and specs.** `docs/` is the Nimbus documentation site; `openspec/` holds the specs and changes.
- **Placement.** The forge actor maps `org/<org>` or `repo/<org>/<repo>` to a Rivet pool. New actors for that target
  are created in the pool, so a large org or repository runs on its own machines while staying part of the forge.

## Goals

### Goal 1: the forge core

1. Forge, org and repo actors with their own SQLite, created through the HTTP API. **Done.**
2. Every change is an event; subscribers receive matching events in order per source, at least once. **Done.**
3. Ref events from the Git host become forge events exactly once per `(seq, ref)`, behind a signed webhook. **Done**
   against walgit's documented event contract.
4. Placement puts an org's and a repository's actors in a named pool. **Done.**
5. `git clone`, `fetch` and `push` through walgit on RustFS, with the real webhook, and a clone from a second walgit
   host with an empty cache. **Done.**

### Goal 2: WASM extensions

Extensions are WebAssembly components of the `tartan:ext@0.1.0` world (`wit/tartan.wit`), so existing extensions
such as Issues run unchanged.

1. A wasmtime host for the world: guarded SQL against the installation's database, migrations, configuration,
   clock and randomness, buffered effects, and fuel, memory and time limits per call. **Done:** the Issues
   component creates, lists and renders issues on it.
2. Packages in object storage and installations on a repository, each an `extension` actor with its own SQLite;
   tools, render and actions over the API. **Done:** Issues is published, installed on a repository and used over
   HTTP. Packages are immutable per `id@version`.
3. `on-event` subscriptions and `effects.emit` through the outbox. **Done:** an installation receives the events of
   its repository that match its manifest's `subscribe` patterns, once each and in order; an event the extension
   fails is kept as a dead letter and does not block the next; what it emits (`x.<extension>.<type>`) reaches the
   forge's subscribers. `extensions/fixture` is the test extension.

4. Extensions working together on a repository. **Done:** every package installs on repositories
   (`storage.scope: "repo"`). An installation receives matching events from its repository and from the other
   extensions installed on it, including what they emitted before it subscribed. The `calls` import reaches
   another installation's tool on the same repository as the same principal, if the manifest's
   `permissions.calls` names it. A manifest's `requires` names extensions the same configuration must install.

Not yet: gates (walgit has no pre-receive hook to call them from), `notify` and why-notes (refused as
`unavailable`), and identities beyond `admin`.

### First-party extensions

| Extension | Installs on | Does |
| --- | --- | --- |
| `extensions/issues` (`rawkode.issues`) | a repository | numbered issues with typed properties from `tartan.dev/types`; emits `x.rawkode.issues.issue.created` and `.updated` snapshots |
| `extensions/kanban` (`rawkode.kanban`) | a repository | boards for the repository or a project, with columns, lanes and groups from Issues' properties; cards follow Issues' snapshots, and moving one calls Issues' `update`; requires Issues |
| `extensions/fixture` (`tartan.fixture`) | a repository | test extension for event delivery |

### Goal 3: the web frontend

Astro, Vue single-file components and Panda CSS against the gateway's API (`web/`). **Done:** orgs and
repositories, a repository's configuration steps, issues with every property type, and Kanban boards with drag and
drop.

## Layout

| Crate | Holds |
| --- | --- |
| `tartan-forge` | the actors, the event log and outbox, the walgit client and webhook contract |
| `tartan-extensions` | the wasmtime host of the `tartan:ext` world, packages and migrations |
| `tartan` | the binary: the HTTP gateway, the forge's CUE registry and the actor host |
| `tartan-config` | repository and deployment configuration, evaluated natively as CUE |

## Demo

```sh
WALGIT_BIN=/path/to/walgit REGISTRY_BIN=/path/to/registry CUE_BIN=/path/to/cue scripts/demo.sh
```

Starts RustFS, walgit, a source CUE registry with the types, Issues and Kanban modules, the Rivet engine, the gateway
and the web interface, then seeds org `acme` and repository `acme/api`: a configuration pushed with `git` that
installs Issues with typed properties and Kanban with two boards, and a dozen issues. Open
`http://127.0.0.1:4321/#/acme/api/boards/triage`; the admin token is `dev-admin-token`. Ctrl-C stops everything.
State lives in `.dev/demo`; each run starts fresh, and `--keep` reuses the previous one. See `docs/` (Demo).

## Running

Needs Rust 1.98, `git`, `rustfs` and `rclone` on `PATH`, and a walgit binary built from
[github.com/tobi/walgit](https://github.com/tobi/walgit) at `80e9a20` (its build needs `protoc`):
`cargo build --release -p walgit-cli`, then set `WALGIT_BIN` to `target/release/walgit`.

```sh
WALGIT_BIN=/path/to/walgit scripts/e2e.sh   # fmt, clippy and every test, git included
WALGIT_BIN=/path/to/walgit scripts/dev.sh   # RustFS, the Rivet engine, walgit and the gateway
```

Both also need `REGISTRY_BIN` (a CNCF distribution `registry` binary), `cue` (or `CUE_BIN`), `jq`, `wasm-tools` and
the `wasm32-unknown-unknown` target: `scripts/build-extension.sh` builds an extension crate (`extensions/*`, built on
`sdk/rust/tartan-ext`) into a CUE module directory holding its CUE, `component.wasm`, `tartan.json` and its
migrations.

`scripts/rivet-engine.sh` downloads the Rivet engine matching the `rivetkit` crate and verifies its checksum.

## License

The forge (`crates/*`) is AGPL-3.0-or-later: it links [cuengine](https://crates.io/crates/cuengine), which is
AGPL-licensed, to evaluate CUE. The extension SDK (`sdk/`) and the first-party extensions (`extensions/`) are MIT;
they are separate WebAssembly components and do not link the forge.

## Style

[Google's Rust style guide](https://google.github.io/styleguide/rustguide.html), `cargo fmt` with `rustfmt.toml`, and
`cargo clippy --all-targets -- -D warnings`.
