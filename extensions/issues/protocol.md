# Issues (rawkode.issues)

Each repository has its own numbered issues (`#1`, `#2`, …), open or closed. An issue may belong to a project, named by its root path (`crates/lsp`).

- `issues_list` lists a repository's issues, newest first (`state`: `open` by default, `closed` or `all`; `limit` ≤ 100; `project` for one project's, with its counts). It returns no bodies; `issues_get` returns one issue with its body.
- `issues_create` opens an issue (`title` ≤ 200 characters, `body` ≤ 20000, Markdown; `project` optional). Search the open issues first instead of filing a duplicate.
- `issues_close` and `issues_reopen` change an issue's state (Developer). Both are idempotent: `changed` is false when the issue already was in that state.
- With `project`, `issues_get`, `issues_close` and `issues_reopen` find only that project's issues.

Over MCP `project` is taken as written: it is not checked against the project graph or a project's switch.

The author of an issue is the caller (for an agent, the user it acts for is recorded too).
