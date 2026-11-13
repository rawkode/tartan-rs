## ADDED Requirements

### Requirement: The forge is a tree of groups and repositories
A forge SHALL have one tree. Its top level SHALL hold groups and repositories; a group SHALL hold groups and repositories; a repository SHALL hold nothing. A node's path SHALL be its ancestors' slugs and its own, joined by `/`, and SHALL identify it. Slugs SHALL be unique among the children of one parent, groups and repositories together. The tree SHALL have no depth limit, and a path SHALL be at most 1024 bytes.

#### Scenario: Nested groups and a top-level repository
- **WHEN** the owner creates the group `acme`, the group `platform` in it, the repository `api` in `acme/platform`, and the repository `notes` at the top level
- **THEN** `acme/platform/api` and `notes` each resolve to a repository and `acme/platform` to a group

#### Scenario: A group and a repository with one name
- **WHEN** `acme` is a group and the owner creates a repository `acme` at the top level
- **THEN** the API answers 409 and nothing is created

#### Scenario: A repository holds nothing
- **WHEN** the owner creates a child in the repository `acme/platform/api`
- **THEN** the API answers 400 and nothing is created

#### Scenario: A path too long
- **WHEN** a create would give a node a path of 1025 bytes
- **THEN** the API answers 400 and nothing is created

### Requirement: Slugs name groups and repositories
A group or repository slug SHALL be 1 to 64 lowercase letters, digits and `-`, and SHALL NOT start or end with `-`. `-` SHALL NOT be a slug, so a path never contains the `/-/` that separates a repository's path from its resources.

#### Scenario: Invalid slug
- **WHEN** a group is created with the slug `Not Valid`
- **THEN** the API answers 400 and nothing is created

### Requirement: The tree API is addressed by path
`GET /-/api/tree` SHALL list the top level and `GET /-/api/tree/<path>` SHALL answer the node at `<path>`: a group with its children, or a repository with its clone URL. A listing SHALL give each child's kind, slug and open-issue count. `POST /-/api/tree` and `POST /-/api/tree/<group path>` SHALL create a group or repository, given `{"kind": "group" | "repo", "slug": …}`, in the top level or that group. A repository's resources SHALL be at `/-/api/tree/<repo path>/-/<resource>`. An unknown path, or a path that continues below a repository outside `/-/`, SHALL be answered 404.

#### Scenario: Listing the top level
- **WHEN** an owner calls `GET /-/api/tree` on a forge with the group `acme` and the repository `notes`
- **THEN** both are listed with their kind and slug

#### Scenario: Reading a group
- **WHEN** a client calls `GET /-/api/tree/acme/platform`
- **THEN** the answer is the group with its child groups and repositories

#### Scenario: Unknown path
- **WHEN** a client asks for `acme/nope`
- **THEN** the API answers 404

#### Scenario: Below a repository
- **WHEN** a client asks for `acme/platform/api/more`
- **THEN** the API answers 404

### Requirement: Creating a group or repository is idempotent
Creating a group, or a repository, under a parent SHALL return the existing node of that kind when its slug is already taken, and SHALL create the repository on the Git host before the forge reports it. A retry after a failure SHALL find the same node.

#### Scenario: Group created twice
- **WHEN** `POST /-/api/tree {"kind": "group", "slug": "acme"}` is sent twice
- **THEN** both answers carry the same group id

#### Scenario: Repository created on the Git host
- **WHEN** `POST /-/api/tree/acme/platform {"kind": "repo", "slug": "api"}` succeeds
- **THEN** the Git host has a repository for it and the answer carries its clone URL, `<public url>/acme/platform/api.git`

### Requirement: A repository is cloned at its path
A repository's clone URL SHALL be `<TARTAN_PUBLIC_URL>/<path>.git`. The gateway SHALL serve Git smart HTTP (`/info/refs`, `git-upload-pack`, `git-receive-pack`) under it by resolving the path and forwarding the request, with its `Authorization` header unchanged, to the repository's location on the Git host, streaming bodies both ways. A path that is not a repository SHALL answer 404.

#### Scenario: Clone a nested repository
- **WHEN** a client runs `git clone <public url>/acme/platform/api.git`
- **THEN** the clone's `HEAD` is the repository's last pushed commit

#### Scenario: Push
- **WHEN** a client pushes to `<public url>/acme/platform/api.git` with the Git host token as a bearer header
- **THEN** the push succeeds and `push.accepted` is recorded in the log of `acme/platform/api`

#### Scenario: Not a repository
- **WHEN** a client clones `<public url>/acme/platform.git`, where `acme/platform` is a group
- **THEN** the gateway answers 404

### Requirement: A repository's Git host location does not depend on its path
A new repository SHALL be created on the Git host as `tartan/<repository id>`, because the Git host names a repository by two segments. A repository migrated from an org SHALL keep its `<org>/<repo>` location. Its clone URL SHALL NOT change when it is migrated. The forge SHALL map ref events and Git reads from a Git host location to the repository.

#### Scenario: Migrated repository
- **WHEN** a forge with the org `acme` and its repository `api` is migrated
- **THEN** `acme/api` is cloned at `<public url>/acme/api.git`, its Git host location is unchanged so remotes to the Git host keep working, and pushes reach its log

### Requirement: Only owners change the tree
Creating groups and repositories SHALL need an owner with `admin` rights. Participants, readers and anonymous callers SHALL NOT create, change or place any node.

#### Scenario: Participant creates a group
- **WHEN** a participant calls `POST /-/api/tree {"kind": "group", "slug": "mine"}`
- **THEN** the API answers 403 and nothing is created

### Requirement: An org-based forge migrates in place
On start with orgs recorded, and before it serves requests, the forge SHALL turn each org into a top-level group with the same slug and id, keep every repository's path, id and Git host location, rewrite `org/<org>` placements to `group/<org>`, and rename the group's event source from `org:<id>` to `group:<id>` with its history and its subscribers' cursors. Subscriptions matching `org.*` event types SHALL stop matching. Repeating the migration SHALL change nothing.

#### Scenario: Upgrade
- **WHEN** a forge with the org `acme` and its repository `api` starts on this version
- **THEN** `acme` is a group, `acme/api` is the same repository with the same clone URL and issues, and no data is lost

#### Scenario: Second start
- **WHEN** the migrated forge restarts
- **THEN** nothing is migrated again
