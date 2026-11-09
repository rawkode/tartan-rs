# issues.tartan.rawkode.academy

Per-repository issues for Tartan, written in Rust and compiled to a WebAssembly component (`tartan:ext@0.1.0`). Each
repository's installation has its own SQLite database (`storage.scope: "repo"`) and its own issue numbers (`#1`, `#2`,
…). An issue may belong to a project of the repository, and has the typed properties the repository's configuration
defines.

## The module

Issues is one CUE module, `tartan.rawkode.academy/issues`: its settings schema (`cue/`) and its package
(`tartan.json`, `component.wasm`, `migrations/`). `#Extension` fixes the id and the version, and types the settings
with [`tartan.rawkode.academy/types`](../../cue/types):

```cue
package tartan

import (
	"tartan.rawkode.academy/types"
	"tartan.rawkode.academy/issues"
)

extensions: "issues.tartan.rawkode.academy": issues.#Extension & {settings: properties: {
	status: types.#Select & {options: ["todo", "doing", "done"], labels: todo: "To do", default: "todo"}
	notes:  types.#LongText
	budget: types.#Money & {currency: "GBP"}
}}
```

Each property records its name (`settings.properties.status.name == "status"`), so another extension's configuration,
such as Kanban's boards, can refer to the property itself.

## Values

| Kind          | Stored as                                   | Form                          |
| ------------- | ------------------------------------------- | ----------------------------- |
| `shorttext`   | one line, at most `maxLength` (≤ 200)       | text                          |
| `longtext`    | Markdown, at most `maxLength` (≤ 20000)     | text area; shown as Markdown  |
| `number`      | a number within `min` and `max`             | number                        |
| `date`        | `YYYY-MM-DD`, a real day                    | date                          |
| `boolean`     | true or false                               | checkbox                      |
| `select`      | one of `options`                            | dropdown, titles from labels  |
| `multiselect` | distinct `options`                          | multi-select                  |
| `person`      | a principal id, such as `usr_dev`           | text                          |
| `url`         | an `http` or `https` URL (≤ 2048)           | URL                           |
| `money`       | whole minor units: `1250` is GBP 12.50      | decimal text, in its currency |

A value is checked when it is written; a default fills in when an issue is read. Changing the configuration never
rewrites stored values: a removed property or option stays on the issues that have it. `testdata/property-values.json`
holds the values each kind accepts and refuses; `cue/types/test.sh` checks the same vectors as CUE defaults, so the
module and the extension agree.

## Tools and slots

| Name                | Role      | What it does                                                                     |
| ------------------- | --------- | -------------------------------------------------------------------------------- |
| `repo.tab` `issues` | Reporter  | counts, the newest 100 issues and a "New issue" form (action `create`)           |
| slot `issue`        | Reporter  | one issue (`props.number`): its body and long texts as Markdown, its properties |
| tool `list`         | Reporter  | `state` (`open` by default, `closed`, `all`), `limit` 1–100 (30); no bodies      |
| tool `get`          | Reporter  | one issue by `number`, with its body                                             |
| tool `properties`   | Reporter  | the repository's properties, as configured                                       |
| tool `create`       | Reporter  | `title`, `body` (Markdown), `project`, `properties`; the caller authors          |
| tool `update`       | Developer | title, body, `properties` and `state` in one write; `expectedVersion`            |
| tool `close`        | Developer | closes an issue; `changed: false` when it already was closed                     |
| tool `reopen`       | Developer | reopens an issue; `changed: false` when it already was open                      |

Every write emits `x.issues.tartan.rawkode.academy.issue.created` or `.updated` with the issue's number, title, project, state,
stored `properties`, version and update time (not its body), so another extension keeps a copy by keeping the highest
version.

Layout: `src/issue.rs` (argument checks, typed values, JSON shapes and UI documents; pure), `src/store.rs` (the `issues`
table), `src/lib.rs` (the hooks), `migrations/`, `cue/` (the module's CUE), `protocol.md`, `tartan.json`.

```sh
cargo test --manifest-path extensions/issues/Cargo.toml
scripts/build-extension.sh extensions/issues <out-dir>
```

License: MIT.
