# Design

## Context

Today the forge actor owns an `orgs` table, an `Org` actor per org owns its repositories, and a `Repo` actor stores `org_id`, `org_slug` and `slug` and creates its repository on walgit as `<org>/<repo>`. Routes are `/-/api/orgs/{org}/repos/{repo}/…`. Placements, access-token repository limits, signature routes and the `tt sign recheck --repo` argument all assume exactly two path segments. walgit routes `/{owner}/{repo}` and so names a repository by exactly two segments, which rules out nested Git host names.

Access has three rungs (owner, reader, none). Extension tools declare `access: "read" | "write"`; Issues has no comment tool. See proposal.md for the motivation.

## Goals / Non-Goals

**Goals:**
- One tree, one namespace per parent, any depth, resolved by path.
- Existing forges upgrade in place with no data loss, no changed clone URL and no Git host move.
- A participant rung that can open issues and comment and nothing else.

**Non-Goals:**
- Moving, renaming or deleting nodes. A path is permanent, which keeps actor keys, event history and signature records stable.
- Participant-owned state beyond issues and comments; per-node permissions; tokens for participants.
- Enforcing the forge's `readers` policy on Git reads: anonymous reads stay allowed, as with the Git host today. Through the public origin they now also answer 200 versus 404 for a path, so the Deploying docs say clone access is public by path and that the uniform 404 covers the API only.

## Decisions

### 1. Groups are actors; the root is a group

One `Group` actor per group, keyed by id, replaces `Org`. The Rust type is renamed but the Rivet actor name stays `org` for every group, new or migrated: Rivet finds an actor by name and key, so renaming the registered name would address a new, empty actor and orphan a migrated group's repository list, event log and subscriber cursors. It owns a `children` table `(slug, kind, id)` with `slug` unique, so a group and a repository cannot share a name, and records its own `parent_id` and path. The forge actor loses its `orgs` table and records only the id of the root group, which has no slug and no parent and is a `Group` actor like the rest.

Creation walks the path from the root through `ResolveChild` calls, then sends `CreateChild` to the parent. This reuses the two-step init-then-mark-ready flow of `Org` and `Repo` so a retry after a failure finds the same node. A repository's `path` is stored on the node and carried in events; as nodes never move it never changes.

*Alternative:* a single `tree` table in the forge actor. Simpler lookups, but every create and every read serializes on one actor, and placement by pool (which is per actor) could not apply to a subtree. Group actors keep the placement model.

### 2. Path-addressed API with a `/-/` separator

`/-/api/tree/{*rest}` is one catch-all route. The handler splits `rest` at the first `/-/`: the left side is a node path and the right side a repository resource (`extensions/…`, `commits/<sha>/signature`, `signatures`, `proofs/<sha256>`, `config/status`). Slugs cannot be `-` (they cannot start or end with `-`), so the separator is unambiguous without reserving names. axum cannot place a wildcard before a static suffix, so the existing per-resource routes become resource handlers dispatched from this one. One parser, shared by `authenticate` and the dispatcher, splits the decoded `rest` into a node path and an exact resource grammar, matched segment by segment with no prefix or suffix matching, so an encoded `%2F-%2F` is never a separator. The access class comes from the method and the parsed resource, not from the route template: `GET` is read; `POST` of a child (no `/-/`) is admin; `POST /-/extensions/<ext>/{tools,render,actions}/<name>` is write, with `render` and read-declared tools as read; `POST /-/commits/<sha>/signature/recheck` is the signer's recheck; token repository limits are checked against the parsed path (a limited token may read a group above a listed repository, and `GET /tree`, `?flat=1` and creates are refused). The existing `route_classes` and `repository_limits` unit tests port to this grammar. Paths are permanent and nodes are never deleted, so the gateway caches path → (kind, id, pool) indefinitely, along with the root group's id.

Resolution is a lookup, not syntax: `a/b` is a group or a repository only by asking the tree.

*Alternative:* `/-/api/repos/<id>/…` by id. Cleaner routing, but every client would have to resolve a path to an id first, and the web UI's URLs are paths anyway.

### 3. Clone URLs are paths; the gateway proxies Git

The clone URL is `<TARTAN_PUBLIC_URL>/<path>.git`; actors learn `TARTAN_PUBLIC_URL` through `Services` to put it in `repo.created` and `repo.ready`. Before the upgrade a repository's advertised URL was `<TARTAN_GIT_HOST_URL>/<org>/<repo>.git`; after it, the public URL. Old remotes keep working because the walgit location is unchanged and the Git host stays reachable. The gateway handles `GET <path>.git/info/refs`, `POST <path>.git/git-upload-pack` and `POST <path>.git/git-receive-pack`: it splits the path at `.git/`, resolves it to a repository, and forwards only those three endpoints (so walgit's own `/{owner}/{repo}/api`, including create, is never exposed) to walgit at the repository's location with the client's `Authorization` header unchanged, plus the query string (`?service=`), `Content-Type`, `Content-Encoding` (git gzips `upload-pack` requests), `Git-Protocol` and `Accept`; it drops `Cookie`, never logs `Authorization`, sets no request timeout that would cut a long push, and streams bodies both ways (the workspace's `reqwest` gains its `stream` feature, or a hyper client is used). axum cannot match `{*path}.git/…`, so a root fallback does the matching and must not shadow `/-/` or `/v2/`. Pushes keep the Git host token as their credential, so attribution is unchanged.

walgit names a repository by two segments. New repositories are created as `tartan/<repository id>` (id lowercased to fit the slug rule); the `Repo` actor stores `git_owner` and `git_name`, and migrated repositories keep `<org>/<repo>`. `GitHost::create_repo`, `clone_url`, `is_ancestor` and reads take the location. walgit's ref events map from `owner/name` to the repository through a `git_locations` index in the root group; events for unknown locations stay skipped. Because the proxy owns the public URL, a repository's location can change later without changing its clone URL.

The origin that serves the web UI must forward `*.git/*` to the gateway as well as `/-/`: the Astro dev server (`web`), the demo and dev scripts, and the production proxy documented in Deploying.

*Alternative:* hand out the Git host's `tartan/<id>` URL. No proxy, but URLs are opaque and tied to walgit's layout; rejected because clone paths should be the URL path.

### 4. Participation is a third access class on tools

Manifest `access` becomes `read | participate | write`; a missing or unknown value is `write`. The manifest is raw JSON checked in `actors/extension.rs`, so the work is there, in the SDK and in the doc comments of `wit/tartan.wit`, not in a typed manifest. The gateway computes rights as `read`, `participate`, `write`, `admin`; owners hold all, a participant `read` and `participate`.

The extension actor's `read_only: bool`, carried from `CallTool` into `InstallationCaller`, becomes an access level (`Read`, `Participate`, `Write`) taken from the principal's rights and passed through every hop. At each nested call the target tool's declared access is checked: a call made during a `Participate` call may reach only `read` and `participate` tools (read tools read-only), never a `write` tool. A participate tool that calls another installation's write tool is refused with `denied`.

The caller's role reaches extensions in `slot-context.extra` (`{"role": "owner" | "participant" | "reader"}`), not as a new field of the WIT records, so the `tartan:ext@0.1.0` world is unchanged and published packages (Kanban, the fixture, older Issues versions) still fit. Participants can open issues only in repositories that pin the new Issues version, which the demo and dev configurations and the docs account for.

`participants` is parsed in `tartan-config/src/deployment.rs` (which refuses unknown keys) and carried in both `GatewayState` and `Services`: the identity actor's sign-in rule (`Services::can_read`, `people.rs`) and the stranger cap in `signatures.rs` must treat participants as readers.

The rate limit must not spend the budget on read tools, which the issues page calls on every load. The gateway resolves the tool's declared access (installation query, then the package manifest, cached by package version) before limiting: only `participate` calls are limited, per DID, with a forge-wide ceiling as well because `signed_in` admits any number of DIDs. Owners are not limited.

Tokens: participants hold no rights beyond reading and participating. Creating access tokens needs `read` today, which a participant has as a reader; tokens a person who is only a participant (not a reader) creates are refused, and any token's scopes never exceed `read` for non-owners. `valid_repo_entry` in `actors/identity/tokens.rs` accepts one-segment top-level paths and `group/*` at any depth, and existing `org/*` entries keep their meaning (a top-level group's whole subtree).

*Alternative:* let `readers` also write. Fails the requirement that visitors cannot touch anything but issues and comments.

### 5. Migration runs once, before the gateway serves

At startup, before the gateway binds, the forge actor finds rows in `orgs` and completes the migration; until it completes the gateway does not serve, and `/-/hooks/git-host` answers 503 (so walgit retries) for a location missing from `git_locations` rather than 204. For each org it sends `Group::Adopt { id, slug, parent: root }`, which turns the existing actor (same name `org`, same key) into a group: adds `ALTER TABLE` steps where schemas gain columns (`apply_schema` only creates tables), adds its repositories to `children`, rewrites its outbox source `org:<id>` to `group:<id>` and records `group.ready`. Each `Repo` receives `parent_id`, `path` and its Git location `<org>/<repo>`. The root group's `children` (with each org's pool) and `git_locations` are filled for every adopted repository before the migration is marked complete. Existing `org/<org>` placement rows become `group/<org>`. Rows still pending (`ready = 0`) and repository actors never initialized are completed or dropped by the same rule creation uses: a pending org or repository is finished as if its create were retried. Subscribers' stored per-source delivery rows are rewritten from `org:<id>` to `group:<id>` so cursors continue and no gap is logged; subscriptions matching `org.*` patterns stop matching, which the upgrade notes say. The forge actor drops `orgs` in the same transaction that records completion. Every step is idempotent, so a crash mid-way re-runs it.

For new repositories the `git_locations` entry is written before the parent marks the child ready, so no push can arrive for an unknown location.

*Rollback:* none in place. Operators snapshot the Rivet data before upgrading; this is documented in the upgrade section of Deploying.

### 6. Web routing, sidebar and counts

The web router moves from `#/<org>/<repo>` to `#/<path>`. The UI asks `GET /-/api/tree/<path>` to learn whether a path is a group or a repository, then renders the listing or the repository. Sub-pages use `#/<repo path>/-/issues` so the same separator holds in the browser.

The sidebar (`tree-navigation`) lazily loads one group's children per expansion from `GET /-/api/tree/<path>`. Open-issue counts are eventually consistent and carried as absolute values with a version: an installation's Issues snapshot events (delivered to the repository actor as a recipient, like other installation events) set the repository's `open_issues`; each group keeps `(count, version)` per child and sums its rows, and sends its own total to its parent through the outbox with retries, so a retry or a crash between hops converges instead of double-counting and ordering between concurrent actions does not matter. Migrated repositories are seeded by recounting their Issues installation; the Issues event type is read from the installation's package id, not hard-coded in the kernel. Listings return each child's rolled-up count, so a sidebar costs one call per expanded group. The jump field filters paths from the listings already loaded plus a one-time `GET /-/api/tree?flat=1` returning every path (fine for one person's forge; revisit with server-side search if it grows).

## Risks / Trade-offs

- [Git proxy adds a streaming hop to every clone and push] → stream without buffering; e2e test with a large push; the Git host token is never logged.
- [Migration touches every actor] → idempotent steps, an e2e test that upgrades a populated org-based forge, and a documented snapshot before upgrading.
- [Event sources and types change for subscribers] → breaking and documented in the upgrade notes; history is renamed in place so a subscriber's cursor continues.
- [Participant spam] → per-DID rate limit, no property setting, no tokens; the owner can set `participants` back to `"owners"` at any time, and participation requires signing in with a verified DID.
- [Deep trees cost one actor call per level on every path resolution] → resolution is cached per request; creation depth is not limited, so a pathological tree is slow rather than broken. Revisit with a path index in the root group if it matters.
- [Path permanence] → no rename or move means a wrongly named group stays; moves are a separate change.

## Testing the migration

An org-based forge cannot be built once `Org` and `CreateOrg` are gone, so legacy seeding actions live behind a `legacy-org-seed` test-support feature (never in release builds) and create the old schemas and rows. The migration has a fault-injection point between adopting groups and dropping `orgs`, used to prove crash safety.
