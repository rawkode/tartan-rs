# Proposal

## Why

Every call to the forge acts as one shared principal, `admin`, authenticated by a static token (`TARTAN_ADMIN_TOKEN`, `dev-admin-token` in development). Nobody can be told apart, person properties hold free-form ids, and a pushed commit carries no evidence of who made it. ATProto gives every person a portable, self-certifying identity (a DID) and a public repository that can hold their keys, so the forge can know who people are and prove who signed a commit without running its own accounts.

## What Changes

- People sign in with their ATProto identity through ATProto OAuth (the forge is a confidential backend-for-frontend client). The forge keeps its own session cookie and no OAuth tokens; the UI shows handles, and DIDs are the identifiers everywhere else.
- Automation and agents act for a person through scoped, expiring, revocable access tokens (`tartan_pat_…`), created by that person in a signed-in session or by an operator with `tartan token create`.
- Forge administrators are the DIDs listed in `owners` in the deployment configuration (package `forge`). **BREAKING**: `TARTAN_ADMIN_TOKEN` and the shared admin principal are removed; the forge refuses to start with no owners. A new `readers` setting says who else may read: by default only owners (`"owners"`), so an upgraded forge stays as private as the admin token kept it and other people who sign in see a private-forge page; `"signed_in"` lets any ATProto account read; a list admits named DIDs. Until org and repository membership exist, readers who are not owners may only read, and the web UI shows them a read-only view (write controls hidden or disabled, a short notice, and an explained message for a refused write).
- A new forge-singleton `identity` actor holds people (DID, verified handle, PDS), sessions, access tokens, pending OAuth requests and the signing-key index, and records its changes as events on a new event source `identity`.
- Commit and tag signatures (SSH signatures, `gpg.format=ssh`) are verified after each push: the key is found in the signer's ATProto repository as an `org.tangled.key.sshKey` record (Tangled's lexicon, so keys work across forges; legacy `sh.tangled.publicKey` is read too), and the record is proved back to the DID with a `com.atproto.sync.getRecord` CAR proof (a signed proof that the record no longer exists makes the key unknown at once) and a verified DID document (the did:plc operation log is replayed, not trusted).
- Each verification result is stored once, at push time, with its proof bundle in the object store, so later key deletion or rotation annotates history instead of rewriting it. A proof that does not verify is retried after refreshing the signer's identity, as the ATProto sync specification asks, and is never recorded as a bad signature. Key, identity and account-status changes arrive from Jetstream as triggers only: keys are re-proved, account status is read from the person's verified PLC log and PDS (never from the event), and sessions and tokens of inactive accounts are refused. An inactive account (deactivated, taken down, suspended or deleted; not merely `throttled` or `desynchronized`) cannot sign in: its status is refreshed during the sign-in callback, and the sign-in page explains the refusal instead of creating a session the forge would then refuse. Sign-in and key-write failure pages show hosts and server messages only to the browser that ran the flow, never from the link, so a crafted link cannot put someone else's words on the forge's pages.
- A repository may require signed commits on chosen refs with a top-level `signing` field in its root package `tartan`. walgit cannot refuse a push, so a violation is recorded as an event, shown in the UI, and stops a configuration on `main` from being applied. Each ref update is judged on its own, so a slow check of an older update neither delays nor overrides a newer one. A result verified on an earlier update counts toward a later update of a covered ref only while the signer still publishes the key and is active, so deleting a compromised key also stops commits it signed from being merged into a protected ref later. A check that gives up after an hour still judges its update (on the tip alone when the Git host never answered), so a `main` configuration is refused rather than left waiting; an owner can re-check an `unverifiable` result, and every update that listed it is judged again by the rule it was pushed under.
- **BREAKING**: `tartan.dev/types` v0.2.0 adds `#DID`; `#Person` values become DIDs. Issues stores authors and person values as DIDs; Kanban labels person lanes with handles. A new `tartan-ui@1` node, `person`, lets extensions show a DID as its handle.
- Local development and tests use `tartan-devnet`, an in-process PLC directory, PDS and OAuth server. The forge accepts it only when built with the `dev-identity` feature and started with `TARTAN_DEV_IDENTITY=1`; nothing touches the public network.

## Capabilities

### New Capabilities
- `atproto-identity`: resolving and verifying DIDs and handles, showing people by handle, outbound-fetch safety, and the development identity network.
- `sign-in`: ATProto OAuth sign-in, the client metadata the forge publishes, refusing inactive accounts at sign-in, forge sessions and signing out.
- `access-tokens`: scoped, expiring, revocable tokens that let automation act on behalf of a DID.
- `forge-access`: who the caller is (session, token or anonymous), forge owners and readers, refusing inactive accounts, what each kind of caller may do, and how the web UI shows private and read-only access.
- `signing-keys`: which ATProto records publish a person's signing keys, how the forge indexes them and keeps the index fresh, and adding or removing keys from the UI (the forge removes only `org.tangled.key.sshKey` records and reports a key still published elsewhere).
- `commit-signatures`: verifying commit and tag signatures after a push, the stored result and proof, its later annotation, and the events and API that expose it.

### Modified Capabilities
- `forge-hierarchy`: the admin-token requirement is removed (replaced by `forge-access`), and the new `identity` actor is one of the singletons keyed by the forge id.
- `forge-cue-registry`: the deployment configuration gains `owners` and `readers`.
- `config-types`: `#DID` is added and `#Person` holds a DID; `#Signing` describes the signing rule.
- `repo-config`: the root package may carry a `signing` rule, which is checked.
- `config-reconciliation`: a configuration whose `main` update breaks the signing rule in force is refused, judged on the update that set `main` to it.
- `git-hosting`: the forge reads the commits a push introduced.
- `event-delivery`: `identity` becomes an event source.
- `extension-runtime`: calls carry the caller's principal (a person's DID, or an agent on behalf of one).
- `issues-extension`: person values and authors are DIDs, shown by handle.
- `kanban-extension`: person lanes and groups are labelled by handle.

## Impact

- **Crates**: `tartan` (gateway auth middleware, auth and token routes, `tartan token` subcommand, Jetstream task), `tartan-forge` (new `identity` actor, Repo actor signature tables, `GitHost` commit reading), `tartan-config` (`owners`, `signing`), new `tartan-atproto` (resolution, PLC log and record proof verification, SSH signature checks) and `tartan-devnet` (development only). All AGPL-3.0-or-later.
- **Dependencies** (crates.io, exact pins): jacquard-oauth, jacquard-identity and jacquard-repo 0.12.2 (MPL-2.0), atrium-crypto 0.1.3 (MIT), ssh-key 0.6.7, gix 0.90.0, hickory-resolver 0.26.3, tokio-tungstenite 0.30.0, p256 0.13.2 and k256 0.13.4 (MIT OR Apache-2.0). Details in design.md.
- **SDK and extensions (MIT)**: `ui::person`, Issues and Kanban person handling; no ATProto dependency.
- **CUE**: `tartan.dev/types` v0.2.0; Issues and Kanban republished against it.
- **Web**: sign-in page, account menu, access-token and signing-key settings, person input and display, signature badges, a private-forge page for people the forge does not let read and a read-only view for readers who are not owners; the admin-token setting is removed.
- **Operations**: new `TARTAN_PUBLIC_URL` (the browser-facing origin that serves the web UI and proxies `/-/` to the gateway), `TARTAN_OAUTH_KEYS_FILE`, `TARTAN_STATE_DIR`, `TARTAN_JETSTREAM_URL`, `TARTAN_TRUSTED_PROXIES` (reverse proxies whose `X-Forwarded-For` gives the client address for rate limits); `TARTAN_ADMIN_TOKEN` removed; `TARTAN_PUBLIC_URL` must be reachable over public HTTPS by people's authorization servers for sign-in to work; `scripts/dev.sh`, `demo.sh` and `e2e.sh` run `tartan-devnet`. The demo assigns its issues to five devnet people by DID, applies a signing rule, and pushes commits signed by an allowed and a disallowed person (it needs OpenSSH `ssh-keygen` and Git 2.34 or later for `git commit -S`).
- **Docs**: new identity, signing and token pages; configuration, running-locally, demo, web, events and property-types pages updated.
