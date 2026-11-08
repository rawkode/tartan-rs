# rawkode.issues

Per-repository issues for Tartan, written in **Rust and compiled to a WebAssembly component** (`tartan:ext@0.1.0`). It
is published and installed like any third-party package (`id` outside `tartan.*`, `runtime: "wasm"`, no permissions),
and runs as a Dynamic Worker facet of its installation's own Durable Object, with its own SQLite: `storage.scope:
"repo"`, so each repository has its own database and its own issue numbers (`#1`, `#2`, …). An issue may belong to a
project of the repository.

| Contribution        | Role      | What it does                                                                         |
| ------------------- | --------- | ------------------------------------------------------------------------------------ |
| `repo.tab` `issues` | Reporter  | the Issues page: open and closed counts, the newest 100 issues, a "New issue" form   |
| action `create`     | Reporter  | the form's submit: opens an issue as the viewer, then a toast and a re-render        |
| tool `list`         | Reporter  | `state` (`open` by default, `closed`, `all`), `limit` 1–100 (30); no bodies          |
| tool `get`          | Reporter  | one issue by `number`, with its body                                                 |
| tool `create`       | Reporter  | `title` (one line, ≤ 200 characters), `body` (Markdown, ≤ 20000); the caller authors |
| tool `close`        | Developer | closes an issue by `number`; `changed: false` when it already was closed             |
| tool `reopen`       | Developer | reopens an issue by `number`; `changed: false` when it already was open              |
| `protocol.md`       |           | the MCP protocol card                                                                |

| tool `update` | Developer | title, body, `fields` and `state` in one write; `expectedVersion` refuses a stale one |
| tool `fields` | Reporter | the fields this repository's `tartan.cue` defines |

Over MCP the tools are `issues_list`, `issues_get`, `issues_fields`, `issues_create`, `issues_update`, `issues_close` and `issues_reopen`. All of them are also exported as `rawkode.issues/issues@1` for other extensions that declare it ([Data access](../../docs/extensions.md#data-access)).

Fields come from the repository's root `tartan.cue`, checked against `config/settings.cue`:

```cue
package tartan

import "tartan.dev/ext/types"

extensions: "rawkode.issues": settings: fields: {
	status:   types.#Select & {options: ["todo", "doing", "done"], default: "todo"}
	tags:     types.#MultiSelect & {options: ["bug", "docs"]}
	estimate: types.#Number & {min: 0}
	budget:   types.#Monetary & {currency: "GBP"}
	due:      types.#Date
	urgent:   types.#Boolean & {default: false}
	note:     types.#Text & {label: "Note", maxLength: 200}
}
```

| Type           | Options                         | Stored as                              | Form                                  |
| -------------- | ------------------------------- | -------------------------------------- | ------------------------------------- |
| `#Text`        | `maxLength` (at most 1000)      | string                                 | text                                  |
| `#Number`      | `min`, `max`                    | number                                 | number                                |
| `#Select`      | `options`                       | one of the options                     | dropdown                              |
| `#MultiSelect` | `options`                       | list of options                        | multi-select                          |
| `#Monetary`    | `currency`, a three-letter code | whole minor units: `1250` is GBP 12.50 | decimal text, in the field's currency |
| `#Date`        |                                 | `YYYY-MM-DD`                           | date                                  |
| `#Boolean`     |                                 | true or false                          | checkbox                              |

Every type also takes `label` and `default`. A monetary field has one currency, and its amounts stay exact: they are integers, and the form reads them from decimal text without going through a float. ISO 4217 sets each code's decimal places; any other code has two.

A value is checked when it's written. A default fills in when an issue is read. Changing the config never rewrites stored values: a removed field or option stays on the issues that have it.
Opening an
issue is a Reporter action, like commenting: the host applies the tab's role to its form, so the `create` tool has the
same role. Changing an issue's state needs Developer and has no button in the tab yet, since a slot's actions all share
the slot's role. An issue's author is the actor the host gives the call (`kind` and `id`, and `on_behalf_of` for an
agent acting for a user).

## Projects

Every tool takes an optional `project` and every issue carries `project` (null for an issue of the whole repository). It
is the project's **root path** (`crates/lsp`), not its name, and it is stored as written when the issue is opened:

- `list` with `project` filters the issues and their counts to it, by exact root: a nested project's issues are its own.
- `create` files the issue against it. The tab's form files repo-wide issues and never reads `project`.
- `get`, `close` and `reopen` with `project` answer `not_found` for another project's issue. Without it they reach any
  issue in the repository.
- Without `project`, `list` is repo-wide: every issue, whichever project it belongs to.

The extension cannot see the project graph, so it checks only the path's shape. The kernel's JSON route
(`POST /-/api/repos/<repoId>/ext/rawkode.issues/tools/<tool>`, [docs/projects.md](../../docs/projects.md)) resolves a
project's name to its root, refuses an unknown one and refuses one that switched Issues off.

What follows from that, and is not handled:

- **Renaming a project keeps its issues; moving it does not.** A change to `name:` leaves the root alone. A moved
  directory is a new root, so its old issues stay filed under the old path: the project's list no longer shows them, and
  the project's `get`, `close` and `reopen` answer `not_found`. They still appear repo-wide. Nothing rewrites them, and
  no issue can be refiled.
- **A deleted project's issues stay** under its old path, and repo-wide.
- **Over MCP, `project` is not checked.** An MCP caller's string is stored as written, against any path or against a
  project with Issues switched off. The per-project switch scopes the JSON route only ([docs/projects.md](../../docs/projects.md)).

Layout: `src/issue.rs` (argument checks, JSON shapes and the tab document; pure), `src/store.rs` (the `issues` table),
`src/lib.rs` (the hooks), `migrations/` (`0002` adds the nullable `project` and its index), `protocol.md`, `tartan.json`.

```sh
cargo test --manifest-path extensions/issues/Cargo.toml
deno task build:ext issues      # → extensions/issues/dist/ (never committed)
```

The build needs `wasm-tools` 1.248.0 (`cargo install --locked wasm-tools@1.248.0`, or `WASM_TOOLS`). The authoring guide
is [docs/ext/](../../docs/ext/README.md). License: MIT.
