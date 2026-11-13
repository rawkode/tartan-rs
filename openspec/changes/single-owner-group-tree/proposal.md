# Proposal

## Why

Tartan is a forge for one person, but it models a multi-tenant host: every repository lives in exactly one org, and the only place to put structure is the org. A single owner wants to arrange repositories the way they think about them, in groups inside groups to any depth, with some repositories at the top level. The access model also has no place for the people a single-owner forge most wants: visitors who sign in to comment on or open an issue without gaining any control of the forge.

## What Changes

- **BREAKING** Remove orgs. A forge has a tree: the top level holds groups and repositories, a group holds groups and repositories, and a repository is a leaf. A path such as `acme/platform/api` names one node; sibling names are unique across groups and repositories.
- **BREAKING** Replace the org/repo API (`/-/api/orgs/<org>/repos/<repo>/…`) with one path-addressed tree API under `/-/api/tree`. Repository-scoped resources (extensions, signatures, proofs, config status) hang off `<repo path>/-/<resource>`.
- Replace the org actor with a group actor, one per group, owning its children. The forge actor holds no org directory; the root is a group.
- A repository's clone URL is its URL path: `<public url>/<path>.git`. The gateway serves Git smart HTTP there and forwards to the Git host (walgit), which names a repository by two segments, so new repositories are hosted as `tartan/<repository id>` and migrated ones keep `<org>/<repo>`.
- Placements name a `group/<path>` or `repo/<path>`; a node without an entry uses its nearest placed ancestor.
- Events: `org.created` and `org.ready` become `group.created` and `group.ready`; `repo.created` carries `path` and `parent_id` instead of `org_id` and `org_slug`; a group's source is `group:<id>`.
- Access tokens limit repositories by path or path prefix (`acme/platform/*`) instead of `org/repo` and `org/*`.
- Add a **participant** right. A signed-in non-owner the deployment's `participants` setting admits (default none, so upgraded forges stay closed) may read, open issues and comment. Owners alone create, change and place groups and repositories, install extensions by committing configuration, and use write tools.
- Extension tools declare `access: "read" | "participate" | "write"`. Issues gains a `comment` tool and declares `create` and `comment` as `participate`; a participant cannot set properties or close, reopen or update.
- Participant writes are rate-limited per DID.
- The web UI browses the tree with a sidebar (expandable groups, rolled-up open-issue counts), breadcrumbs and a jump field, shows owner-only controls to owners, and shows participants the issue and comment controls only.
- Existing forges migrate in place: each org becomes a top-level group with the same slug, and repositories keep their paths, actor ids and Git host locations.
- `tt`, the demo seed and the Nimbus docs follow the new model.

Out of scope: moving, renaming and deleting groups or repositories (the forge has none today), per-group or per-repository permissions, and access tokens for participants.

## Capabilities

### New Capabilities

- `forge-tree`: groups and repositories as a path-addressed tree: naming, creation, listing, resolution, the `/-/api/tree` API, path-shaped clone URLs and migration of an org-based forge.
- `tree-navigation`: the web sidebar that browses the tree to any depth with rolled-up open-issue counts, breadcrumbs and a jump field.

### Modified Capabilities

- `forge-hierarchy`: the org/repo requirements are replaced by `forge-tree`; what stays is the forge id that separates forges sharing a Rivet namespace.
- `forge-access`: adds the participant right and `participants` setting, the `participate` tool access, participant rate limits and the web UI for them; routes and scenarios use the tree API.
- `issues-extension`: adds the `comment` tool and the participant rules for `create` and `comment`.
- `extension-calls`: calls stay inside the repository, not an org.
- `extension-installations`: installations live on repositories at tree paths; the list route moves.
- `actor-placement`: targets are `group/<path>` and `repo/<path>`, resolved by nearest ancestor.
- `access-tokens`: repository limits are paths and path prefixes; scenarios use the tree API.
- `commit-signatures`: signature and recheck routes move under the tree API; the uniform 404 hides the whole tree.
- `git-hosting`: clones, fetches and pushes go through the gateway at the repository's path.
- `event-delivery`: group events replace org events.
- `tt-sign`: `tt sign recheck --repo` takes a repository path.

## Impact

- Rust: `tartan-forge` (`actors/org.rs` becomes `group.rs`, `actors/forge.rs`, `actors/repo.rs`, `slug.rs`, clients, envelope), `tartan` (gateway, `access.rs`, extension, signature and people routes, settings), `tt`, and the Git host client for nested paths.
- Extensions: Issues manifest and code (`comment`, `participate`), Kanban's use of the repository path, the SDK and `wit/tartan.wit` for the tool access class.
- Web: router, API client, repository context, views, sign-in and read-only notices.
- Data: forge, org and repo actor databases migrate on first start; subscribers see new event types and sources.
- Operators: new `participants` deployment setting; API clients move to `/-/api/tree`.
- Docs (`docs/`), `scripts/dev.sh`, `scripts/demo.sh` and the e2e tests.
- The public origin's front end (Astro dev server, production proxy) must forward `*.git/*` as well as `/-/` to the gateway.
