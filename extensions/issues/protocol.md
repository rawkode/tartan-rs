# Issues (issues.tartan.rawkode.academy)

Each repository has its own numbered issues (`#1`, `#2`, …), open or closed. An issue may belong to a project, named by its root path (`crates/lsp`), and has the typed properties the repository's configuration defines.

- `issues_properties` lists the repository's properties: each one's `name`, `kind` (`shorttext`, `longtext`, `number`, `date`, `boolean`, `select`, `multiselect`, `person`, `url` or `money`) and its constraints (`options`, `labels`, `min`, `max`, `maxLength`, `currency`, `default`). Read it before setting properties.
- `issues_list` lists a repository's issues, newest first (`state`: `open` by default, `closed` or `all`; `limit` ≤ 100; `project` for one project's, with its counts). It returns no bodies; `issues_get` returns one issue with its body.
- `issues_create` opens an issue (`title` ≤ 200 characters, `body` ≤ 20000, Markdown; `project` optional; `properties` by name). Search the open issues first instead of filing a duplicate.
- `issues_update` changes the title, body, `properties` or `state` in one write (Developer); null, an empty string or an empty list clears a property (and on `issues_create` leaves it unset), and `expectedVersion` refuses a stale write.
- `issues_close` and `issues_reopen` change an issue's state (Developer). Both are idempotent: `changed` is false when the issue already was in that state.
- With `project`, `issues_get`, `issues_close` and `issues_reopen` find only that project's issues.

Property values: text as strings (`longtext` is Markdown), `date` as `YYYY-MM-DD`, `person` as a DID (`did:plc:ewvi7nxzyoun6zhxrhs64oiz` or `did:web:rawkode.dev`; resolve a handle with the forge's `POST /-/api/people/resolve`), `url` as an `http` or `https` URL, `money` as an integer of minor units in the property's currency (`1250` is GBP 12.50), `multiselect` as a list of distinct options. A value that does not fit its property is refused with `invalid`.

Over MCP `project` is taken as written: it is not checked against the project graph or a project's switch.

The author of an issue is the caller (for an agent, the user it acts for is recorded too).
