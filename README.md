# Tartan

An event-driven Git forge. The backend is Rust on [Rivet Actors](https://rivet.dev); Git hosting and repository storage
are [walgit](https://github.com/tobi/walgit), which keeps repositories in an S3-compatible object store
([RustFS](https://rustfs.com) in development). The web frontend will be Astro, TypeScript and Vue.

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
- **Extensions.** `POST /-/api/packages` publishes a package (manifest, base64 component, migrations) to object
  storage; `POST /-/api/orgs/<org>/repos/<repo>/extensions` installs one, and
  `…/extensions/<id>/tools/<tool>`, `…/render/<slot>` and `…/actions/<action>` call it. Every API call acts as
  `admin` until the gateway has identities.
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

4. Org installations and calls between extensions. **Done:** a package's storage scope (`org` or `repo`) decides
   where it installs; an org installation receives matching events from the whole org, replayed from the start
   when it subscribes. The `calls` import reaches another installation's tool as the same principal, if the
   manifest's `permissions.calls` names it and the target is in the same org (and, for a repository's
   installation, the same repository).

Not yet: gates (walgit has no pre-receive hook to call them from), `notify` and why-notes (refused as
`unavailable`), and identities beyond `admin`.

### First-party extensions

| Extension | Installs on | Does |
| --- | --- | --- |
| `extensions/issues` (`rawkode.issues`) | a repository | numbered issues with configured fields; emits `x.rawkode.issues.issue.created` and `.updated` snapshots |
| `extensions/kanban` (`rawkode.kanban`) | an org | boards for the org, a repository or a project, each with its own field and columns; cards follow Issues' snapshots, and moving one calls Issues' `update` |
| `extensions/fixture` (`tartan.fixture`) | a repository | test extension for event delivery |

### Goal 3: the web frontend

Astro, TypeScript and Vue against the gateway's API. Not started.

## Layout

| Crate | Holds |
| --- | --- |
| `tartan-forge` | the actors, the event log and outbox, the walgit client and webhook contract |
| `tartan-extensions` | the wasmtime host of the `tartan:ext` world, packages and migrations |
| `tartan` | the binary: the HTTP gateway and the actor host |

## Running

Needs Rust 1.98, `git`, `rustfs` and `rclone` on `PATH`, and a walgit binary built from
[github.com/tobi/walgit](https://github.com/tobi/walgit) at `80e9a20` (its build needs `protoc`):
`cargo build --release -p walgit-cli`, then set `WALGIT_BIN` to `target/release/walgit`.

```sh
WALGIT_BIN=/path/to/walgit scripts/e2e.sh   # fmt, clippy and every test, git included
WALGIT_BIN=/path/to/walgit scripts/dev.sh   # RustFS, the Rivet engine, walgit and the gateway
```

`e2e.sh` also needs `wasm-tools` and the `wasm32-unknown-unknown` target: `scripts/build-extension.sh` packages an
extension crate (`extensions/*`, built on `sdk/rust/tartan-ext`) as `component.wasm`, `tartan.json` and its
migrations.

`scripts/rivet-engine.sh` downloads the Rivet engine matching the `rivetkit` crate and verifies its checksum.

## Style

[Google's Rust style guide](https://google.github.io/styleguide/rustguide.html), `cargo fmt` with `rustfmt.toml`, and
`cargo clippy --all-targets -- -D warnings`.
