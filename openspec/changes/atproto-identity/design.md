# Design

## Context

See proposal.md for the motivation. Facts about the current code that shape the approach:

- **Gateway authentication.** `crates/tartan/src/gateway.rs` puts every `/-/api` route behind `require_admin`, which compares a bearer token with `TARTAN_ADMIN_TOKEN` in constant time. `extension_routes.rs` hard-codes `const VIEWER: &str = "admin"` for `tool`, `render` and `act` calls. The WIT `actor` record (`kind: user|agent|ext|system`, `id`, `on-behalf-of`) can already describe a person or an agent acting for one, so the extension ABI does not change.
- **Actors.** Each Rivet actor has its own SQLite database, set up with `CREATE TABLE IF NOT EXISTS` (`apply_schema`). Events are appended with `event_log::append` in the same transaction as the change. Process-wide dependencies live in `Services` (a `OnceLock`).
- **Git host.** walgit hosts Git. Every push is made with one static token (principal `tartan`), so the forge cannot tell who pushed. walgit's ref events carry only `old` and `new`. walgit has no API that returns raw objects and no pre-receive hook, so the forge cannot refuse a push.
- **Deployment configuration.** `crates/tartan-config/src/deployment.rs` parses package `forge` and refuses any key other than `modules` and `sources`.
- **Types.** `tartan.dev/types@v0.1.0` defines `#Person` as a free-form principal id. Issues and Kanban mirror that regex. The web UI keeps an admin token in `localStorage`.
- **Tests.** One Forge and one gateway run per test process. `scripts/e2e.sh` never touches the public network.

## Goals / Non-Goals

**Goals:**
- Trust a DID only on cryptographic evidence: a replayed did:plc log, or a did:web document fetched over TLS. Never trust a PDS's or the directory's word alone.
- Make every verification result reproducible offline from what the forge stored at push time.
- Keep the forge's own secrets to a minimum: no OAuth refresh tokens, and only hashes of session and token secrets.
- Exercise the real code paths in development and tests, with only URLs changed and nothing on the public network.

**Non-Goals:**
- Org and repository membership or per-repository roles. Non-owners are read-only until a later change adds membership.
- Per-person Git push credentials or refusing pushes. walgit has no hook for either. Pushes stay unattributed, and signatures provide the attribution.
- OpenPGP or X.509 signatures (shown as `unsupported_format`), RSA SSH keys, and `mergetag` verification.
- Generic OIDC sign-in, and email addresses or email verification.
- Writing records other than key records into people's repositories.

## Decisions

### D1. Sign-in is login-only ATProto OAuth, with a confidential client and no stored tokens

The forge acts as a backend-for-frontend client.

**Client metadata** is served at `<TARTAN_PUBLIC_URL>/-/auth/client-metadata.json`:

```json
{
  "client_id": "https://forge.example/-/auth/client-metadata.json",
  "client_name": "Tartan (forge.example)",
  "client_uri": "https://forge.example",
  "application_type": "web",
  "redirect_uris": ["https://forge.example/-/auth/callback"],
  "response_types": ["code"],
  "grant_types": ["authorization_code"],
  "scope": "atproto repo:org.tangled.key.sshKey?action=create repo:org.tangled.key.sshKey?action=delete",
  "token_endpoint_auth_method": "private_key_jwt",
  "token_endpoint_auth_signing_alg": "ES256",
  "jwks_uri": "https://forge.example/-/auth/jwks.json",
  "dpop_bound_access_tokens": true
}
```

**Declared scopes.** Every scope token the forge requests appears verbatim in the metadata `scope`, because authorization servers compare tokens literally: the reference implementation (`@atproto/oauth-provider`, `client.ts`, `validateRequest`) splits the declared `scope` on spaces and refuses any requested token not in that list with `invalid_scope`, without interpreting `repo:` permissions. Sign-in requests `atproto`; a key add requests `atproto repo:org.tangled.key.sshKey?action=create`; a key removal requests `atproto repo:org.tangled.key.sshKey?action=delete`.

**Reachability.** The person's authorization server fetches the `client_id` URL and `jwks_uri` itself, over public HTTPS. A forge whose `TARTAN_PUBLIC_URL` is not reachable from the internet cannot offer ATProto sign-in; it is administered only with operator-minted tokens (`tartan token create`).

**`TARTAN_PUBLIC_URL` is the browser-facing origin.** It is the origin that serves the web UI and forwards `/-/` (and so `/-/auth/*`) and `/v2/` to the gateway, not the gateway's listen address. The gateway serves no UI: the web UI is a static site, or the Astro dev server, behind a proxy (`web/astro.config.ts`, `docs/src/content/docs/web.mdx`). The `client_id`, the redirect URI, the `return_to` target and the CSRF `Origin` check (D4) all use this one origin, so production must serve the UI and `/-/` from it. In development it is the Astro origin (`http://127.0.0.1:4321`). Tests and the e2e harness that run no web server use the gateway's origin.

**Validation.** In production, `TARTAN_PUBLIC_URL` must be `https`. The ATProto profile requires an `https` `client_id` without a port; the client_id URL itself has a path. Requiring `TARTAN_PUBLIC_URL` to have no path is Tartan's own choice, so the `__Host-` cookies with `Path=/` work. Under dev-identity it may be `http` with a port, and its host must be `127.0.0.1`, not `localhost`: jacquard-oauth treats an `http://localhost` client_id as a public loopback client, which would skip the confidential `private_key_jwt` path.

**Browser binding.** Every pending request is bound to the browser that started it (RFC 9700 §4.7.1); the `state` alone is not enough, because the forge holds the PKCE verifier server-side. `POST /-/auth/login` and `POST /-/auth/keys` set a pre-auth cookie `__Host-tartan_oauth` (`tartan_oauth` under dev-identity over HTTP): HttpOnly, Secure (not in dev over HTTP), SameSite=Lax, Path=/, Max-Age 600. Its value is 32 random bytes in base64url, and the pending row stores its SHA-256 in `browser_sha256`. SameSite=Lax still sends the cookie on the top-level GET redirect back from the authorization server.

**Flow.** The person enters a handle or DID on the sign-in page, which submits it as a top-level form `POST` (so the browser follows the 303 and stores the cookie). The signing-key settings page submits `POST /-/auth/keys` the same way, as a top-level form `POST`, never with `fetch`: a 303 to a cross-origin authorization endpoint cannot navigate the browser from a script. The forge then:
1. Resolves the DID and its PDS (D9).
2. Fetches `PDS/.well-known/oauth-protected-resource`, then `AS/.well-known/oauth-authorization-server`, and checks `issuer`.
3. Sends a PAR with `login_hint` and the purpose's scope. For a key write, `login_hint` is the DID of the current session, never a handle, so a handle reassigned during the session cannot steer consent to another account (the `sub` check in callback step 6 would refuse it anyway). The authorization server metadata's `authorization_endpoint` must be an `https` URL on the issuer's origin (http only under dev-identity), else the request fails with `server_unreachable` before any redirect (`atproto` for sign-in; see *jacquard integration*). jacquard's `par()` generates the `state`, the PKCE verifier and a fresh DPoP key, and the PAR request already carries the client assertion and a DPoP proof made with that key, handling AS nonces, as ATProto requires DPoP from PAR onward.
4. Only after PAR succeeds, stores the pending request in the identity actor: `state`, the whole serialized `AuthRequestData` `par()` returned (PKCE verifier, DPoP private key and AS nonce, `request_uri`, scopes, token and revocation endpoints, account DID), so `ClientAuthStore::get_auth_req_info` can return it intact; issuer, expected DID, purpose, the validated `return_to` (D15; it is never taken from the callback query), `browser_sha256`, the initiating session id for key writes, 10-minute expiry. A PAR failure therefore stores nothing.
5. Answers 303 to `authorization_endpoint?client_id&request_uri` and sets the pre-auth cookie.

On callback, the forge:
1. Consumes the `state`, so it is single-use, whatever the outcome.
2. Requires the pre-auth cookie, and requires its SHA-256 to equal the row's `browser_sha256`. For a key write it also requires the request's current session to be the session id stored in the row. Otherwise it fails and creates no session.
3. If the callback carries `error` (for example `access_denied`) instead of `code`, it stops there and reports it (see *Failures*).
4. Requires `iss` and checks it against the issuer. The ATProto OAuth profile requires authorization servers to advertise `authorization_response_iss_parameter_supported: true`, so login step 2 treats authorization server metadata without it as a malformed document (`server_unreachable`), and a callback without `iss` fails with `wrong_issuer`.
5. Exchanges the code with DPoP (handling server nonces) and an ES256 client assertion (`iss`=`sub`=client_id, `aud`=issuer, unique `jti`).
6. Requires `sub` to be the expected DID and the granted scope to contain `atproto`, and, for a key write, the requested `repo:` token as well (a grant without it is the key-write outcome `scope_refused`, not a later 403 from `createRecord`). jacquard-oauth does not compare `sub` with the expected DID, so this check is ours. Because every flow starts from a DID the forge resolved itself (D9) and whose PDS named this issuer, comparing `sub` with that stored DID is ATProto's identity-authentication check; jacquard's `exchange_code` still calls `verify_issuer`, which resolves `sub` again through our `OAuthResolver`; that resolver answers it from the evidence cached for this pending request, so a second PLC fetch cannot fail a sign-in that otherwise succeeded.
7. For a sign-in purpose, runs `RefreshIdentity{did}` (D10, *Account status*) synchronously, before any session exists; each of its fetches is bounded by the 10-second timeout (D9, *Fetch safety*). A transport failure or malformed answer keeps the previous status, as D10 says. If the resulting status is inactive (D10), the forge creates no session, records no `person.signed_in`, and answers 303 to `/#/-/sign-in?problem=account_inactive&status=<status>&host=<PDS host>` with `return_to` kept, after steps 8 and 9. The sign-in page renders `status` only from the fixed list of known statuses (otherwise it says "inactive") and `host` as escaped text, so a crafted link cannot put arbitrary words on the forge's page. A stored inactive status that the PDS now reports active is overwritten by this refresh, so that sign-in succeeds. Without this step the callback would set a cookie that `Authenticate` refuses on the next request, and the UI's 401 redirect would loop back to sign-in with no message. Key writes skip this step: their session was already authenticated.
8. Revokes the tokens if the server lists a `revocation_endpoint`, and drops them, whatever step 7 decided.
9. Clears the pre-auth cookie. It is cleared on every callback outcome, success or failure.

The same machinery serves key writes (D7). There the purpose carries `{add_key: {name, public_key}}` or `{remove_key: {rkey, fingerprint}}`, the requested scope is `atproto` plus exactly one of `repo:org.tangled.key.sshKey?action=create` or `repo:org.tangled.key.sshKey?action=delete` (both declared verbatim in the metadata), and the callback makes one `com.atproto.repo.createRecord`/`deleteRecord` call to the PDS with the DPoP-bound token before dropping it. That call carries a DPoP proof with `ath`, tracks the PDS's DPoP nonce separately from the authorization server's, and answers a `use_dpop_nonce` 401 from the PDS with exactly one retry; the fetch-safe client passes the `DPoP-Nonce` and `WWW-Authenticate` headers through. `createRecord` leaves `validate` unset, because PDSes that do not know the `org.tangled` lexicon refuse `validate: true`.

**Removal is collection-aware.** The forge deletes only `org.tangled.key.sshKey` records, the one collection it declares a `repo:` token for. `POST /-/auth/keys` with `{remove: rkey}` looks up that rkey among the signed-in DID's unremoved `org.tangled.key.sshKey` index rows and copies its fingerprint into the purpose; an rkey that is not such a row (a legacy `sh.tangled.publicKey` rkey, another person's, or an unknown one) answers `keys=not_removable` without a pending row or PAR. The forge does not declare a scope for the legacy collection, which it never writes: that would need another declared `repo:` token and consent for a collection outside its purpose. The settings page marks legacy rows "published by another client; remove it there" and offers no remove button for them.

Steps 1 and 2 of the callback (the `state` lookup, the pre-auth cookie and the session check) run against our pending row *before* jacquard's `OAuthClient::callback` is called, because jacquard's `callback_core` deletes the auth request (`ClientAuthStore::delete_auth_req_info`) and exchanges the code straight away. `callback_core` also seeds the DPoP nonce from the authorization server's, so the PDS answers the first `createRecord` with `use_dpop_nonce`; jacquard's `dpop.rs` already retries once on `use_dpop_nonce`, and that single retry is the only one (we add none on top of it).

**Failures.** No sign-in failure answers a raw 500. Every *sign-in* failure answers 303 to the sign-in page with a problem code, `/#/-/sign-in?problem=<code>`, keeping the original `return_to` as a query parameter so "try again" returns the person to where they started, and the page explains it (key writes have their own outcomes; see *Key-write outcomes*):
- `handle_not_found`: the handle or DID does not resolve. No pending request is stored.
- `server_unreachable` (with the host): the PLC directory, the person's PDS or its authorization server could not be reached: a connection, DNS, TLS or timeout failure, or a response that is not a well-formed OAuth or ATProto document. The page says the person's server could not be reached, naming the host, not that the handle was not found. No pending request is stored.
- `client_rejected` (with the host): PAR or the token endpoint answered an OAuth error about the client or its request, such as `invalid_client`, `invalid_client_metadata`, `unauthorized_client`, or `invalid_scope` for sign-in (for a key write, `invalid_scope` is `scope_refused`). The page says the person's server refused this forge's configuration and that the forge's operator should be told, and shows the server's `error` and `error_description` as escaped text (at most 300 characters). The usual causes are a `TARTAN_PUBLIC_URL` the server cannot reach over public HTTPS, a mistyped `TARTAN_PUBLIC_URL`, or a stale JWKS. The pending request, if one was stored, is deleted.
- `denied`: the callback carries `error` (for example `access_denied`). The pending request is consumed, and the page says sign-in was cancelled or refused, showing the server's `error_description` as plain text (escaped, at most 300 characters).
- `expired`: the `state` is unknown or past its expiry. The page says the sign-in took too long and offers to try again.
- `browser_mismatch`, `wrong_issuer`, `wrong_subject`, `missing_scope`: the request fails, and the page offers to try again.
- `account_inactive` (with the status and the PDS host): the callback's status refresh (callback step 7) found the account inactive, for example `deactivated` (a deactivated Bluesky account can still complete OAuth at its PDS, so it can reactivate or migrate) or `takendown`. No session is created. The page says the account at <PDS host> is <status>, and to reactivate it there and sign in again.
- `rate_limited`: the client address hit the gateway's sign-in rate limit or its 20 pending sign-ins (D3, *Housekeeping*). The page says to try again in a few minutes. No pending request is stored.
- `busy`: the forge-wide cap of 10,000 pending sign-ins is reached. The page says the forge is busy and to try again shortly. No pending request is stored.

Because `POST /-/auth/login` is a top-level form POST, these limits answer with the 303 above, never a bare 429 or 503 that the browser would show as an unexplained error page. The 303 also carries `Retry-After`, for non-browser callers.

A panic inside jacquard on hostile input (D2, *panic paths*) becomes `server_unreachable`, not a dropped connection.

**Key-write outcomes.** The purpose decides where an outcome lands. Every outcome of a key write (`POST /-/auth/keys` and its callback) answers 303 to the signing-key settings page, `/#/-/settings?keys=<outcome>`, never to `/#/-/sign-in`, because the person is already signed in. The settings page explains each one:
- `added`: the record was written and the index refreshed.
- `removed`: after `deleteRecord` succeeded, `RefreshKeys` ran and the purpose's fingerprint is no longer in any unremoved index row of that DID, in either collection. The outcome is judged by the refreshed index, not by the `deleteRecord` answer, because the reference PDS answers `deleteRecord` for a record that does not exist as a no-op success (`bluesky-social/atproto`, `packages/pds/src/api/com/atproto/repo/deleteRecord.ts`).
- `still_published`: `deleteRecord` succeeded, but after the refresh the fingerprint is still published by that DID (typically a legacy `sh.tangled.publicKey` record of the same key). The redirect carries the remaining `collection` and `rkey`, and the page says the key still verifies commits until that record is removed with the client that wrote it.
- `refresh_failed`: `deleteRecord` succeeded but the refresh could not list the PDS, so the forge cannot confirm the key is gone. The page names the PDS host, says the key may still verify, and offers the refresh button.
- `denied`: the callback carries `error` (for example `access_denied`). "Key not added: you cancelled at your server", with `error_description` as escaped text (at most 300 characters).
- `scope_refused`: PAR answered `invalid_scope` for a key-write purpose, or the granted scope lacks the requested `repo:` token (callback step 6). The page shows the help for publishing the `org.tangled.key.sshKey` record with another ATProto client. It is *not* `client_rejected`, because sign-in with `atproto` alone still works.
- `write_failed`: after consent, the PDS answered `createRecord`/`deleteRecord` with an error or could not be reached. The page names the PDS host and shows its message as escaped text (at most 300 characters). Nothing is indexed.
- `expired`, `browser_mismatch`, `session_mismatch`: as for sign-in, with an offer to try again from the settings page.
- `not_removable`: the `remove` rkey is not an `org.tangled.key.sshKey` record the forge has indexed for the signed-in DID (see *Removal is collection-aware*).
- `rate_limited` / `busy`: the per-address or forge-wide pending-request cap (D3, *Housekeeping*), with `Retry-After`, as for sign-in.
- `client_rejected` and `server_unreachable` keep their sign-in meanings (an OAuth client error other than `invalid_scope`; a connection failure) but also land on the settings page.

`POST /-/auth/keys` without a valid session (after the idle expiry, or signed out in another tab) answers 303 to `/#/-/sign-in?return_to=/#/-/settings`, never a bare 401, because it is a top-level form POST. It also checks the key before anything else: `publicKey` is at most 4096 bytes (the lexicon's limit) and must parse with `ssh-key` as a supported type, else it answers 303 to `/#/-/settings?keys=invalid_key` without storing a pending row or sending a PAR.

**jacquard integration.** jacquard-oauth's `callback()` always calls `ClientAuthStore::upsert_session`. We implement `ClientAuthStore` over the identity actor for pending requests only; `upsert_session` and `get_session` are transient and in memory, so no token reaches SQLite. We implement jacquard's `HttpClient`, `IdentityResolver` and `OAuthResolver` traits on `tartan-atproto`'s verifying resolver (D9) and fetch-safe client (D9, *Fetch safety*), so `exchange_code` → `verify_issuer` never trusts the PLC directory and every PAR, token, revocation, `createRecord` and `deleteRecord` request goes through the fetch-safe client. If a jacquard entry point cannot take an injected client, we wrap or replace that call rather than let it build its own.

**Per-purpose scope.** The forge does not use `OAuthClient::start_auth`. In jacquard-oauth 0.12.2 it ignores `AuthorizeOptions::scopes` (`client.rs`, `start_auth`), and `request::par` always sends `metadata.client_metadata.scope` (`request.rs`, `par`), which `atproto_client_metadata` fills from the client's full declared scope set (`atproto.rs`). Used directly, it would request every declared token on every sign-in. Instead, for each flow the forge builds the `OAuthMetadata` itself (its fields are public): `server_metadata` from its own resolution (D9 and step 2), and `client_metadata` from `atproto_client_metadata` with `scope` overwritten by the purpose's requested scope, which is `atproto`, `atproto repo:org.tangled.key.sshKey?action=create` or `atproto repo:org.tangled.key.sshKey?action=delete`. It then calls `jacquard_oauth::request::par` directly. The published `client-metadata.json` keeps the full declared scope.

**Why.** Sign-in only has to prove the DID. Without refresh tokens there is:
- no long-lived bearer secret to encrypt;
- no single-use refresh lock;
- no 180-day refresh horizon;
- no coupling between client-key rotation and live sessions. Rotating a key just means listing the new key first.

**Alternatives.**
- *Keep OAuth sessions with refresh*: rejected. It is needed only for ongoing PDS writes, which we do not do.
- *Public client*: rejected. Sessions would be limited to 2 weeks, there would be no client authentication, and its AS-side trust is weaker.
- *Generic OIDC*: rejected per the defaults. It also does not give a DID.
- *The localhost client_id for development*: rejected. It is public-only and optional per AS, so it would not exercise the confidential path. tartan-devnet (D12) exercises it instead.

### D2. Crates (crates.io only, exact pins, licences checked)

| Crate | Version | Licence | Used for |
| --- | --- | --- | --- |
| jacquard-oauth | 0.12.2 | MPL-2.0 | PAR, PKCE, DPoP with nonces, `private_key_jwt` keyset, AS/resource metadata |
| jacquard-identity | 0.12.2 (feature `dns`) | MPL-2.0 | handle resolution (DNS TXT + HTTPS), did:web documents |
| jacquard-repo | 0.12.2 | MPL-2.0 | `commit::proof::verify_proofs` (commit signature, DID, MST path); devnet repo writing |
| jacquard-common | 0.12.2 | MPL-2.0 | shared types for the above |
| atrium-crypto | 0.1.3 | MIT | did:key/multikey decoding; p256/k256 verification that rejects high-S |
| hickory-resolver | 0.26.3 (through jacquard-identity) | MIT OR Apache-2.0 | DNS TXT |
| cid | 0.11.3 | MIT | CIDs for PLC operations |
| serde_ipld_dagcbor | the exact 0.6 release jacquard-repo 0.12.2 resolves to, pinned with `=` in task 1.1 | MIT OR Apache-2.0 | DAG-CBOR for PLC operations |
| ed25519-dalek | 2.x as already resolved through ssh-key's `ed25519` feature, exact pin (dev-dependency of `tartan-atproto` only) | BSD-3-Clause | building `sk-` signature fixtures in tests |
| p256 0.13.2, k256 0.13.4 | — | Apache-2.0 OR MIT | PLC rotation-key signatures (same major as jacquard and ssh-key) |
| ssh-key | 0.6.7 (features `alloc,std,ed25519,p256,p384,p521`; no `rsa`, no `dsa`) | Apache-2.0 OR MIT | SSHSIG parsing and verification, OpenSSH key parsing, fingerprints |
| gix | 0.90.0 (`default-features = false`, with `sha1` and `blocking-http-transport-reqwest-rust-tls`; rev-walk with hidden tips is core API; exact features fixed in task 1.2) | MIT OR Apache-2.0 | fetching pushed objects (under `spawn_blocking`), reading raw commits and tags, `gpgsig` split |
| tokio-tungstenite | 0.30.0 (rustls, feature `rustls-tls-webpki-roots`) | MIT | Jetstream client |
| tower-http | 0.6.11 (already in Cargo.lock; feature `catch-panic` added) | MIT | `CatchPanicLayer` on the gateway |
| ipnet | 2.12.2 (already in Cargo.lock) | MIT OR Apache-2.0 | `TARTAN_TRUSTED_PROXIES` CIDRs and the public-address predicate |
| getrandom | 0.3.4 (already in Cargo.lock) | MIT OR Apache-2.0 | session, token and pre-auth cookie secrets from the OS RNG |

The workspace's reqwest 0.12 matches jacquard's. gix 0.90.0 depends on gix-transport 0.62, which needs reqwest 0.13 (0.13.5 is already in Cargo.lock), so two reqwest majors are accepted. The gix fetch talks only to walgit, which is internal, so it does not go through the fetch-safe identity client. p256 and k256 stay at 0.13 on purpose although 0.14 exists: jacquard 0.12.2 and ssh-key 0.6.7 use 0.13, and a second major would duplicate the curve code and break type sharing. tartan-devnet's `private_key_jwt` and DPoP checks use the jose-jwk/jose-jwa 0.1 crates jacquard-oauth already pulls in, pinned exactly in task 1.1.

**Licences.**
- MPL-2.0 is file-level copyleft and explicitly compatible with (A)GPL (MPL §3.3).
- It appears only in the AGPL forge crates (`tartan-atproto`, `tartan`, `tartan-devnet`). The MIT SDK and extensions never depend on it.
- We use jacquard unmodified. If we ever patch a jacquard file, that file stays MPL.

**Why jacquard.** It is the only family with confidential-client DPoP OAuth, typed granular scopes, configurable resolution and a ready record-proof verifier in one type system.

**Known gaps in jacquard, closed by our code in `tartan-atproto`:**
- Its CAR reader does not re-hash blocks. We read the CAR with `jacquard_repo::car::parse_car_bytes` and re-hash every block before calling `verify_proofs`, so no direct CAR crate is needed.
- With p256 0.13, `Commit::verify` accepts high-S. We verify the repository commit's signature with atrium-crypto first.
- `jacquard-identity` trusts the PLC directory. We verify the PLC log ourselves (D9).
- *Panic paths on untrusted input.* atrium-crypto 0.1.3 `parse_multikey` indexes `decoded[..2]` without a length check, so we check the length first. jacquard-oauth 0.12.2 `callback_core` calls `Scopes::new(scope).expect(…)` on the token response. Identity, proof and OAuth work runs in a spawned task whose `JoinError` becomes an error (`unverifiable` in actors, `server_unreachable` at sign-in), and the gateway has tower-http's `CatchPanicLayer`.

**Rejected or fallback alternatives.**
- *atrium-oauth/-identity/-repo (MIT, most used)*: our fallback if jacquard 0.12 fails our build or audit. It has no proof verifier, so we would walk the MST ourselves on atrium-repo's hash-checking CarStore (about 150 lines).
- *rsky-\**: too little adoption (tens of downloads), and rsky-pds is not on crates.io.
- *atproto-plc 0.2.0*: about 89 downloads and a year without releases. The PLC rules fit on one page and we need control over the evidence we keep, so we write the verifier.
- *sequoia-openpgp*: LGPL, large, and out of scope anyway.

### D3. A forge-singleton `identity` actor

The new actor type `identity` is keyed `[forge_id]` and logs to source `identity`. It keeps identity state out of the Forge actor's tables and serialises session, token and key-index writes. Schema:

```sql
CREATE TABLE IF NOT EXISTS people (
  did TEXT PRIMARY KEY, handle TEXT, handle_checked_at INTEGER,
  pds TEXT, atproto_key TEXT,                -- did:key of #atproto
  status TEXT NOT NULL DEFAULT 'active',     -- 'active', or the verified account status (D10, *Account status*);
                                             -- every value but active, throttled and desynchronized is inactive
  status_checked_at INTEGER,
  known_as TEXT NOT NULL,                    -- JSON set: owner, signed_in, signer, committer
  first_seen_at INTEGER NOT NULL, keys_refreshed_at INTEGER);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,                       -- ULID, shown in the session list
  secret_sha256 BLOB NOT NULL UNIQUE, did TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL, user_agent TEXT);
CREATE TABLE IF NOT EXISTS tokens (
  id TEXT PRIMARY KEY, secret_sha256 BLOB NOT NULL UNIQUE, did TEXT NOT NULL,
  name TEXT NOT NULL, scopes TEXT NOT NULL, repos TEXT,   -- JSON
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  last_used_at INTEGER, revoked_at INTEGER, revoked_by TEXT);
CREATE TABLE IF NOT EXISTS oauth_requests (
  state TEXT PRIMARY KEY, did TEXT NOT NULL, issuer TEXT NOT NULL, pds TEXT NOT NULL,
  auth_request TEXT NOT NULL,                           -- serialized jacquard AuthRequestData (PKCE verifier,
                                                        -- DPoP private key and nonce); private, deleted on use or expiry
  purpose TEXT NOT NULL,                                -- JSON: login | add_key | remove_key, with return_to
  client_ip TEXT NOT NULL,                              -- client address (D4), for the per-address cap
  browser_sha256 BLOB NOT NULL,                         -- SHA-256 of the pre-auth cookie
  session_id TEXT,                                      -- initiating session, key writes only
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS signing_keys (
  did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT NOT NULL,
  fingerprint TEXT NOT NULL, key_type TEXT NOT NULL, supported INTEGER NOT NULL,
  name TEXT, public_key TEXT NOT NULL, record_cid TEXT,
  first_seen_at INTEGER NOT NULL, removed_at INTEGER,
  PRIMARY KEY (did, collection, rkey));
CREATE INDEX IF NOT EXISTS signing_keys_by_fingerprint ON signing_keys (fingerprint);
CREATE TABLE IF NOT EXISTS jetstream (id INTEGER PRIMARY KEY CHECK (id = 1), cursor_us INTEGER);
```

**Housekeeping.**
- Expired `oauth_requests` and sessions are deleted when the actor wakes and by its scheduled `Sweep` (below).
- The DPoP key inside `auth_request` is the only private key stored, and only for at most 10 minutes.
- At most 20 `oauth_requests` are pending per client address, and at most 10,000 in all; beyond either cap, and beyond the gateway's per-address rate limit, `POST /-/auth/login` answers 303 to `/#/-/sign-in?problem=rate_limited` (per address) or `?problem=busy` (global), keeping `return_to` and adding `Retry-After` (D1, *Failures*). `POST /-/auth/keys` answers the same way on the settings page (`?keys=rate_limited` or `?keys=busy`).
- `purpose` holds the public key to add; it is capped at 4096 bytes and parsed before a row is stored (D1, *Key-write outcomes*). The pending row stores the client address (`client_ip`) for the per-address count. The gateway also rate-limits `POST /-/auth/login`, `POST /-/api/people/resolve` and `GET /-/api/people` lookups of DIDs not yet cached, per client address (D4, *Client address*), and the identity actor caps new DIDs discovered from committer emails at 50 per push and 5,000 forge-wide; the `Sweep` drops from `known_as` a DID known only as `committer` that has not verified a signature within 30 days.
- **Scheduled sweep.** The identity actor owns a recurring scheduled action, `Sweep`, registered on every start with Rivet's named recurring schedule `ctx.cron().every(CronEveryOptions { name: "sweep", interval: 15 min, action: "sweep", .. })` (rivetkit 3.0.6 `context.rs`; `ctx.cron().list()` lists entries). Re-registering the same name replaces the existing entry and does not count against the actor's 1000-schedule capacity, so starts never pile up duplicates, and it survives the actor sleeping. It deletes expired rows and runs `RefreshIdentity` then `RefreshKeys` for every DID in `known_as` whose `keys_refreshed_at` or `status_checked_at` is older than 6 hours, in batches, so account status is re-learned even with Jetstream disabled and an inactive DID becomes active again without another event. Rivet persists schedules and wakes the actor for them, so nothing depends on the actor staying awake.
- These rows are bookkeeping and secret-bearing state, and record no events (event-delivery carve-out). So are `sessions.last_used_at`, `people.handle_checked_at`, the Jetstream cursor and housekeeping deletes.

**Event source.** The identity actor owns an `Outbox` (`Outbox::new("identity")`) like the Forge and Repo actors: it has a `Flush` handler, calls `flush_later(SubscriberSource::Forge)` after each append and in `on_start`, and registering a forge subscriber also sends it `Flush`, so a new subscriber gets identity history without waiting for the next identity event. Extension installations never receive identity events, because `Outbox::reaches` requires a repository.

**Actions:**
- `BeginAuth` / `CompleteAuth`
- `Authenticate{secret_sha256, kind}`: returns the principal, or nothing
- `CreateSession`, `EndSession`, `ListSessions`
- `CreateToken`, `RevokeToken`, `ListTokens`
- `KnowDid{did, because}`
- `RefreshKeys{did}`, `RefreshIdentity{did}` (no other payload; D10, *Account status*)
- `LookupFingerprint{fp}`, `KeysOf{did}`, `People{dids}`
- `JetstreamCursor` get and set
- `Sweep` (scheduled, see *Housekeeping*)

**Alternative.** Tables in the Forge actor: rejected. Every authenticated request would contend with org and placement writes, and the event source would be mixed.

### D4. Principals and authorization in the gateway

`require_admin` becomes `authenticate`. It builds a `Principal { did, kind: User | Agent{token_id}, rights, repos }` and puts it into the request extensions.

**Resolution order:**
1. A bearer `tartan_pat_…` token.
2. Otherwise, the session cookie.
3. Otherwise, anonymous.

**Lookups.**
- The gateway hashes the secret and asks the identity actor.
- Positive and negative answers are cached per process for 30 seconds, keyed by hash. A revocation clears the local cache entry at once, and other processes catch up within 30 seconds. The same lag applies to signing out, ending a session, removing an owner and a DID becoming inactive or active again: another gateway process may answer from its cache for up to 30 seconds. `Authenticate` refuses any credential whose DID's `people.status` is not `active`, so the cache TTL bounds the lag; there is no cross-process eviction. identity.mdx states the lag.
- `last_used_at` is written at most once an hour per credential.

**Rights.**

| Caller | Rights |
| --- | --- |
| owner (session) | `read`, `write`, `admin` |
| other person | `read` |
| token | its scopes ∩ the rights its DID has now, limited to `repos` when that is set |

**Route classes:**
- `read`: `GET` routes, plus extension `render` calls.
- `write`: extension `tool` and `act` calls.
- `admin`: `POST /orgs`, `POST /orgs/{org}/repos`, `PUT /placements`, `PUT /subscribers/{name}`, `GET /subscribers/{name}/events` (the forge-wide event stream).

**Repository-limited tokens.** A token with `repos` is allowed only on routes addressed to a listed repository (`/orgs/{o}/repos/{r}/…`), and for reads also on its org's routes (`GET /orgs/{o}`, the org's repository list). Every other route answers 403, including `POST /orgs`, `PUT /placements`, `/subscribers/**` (and so the forge-wide event stream), `/-/api/tokens`, `/-/api/sessions` and the people-key refresh. The exceptions are `GET /-/api/me` and `GET /-/api/people`. `org/*` lists every repository of that org. The check is a single match on the route's `{org, repo}` path parameters, so a new forge-wide route is refused by default.

**Client address.** Rate limits and the per-address pending cap key on a client address derived as follows. The gateway is served with `into_make_service_with_connect_info::<SocketAddr>()`, so every request carries its peer address. `TARTAN_TRUSTED_PROXIES` is a comma-separated list of CIDRs, empty by default. When the peer is not in that list, the client address is the peer address and `X-Forwarded-For` is ignored. When the peer is trusted, the gateway reads every `X-Forwarded-For` header, walks the addresses from right to left, and takes the first one not in the trusted list; if all are trusted (or the header is absent or malformed), it uses the left-most valid one, or the peer. `Forwarded` is not read, so there is exactly one source. IPv6 addresses are bucketed by their /64. In development, `dev.sh` and `demo.sh` set `TARTAN_TRUSTED_PROXIES=127.0.0.1/32` and the Astro proxy sends `X-Forwarded-For` (`xfwd: true`); a production operator lists their reverse proxy's addresses.

**CSRF.** Cookie-authenticated non-GET requests, and the top-level form POSTs `/-/auth/login` and `/-/auth/keys`, must carry `Origin` equal to the origin of `TARTAN_PUBLIC_URL` (the browser-facing origin, D1). A mismatch answers 403 with a plain-text body naming the expected origin, and the gateway logs a warning with the received one, so a misconfigured `TARTAN_PUBLIC_URL` or a proxy that rewrites `Origin` is easy to diagnose. With SameSite=Lax this blocks cross-site form posts; bearer tokens are exempt. The Astro dev proxy rewrites only `Host` (`changeOrigin`), so the browser's `Origin` reaches the gateway unchanged and must equal `http://127.0.0.1:4321` in development.

**Extensions.** `VIEWER` goes away. `CallTool`, `Render` and `Act` take an `Actor` instead of a `String`, and `SlotContext.viewer` is built from it (spec: `extension-runtime`).

### D5. Access tokens

- **Format.** The secret is `tartan_pat_` followed by base64url (no padding) of 32 random bytes from the OS RNG, which is 43 characters. The prefix lets secret scanners recognise it.
- **Storage.** Only `SHA-256(secret)` is stored, together with `id` (a ULID), `did`, `name`, `scopes ⊆ {read, write, admin}`, optional `repos` (`org/repo` | `org/*`), and an expiry of 1–365 days.
- **Who creates them.** Tokens are created only from a session, never from another token. Operators can also run `tartan token create`, which talks to the identity actor through the Rivet client exactly as the gateway does, refuses DIDs that are not owners, and prints the secret once.
- **Revocation.** The token's DID or any owner can revoke it.
- **Events.** `token.created` and `token.revoked` never carry the secret.

**Alternatives.**
- *JWT access tokens*: rejected. They cannot be revoked without a lookup, so they gain nothing here.
- *ATProto service auth* (`com.atproto.server.getServiceAuth` JWTs signed by the PDS): attractive for agents that already hold a PDS session. It is left for a later change because it needs a forge DID and service entry.

### D6. Sessions

- The cookie is `__Host-tartan_session`: Secure, HttpOnly, SameSite=Lax, Path=/.
- With the `dev-identity` feature over HTTP it is `tartan_session`, without Secure.
- The value is 32 random bytes in base64url, and the row stores its SHA-256.
- Sessions expire 30 days after creation (absolute) or after 7 days idle.
- The web client uses `credentials: "same-origin"` and stops sending `Authorization`. Cookies set by the gateway carry no `Domain`, so behind the proxy they belong to the `TARTAN_PUBLIC_URL` origin.
- Signing out deletes the row. `GET /-/api/sessions` lists the person's own sessions (id, created, last used, user agent) and `DELETE /-/api/sessions/{id}` ends one.

### D7. Signing keys: Tangled's lexicon, read and written

**Lexicon.** We reuse `org.tangled.key.sshKey`:
- record key `tid`;
- `name` (≤255);
- `publicKey` (OpenSSH authorized_keys, ≤4096);
- `usages` (known values `auth`, `sign`; absent means both);
- `createdAt`.

We also read the legacy `sh.tangled.publicKey` (`key`, `name`, `createdAt`) as `usages: [auth, sign]`. When the same fingerprint appears in both collections for one DID, it is a single signing key for verification, and the `org.tangled` record is preferred as its proof. The index keeps one row per record, so the settings page lists both records of such a key, and a key stays a signing key while any unremoved row publishes it. The forge writes and deletes only `org.tangled.key.sshKey` records (D1, *Removal is collection-aware*).

**Writing.** Keys written by the forge use `usages: ["sign"]`, so a key added for Tartan signing does not become a Tangled push credential.

**Why.** People who already publish keys for Tangled get verification with no setup, and the record fits SSHSIG exactly.

**Rejected.**
- *A `dev.tartan.signingKey` lexicon*: it would fragment keys across forges for no field we need today. If we later need validity windows, we would add a separate `dev.tartan.*` record that refers to the Tangled record rather than duplicating it.
- *GPG keys (`org.tangled.key.unspecced.gpgKey`)*: unspecced, and PGP is out of scope.

**Index refresh** (`RefreshKeys{did}`):
1. Run `com.atproto.repo.listRecords` on both collections from the DID's PDS, at most 10 pages of 100.
2. Parse each key with `ssh-key`.
3. Upsert rows. A row that is no longer listed gets `removed_at` and a `signing_key.removed` event; a new one gets `signing_key.added`.
4. If `listRecords` fails for either collection (transport failure, or an error such as `RepoDeactivated`, `RepoTakendown` or `RepoSuspended`), the existing rows are kept unchanged and no `signing_key.removed` is recorded; an account error also triggers `RefreshIdentity`, which records the verified status. A deactivated or unreachable repository therefore never looks like a burst of key removals.

`RefreshKeys` called from a Repo actor's `verify` (D8 step 4) is bounded to 40 seconds of wall clock, under Rivet's default 60-second action timeout for HTTP-dispatched actions (rivetkit-core 3.0.6 `config.rs`). If it does not finish, the identity actor continues it in the background and the attempt counts as a transient failure, retried with backoff.

The listing is untrusted. It only nominates candidates, and the proof comes at verification time (D8).

**Triggers:**
- sign-in;
- the person's refresh button;
- a Jetstream commit on either collection, or an identity or account event (through `RefreshIdentity`), for the DID;
- a committer email that is a DID and is unknown;
- the identity actor's scheduled `Sweep` (D3), which refreshes every known DID's keys and account status at least every 6 hours.

**Jetstream (D10)** runs in each gateway process. The 6-hourly sweep does not: it is the identity actor's own scheduled action, so it runs once per forge whether or not a gateway is up.

**Lookups that do not make a DID known.** `GET /-/api/people?did=` and `POST /-/api/people/resolve` resolve handles for display only. They never add a DID to `known_as`, so they never enlarge the sweep or Jetstream's `wantedDids`.

### D8. Verifying a pushed object

**Queueing.**
- When the Repo actor records `ref.updated` for `refs/heads/*` or `refs/tags/*`, it inserts `signature_checks(seq, ref_name, old, new, rule)` in the same transaction. `rule` is a copy of the `signing_rule` row in force at that moment, so a push is judged by the rule from before it even when a configuration applies between the push and the check. Ref deletions (`new` all zeros) and other refs are never queued.
- **Durable retries.** The queue is worked by the Repo actor's scheduled action `verify` (handler: `verify_later`), never by a task that relies on the actor staying awake. Rivet's `wait_until` does not block idle sleep (`rivetkit-core` 3.0.6 `work_registry.rs`: `WaitUntil` has `blocks_idle_sleep: false`) and an idle actor sleeps after 30 s plus a 15 s grace period, aborting that work. So:
  - **One pending schedule.** Each Repo actor has at most one pending `verify` schedule. Its Rivet event id and time are kept in a one-row table, `verify_schedule`. Every caller goes through one helper, `ensure_verify_at(t)`, serialised by an actor-level `verify_schedule_lock` so two callers never both register. The stored row counts as pending only while Rivet still holds its event: `ensure_verify_at` checks `ctx.schedule().get(event_id)` (rivetkit 3.0.6 `Schedule::get`), because Rivet claims and deletes a one-shot `At` schedule as it dispatches it (rivetkit-core 3.0.6 `actor/schedule.rs`, `take_due_schedule_dispatches`), so a row whose event has fired, or is firing, names nothing. When the row is missing or its event is gone, the helper clears the row and registers `ctx.schedule().at(t, "verify", …)`; when the event is pending at or before `t` it does nothing; when it is pending later than `t` it cancels it (`ctx.schedule().cancel(id)`) and registers the earlier one. `at` and `next_at` are both Unix milliseconds, the unit of `Schedule::at(timestamp_ms, …)`. This matters because Rivet caps an actor at 1000 pending schedules (rivetkit-core 3.0.6 `DEFAULT_MAX_SCHEDULES`; `at()` fails with `max_schedules_exceeded` beyond it), and the outbox's delivery retries (`outbox.rs`, `schedule_retry`) draw on the same budget. A push of 1500 tags therefore costs one schedule, not 1500.
  - after recording `ref.updated`, the actor calls `ensure_verify_at(now)`;
  - **Serialised runs.** `verify` holds an actor-level `verifying` mutex for its whole run, as `reconcile` holds `reconciling`, because Rivet runs actions, scheduled fires included, concurrently (rivetkit 3.0.6 `start.rs`, `spawn_action_reply`). A wake that finds the mutex held sets an actor-level `verify_rerun` flag and returns; its own schedule is already gone, so it does not touch `verify_schedule`. The running pass, when it has worked its due rows, clears the flag and re-reads the queue while the flag was set and its budget allows. Two wakes never work the same row, so events and hand-offs are not duplicated (subscribers still see at-least-once redelivery, as for every event).
  - **Bounded passes.** One run works every due row (`next_at <= now`) in `seq` order, at most 50 per run and for at most 5 minutes of wall clock; a run that stops early leaves the remaining due rows for the next run. Rows of one ref may finish out of order (an earlier update can still be retrying when a later one is judged); results are therefore kept per update, see *Per-update results*. Each attempt runs inside the `verify` action, which keeps the actor awake while it runs. A failed attempt writes `attempts`, `next_at` and `last_error` (a short reason such as `PDS pds.example unreachable`; never a secret or a token) and registers nothing itself. The run ends with exactly one `ensure_verify_at` on every exit path, errors and early returns included: the action wraps the pass so that, whatever the pass returned, it releases `verifying` and then calls `ensure_verify_at` at `now` if due rows remain beyond the 50 or the time budget, otherwise at the minimum `next_at` of the rows still queued, or not at all when the queue is empty. Calling it after releasing the mutex closes the last gap: a wake that arrived during the run and set `verify_rerun` too late to be seen has had its schedule consumed, so the validated helper finds no pending event and registers one. Rivet records an error returned by a scheduled action and registers nothing (rivetkit-core 3.0.6 `finish_schedule_dispatch`), which is why the pass's error must not skip this step. If `ensure_verify_at` itself fails (an SQLite error), the action returns that error, and the next push or `on_start` registers the schedule. Rivet puts no action timeout on scheduled fires (in rivetkit-core 3.0.6 `action_timeout` applies only to HTTP-dispatched actions, `registry/http.rs` and `dispatch.rs`, and a scheduled fire runs inside a keep-awake region), so the 5-minute budget is Tartan's own. A run cut short by shutdown leaves its rows queued, and `on_start` schedules them again. Liveness rests on one invariant: while any row is queued, either a run holds `verifying` (and will call `ensure_verify_at` on exit) or a `verify` event that Rivet still holds is pending at or before the earliest `next_at`;
  - the backoff is 5s × 2ⁿ capped at 5 min; the attempt that runs one hour or more after the first records `unverifiable`;
  - `Repo::on_start` calls `ensure_verify_at(min next_at)` if any row is queued, through the same validated helper (a stored id whose event Rivet no longer holds is cleared), so a schedule lost to a crash is recovered on the next start and a surviving one is not duplicated.
- **Per-update results.** Each judged update writes its own row in `update_signing`, keyed `(seq, ref_name)`, with its `new`, its compliance and its violation. `ref_signing` (the ref's current state, read by `signatures?ref=`, badges and the non-compliance banner) carries the `seq` of the update it describes and is upserted only when the new row's `seq` is at least the stored one. A late result for an older update therefore fills in its own `update_signing` row but never overwrites the result of a newer update.
- **The head update.** `config_head` always holds the `new` of the latest `main` update (`RecordRefEvents`, `crates/tartan-forge/src/actors/repo.rs`), so the *head update* is the `refs/heads/main` update with the greatest `seq`. The configuration gate and the hand-off concern only that update.
- **Hand-off to reconciliation.** When `verify_later` resolves the head update's `signature_checks` row (judged, or recorded `unverifiable`), whatever older `main` rows are still queued, it calls `reconcile` once inside the same `verify` action and, only if that returns `Later`, `reconcile_later`. So the final decision for `main` happens inside a scheduled wake. `reconcile_later` itself still runs in `wait_until`, which does not survive sleep; that is existing behaviour, and `Repo::on_start` recovers it by resuming any unapplied head. `Repo::on_start` schedules `verify` before resuming `reconcile_later`. See D11, *Configuration gating*.
- **Pending is visible.** While a row is queued, `GET …/commits/{sha}/signature` for its `new` answers `{status: "pending", attempts, next_at, last_error}`, and so does `GET …/signatures?ref=` while the ref's latest update is queued (an older queued update does not hide the latest result). A 404 is only for an oid that is neither stored nor queued. Objects below `new` in a queued update also answer `pending` once the walk has listed them; before that they answer 404.

**Two sets per update.** Each queued update keeps two sets apart:
- *Objects to judge*: always the commit `new` resolves to (the tip), plus the annotated tag if `new` is one, plus every commit in `old..new` (a walk from `new` hiding only `old`), newest first, up to 1000. The tip is included even when `old..new` is empty, so a rewind of a ref to an ancestor is judged on that ancestor. When `old` is zero (a new ref), nothing is hidden, so the walk covers the ref's history up to the same 1000-commit bound. A walk that stops at the bound sets `truncated`, and the commits beyond it are `not_checked`.
- *Objects to verify*: the objects to judge that have no row in `object_signatures` yet, the tip included. Objects already verified for another ref are not verified again, but they are still judged, with their stored result, against this ref's rule.

**Reading objects.**
- `GitHost` gains `read_new_objects(owner, repo, new, hidden: &[oid], limit, skip: &dyn Fn(oid) -> bool) -> Walk{listed: Vec<(oid, kind)>, raw: Vec<RawObject{oid, kind, bytes}>, truncated}`. `listed` is the set to judge; `raw` holds bytes only for listed oids that `skip` does not exclude (those already in `object_signatures`).
- The walgit implementation fetches `new` over smart HTTP with the Git host token, using gix, into a bare cache repository at `TARTAN_STATE_DIR/git/<repo id>.git`. The token reaches gix as a per-fetch `http.extraHeader` (or a credential helper in memory); it is never written into the cache repository's config.
- It walks from `new` hiding only `old`, newest first, up to 1000 commits. `listed` always contains the tip commit (and the annotated tag `new` names, if any), even when the walk yields nothing because `new` is an ancestor of `old`.
- If walgit no longer serves `new` (the ref was force-pushed again before the check ran and `new` is unreachable from every tip), the fetch fails with a "not our ref" style error. That is not transient: the check records `unverifiable` at once with `last_error` `object <oid> is no longer on the host`, rather than retrying for an hour.
- The cache is a disposable optimisation. It is pruned by least-recently-used repository above 2 GiB and rebuilt on demand.
- `FakeGitHost` stores raw objects per repository.

**Repo actor schema:**

```sql
CREATE TABLE IF NOT EXISTS signature_checks (
  seq INTEGER NOT NULL, ref_name TEXT NOT NULL, old TEXT, new TEXT NOT NULL,
  rule TEXT,                                          -- the signing rule in force at enqueue
  attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL,
  last_error TEXT,                                    -- last failed attempt's reason, no secrets
  PRIMARY KEY (seq, ref_name));
CREATE TABLE IF NOT EXISTS object_signatures (
  oid TEXT PRIMARY KEY, kind TEXT NOT NULL,          -- commit | tag
  status TEXT NOT NULL, did TEXT, fingerprint TEXT,
  collection TEXT, rkey TEXT, record_cid TEXT,
  proof_sha256 TEXT, candidates TEXT, detail TEXT,
  committed_at INTEGER, checked_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS update_signing (
  seq INTEGER NOT NULL, ref_name TEXT NOT NULL, new TEXT NOT NULL,
  compliant INTEGER NOT NULL, problem TEXT,          -- e.g. "<oid> is unsigned"
  checked_at INTEGER NOT NULL, PRIMARY KEY (seq, ref_name));
CREATE TABLE IF NOT EXISTS ref_signing (            -- upserted only when excluded.seq >= ref_signing.seq
  ref_name TEXT PRIMARY KEY, seq INTEGER NOT NULL, head TEXT NOT NULL, compliant INTEGER NOT NULL,
  violation_seq INTEGER, checked_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS verify_schedule (
  id INTEGER PRIMARY KEY CHECK (id = 1), event_id TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS signing_rule (
  id INTEGER PRIMARY KEY CHECK (id = 1), rule TEXT NOT NULL, config_commit TEXT NOT NULL);
```

`signing_rule` is written in the same transaction as `config.applied`. That transaction lives in `installations::reconcile` (`crates/tartan-forge/src/actors/installations.rs`), so `Repo::reconcile` passes the parsed rule into it. A configuration without `signing`, or an empty root (`Config::default()`), deletes the row, so removing the rule takes effect.

**Per object:**
1. **Extract.**
   - For a commit: find the `gpgsig` header (`gpgsig-sha256` too, for SHA-256 repositories) with `gix-object`'s `CommitRefIter::signature`. The signed data is the object without that header.
   - For an annotated tag: split at the last line that is exactly `-----BEGIN SSH SIGNATURE-----`, as git does, which we do by hand because `gix-object` splits only PGP. A tag message that quotes that line is therefore still parsed correctly.
   - A commit that carries both `gpgsig` and `gpgsig-sha256` follows git's rule: the header for the repository's hash algorithm is the signature, and both headers are removed from the signed data. A fixture covers this.
   - No signature means `unsigned`. PGP or X.509 armor means `unsupported_format`. More than one `gpgsig` (or `gpgsig-sha256`) header is `bad_signature`.
   - `read_new_objects` recomputes each object's oid from its bytes with collision-detecting SHA-1 (gix-hash's `sha1-checked`, as Git uses sha1dc) and refuses a mismatch or a detected collision, so a SHAttered-style pair cannot have a signature on one object judged against the other.
   - A tag `new` names is peeled through nested tags; each tag on the way is an object to judge. If the peeled object is not a commit (a tree or blob), the update's tip is `not_checked`, which is never compliant on a covered ref.
2. **Parse.** `SshSig::from_pem`. Reject any namespace other than `git`, and any hash other than sha256 or sha512 (`bad_signature`). An unsupported key algorithm is `unsupported_format`.
3. **Verify.** `PublicKey::verify("git", signed_data, &sig)`. For `sk-*` keys, also require `flags & 0x01`, as ssh-keygen does. Failure is `bad_signature`.
4. **Find candidates.** Run `LookupFingerprint(SHA256 fingerprint)` against unremoved, supported rows.
   - If the committer email (the tagger for a tag) is a DID, call `KnowDid` and `RefreshKeys` if needed, and keep only that DID. If it does not publish the key, the result is `unknown_key`.
   - Otherwise, with several candidates, keep the only one the rule in force allows for this ref, else the result is `ambiguous_key`.
   - No candidates is `unknown_key`.
5. **Prove** (`tartan-atproto::prove_record`), reusing a bundle fetched at most 10 minutes earlier for the same record. The reuse cache for a DID is dropped when `signing_key.removed`, an identity event or an account event arrives for it, so a revoked key stops verifying at once:
   1. Resolve the DID with evidence (D9). An inactive verified status (D10, *Account status*) means `unknown_key`, with detail.
   2. `GET {pds}/xrpc/com.atproto.sync.getRecord?did&collection&rkey`, at most 1 MiB. `RecordNotFound` marks the key removed (event) and gives `unknown_key`. `RepoDeactivated`, `RepoTakendown` and `RepoSuspended` give `unknown_key` with that detail and trigger `RefreshIdentity`, so an account deactivated while Jetstream is off is caught at proof time and not only by the sweep.
   3. Parse the CAR. Recompute every block's sha2-256 multihash and reject on any mismatch. Allow only CIDv1 with dag-cbor (0x71) or raw codecs. Cap the CAR at 2000 blocks.
   4. Decode the root commit. Require `version == 3`, `did == DID`, and a `rev` not more than 5 minutes in the future. Verify `sig` over the DAG-CBOR of the commit without `sig`, using atrium-crypto with the DID's `#atproto` key. This enforces low-S.
   5. Get the record CID from the signed MST: `jacquard_repo::Mst::load(Arc::new(MemoryBlockStore::new_from_blocks(blocks)), commit.data, None).get("collection/rkey")`. Then call `jacquard_repo::commit::proof::verify_proofs(car, [RecordClaim{collection, rkey, cid: Some(cid)}], did, key)` and require the claim in `verified`. Neither call takes depth or fan-out caps; the 2000-block and 1 MiB CAR caps bound the work, since content addressing rules out cycles. The indexed `signing_keys.record_cid` is only a hint: a mismatch refreshes the index.
   6. Require the record block's re-hashed CID to equal that CID. Decode it strictly, check `$type` equals the collection, and require the record's key to equal the signature's key and its usages to include `sign`.
6. **Store.**
   - Write the bundle to the object store at `proofs/sha256/<hex>.json`, beside the bucket's existing `registry/` and `packages/` prefixes. It is content-addressed and immutable; an existing object is not rewritten.
   - Upsert `object_signatures` for the objects verified, judge the update (D11, *Compliance*) using the stored result of every object to judge, insert its `update_signing` row, upsert `ref_signing` under the `seq` guard, append `ref.signatures.checked` (listing every judged object), and if the rule is broken, `ref.signature.violation`.
   - Delete the `signature_checks` row, all in one transaction.

**Proof bundle (`tartan.signature-proof/1`, JSON, bytes base64):**

```json
{
  "format": "tartan.signature-proof/1",
  "did": "did:plc:…",
  "identity": { "method": "plc", "audit_log": "<bytes as fetched>", "operation_cid": "bafyrei…" },
  "atproto_key": "did:key:zQ3s…",
  "record": { "collection": "org.tangled.key.sshKey", "rkey": "3l…", "cid": "bafyrei…" },
  "repo_commit": { "cid": "bafyrei…", "rev": "3l…" },
  "car": "<getRecord response bytes>",
  "fetched_at": "2026-11-09T12:00:00Z"
}
```

For did:web, `identity` is `{ "method": "web", "url": "https://host/.well-known/did.json", "document": "<bytes>" }`. The trust root there is the TLS fetch at `fetched_at`, so did:web evidence is a record of what was fetched, not an offline proof.

The proof download route serves only digests referenced by that repository's `object_signatures`.

**Statuses are final.** The only exception is `unverifiable`, which an owner can re-queue with `POST …/commits/{sha}/signature/recheck`. A re-check re-queues the update rows that judged the object; when one of them is the head update (above) and its configuration was refused because of that result, `config_head.applied` is reset and `reconcile` runs again in the same `verify` action once the row is resolved.

**Annotations are computed on read.** They come from the identity actor (`signing_keys.removed_at`, `people.status`), so history is never rewritten.

**Alternatives for reading objects.**
- *Read walgit's packs from the bucket*: rejected. It depends on walgit's private manifest format.
- *Add `GET /api/object/{sha}?raw` and a range walk to walgit upstream*: the better long-term source. `GitHost::read_new_objects` hides the choice, so swapping later changes one implementation.
- *Shelling out to `git`*: rejected. It adds a runtime tool dependency.

### D9. Resolving DIDs with evidence

**did:plc:**
1. `GET {plc}/{did}/log/audit`, at most 1 MiB and 1000 entries.
2. Consider the non-nullified entries in order. The first must be a genesis operation: `prev` null, type `plc_operation` (or legacy `create`). Check `did == "did:plc:" + base32lower(sha256(dagcbor(signed_op)))[..24]`.
3. For each operation, recompute its CID and require it to equal the entry's `cid`. Require `prev` to equal the previous accepted operation's CID. Verify `sig` (base64url, low-S, p256 or k256) by one of the previous operation's `rotationKeys` (for genesis, its own).
4. The DID document comes from the last accepted operation:
   - `verificationMethods.atproto` gives the `#atproto` key;
   - the first `at://` entry of `alsoKnownAs` gives the handle claim (compared lowercased);
   - `services.atproto_pds` gives the PDS.
5. A `plc_tombstone` makes the DID deactivated.

A legacy `create` genesis (2022-era) is normalised before step 4, as did-method-plc's `normalizeOp` does: its rotation keys are `[recoveryKey, signingKey]`, its `#atproto` key is `signingKey`, its handle is `handle` with `at://` added when absent, and its PDS is `service` with `https://` added when it has no scheme. The DID hash in step 2 is still taken over the original legacy operation's bytes. A captured legacy log is a test fixture.

We accept the directory's choice of fork (nullified entries). That choice is limited to operations signed by legitimate rotation keys within 72 hours, and we keep the whole log in the bundle so an auditor can re-judge it. The production directory is `https://plc.directory`.

**did:web:**
- `GET https://{host}/.well-known/did.json`, at most 64 KiB, with `id == did`.
- Legacy `EcdsaSecp256r1VerificationKey2019` / `EcdsaSecp256k1VerificationKey2019` methods with uncompressed multibase keys are accepted, as ATProto still asks of resolvers. atrium-crypto's `parse_multikey` needs a multicodec prefix, so `tartan-atproto` decodes these forms itself.

**Handles.** jacquard-identity resolves DNS TXT and HTTPS in parallel, prefers DNS, and fails on conflicting TXT records. A handle is accepted only when it matches the document's first `at://` `alsoKnownAs` entry, case-insensitively.

**Caching and display.** Handles are cached in `people`. `GET /-/api/people` answers from this cache only and never waits on the network:
- a stale handle (older than 24 hours) is still answered, and a background re-verification is scheduled (at most one in flight per DID);
- a transient failure (DNS, PDS or directory unreachable, a timeout) keeps the last verified handle;
- only an affirmative mismatch (the handle no longer resolves to the DID, or the document's `alsoKnownAs` names another handle) makes it `handle.invalid`;
- a DID with no verified handle yet is answered as `{handle: null}`, and a lookup is scheduled; the UI shows a shortened DID such as `did:plc:abcd…wxyz` until the handle resolves.

Sign-in uses a handle at most 10 minutes old, and identity events re-verify at once.

**Fetch safety.** All outbound identity HTTP, including every jacquard-identity and jacquard-oauth fetch (PAR, token, revocation) and the PDS `createRecord` and `deleteRecord` calls (D1, *jacquard integration*), uses one reqwest client:
- built with `.no_proxy()`, so `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` never move resolution elsewhere;
- before every request and every redirect hop, the URL host is checked: an IP literal must pass the public-address predicate. This is needed because hyper-util's `HttpConnector` connects to an IP literal without calling the custom resolver (`hyper-util` 0.1.21, `client/legacy/connect/http.rs`);
- a DNS resolver that applies the same predicate to every resolved address;
- the predicate refuses loopback, RFC 1918, link-local (including 169.254.169.254), CGNAT (100.64.0.0/10), ULA, multicast, unspecified (`0.0.0.0`, `::`), broadcast, IPv4-mapped and IPv4-compatible IPv6 (judged by their embedded IPv4 address), NAT64 `64:ff9b::/96`, and the documentation and benchmarking ranges;
- `https` only. Under dev-identity (D12) the override also allows `http` and private addresses, and nothing else changes; unit tests cover both modes;
- a 10-second timeout;
- body caps enforced while streaming.

### D10. Jetstream as a trigger only

Each gateway process runs a task that:
- connects to `TARTAN_JETSTREAM_URL` (default `wss://jetstream2.us-east.bsky.network/subscribe`; empty disables it; under dev-identity there is no default, see D12);
- on reconnect, rewinds the stored cursor by 5 seconds, as Jetstream recommends (refreshes are idempotent);
- connects to the legacy v1 `/subscribe` endpoint with `wantedCollections=org.tangled.key.sshKey&wantedCollections=sh.tangled.publicKey&requireHello=true`, and then sends the filter as an `options_update` message, never with the DIDs in the URL (10,000 DIDs would not fit in a query string);
- **every `options_update` carries the whole filter**: `wantedCollections` `[org.tangled.key.sshKey, sh.tangled.publicKey]` together with the full `wantedDids` list. The first update after connecting does this, and so does every later change to the DID set. Jetstream's v1 `/subscribe` replaces the connection's whole filter atomically on each `options_update` (bluesky-social/jetstream `internal/subscribe/doc.go`; `filter.go` `ParseUpdatePayload` builds a fresh `Filter`, and an empty list matches everything), so an update without `wantedCollections` would clear the collection filter and deliver every commit by every known DID;
- relies on Jetstream delivering identity and account events whatever `wantedCollections` says, filtered only by `wantedDids`;
- resumes from the cursor in the identity actor, stored every 1000 events or 10 seconds;
- caps frames and messages at 1 MiB (tokio-tungstenite `max_frame_size` and `max_message_size`, well below the 64 MiB default) and reconnects on a larger one.

**The consumer filters too.** It never relies on the server's filter alone:
- an event for a DID not in `known_as` is ignored, whatever its kind;
- a commit event becomes `RefreshKeys{did}` only when its collection is `org.tangled.key.sshKey` or `sh.tangled.publicKey`; any other collection (a post, a like, a follow) is ignored;
- identity events become `RefreshIdentity{did}`, which records `person.handle.changed` and updates the `#atproto` key; when the PDS changed (account migration), `RefreshKeys{did}` follows against the new PDS, in that order;
- account events also become `RefreshIdentity{did}`, with no payload: the event's `active` and `status` fields are never read or stored.

This client-side filter is also what keeps the unfiltered fallback below safe: without it, every identity or account event on the network would become `RefreshIdentity`.

**Account status.** `RefreshIdentity{did}` learns status only from authoritative fetches through the fetch-safe client:
1. the DID's evidence (D9): a `plc_tombstone` as the last valid operation of the verified PLC log makes the status `deleted`;
2. otherwise `GET {pds}/xrpc/com.atproto.sync.getRepoStatus?did=` on the PDS named by the verified DID document. `active: true` is `active`; `active: false` stores its `status` verbatim (`takendown`, `suspended`, `deleted`, `deactivated`, `desynchronized`, `throttled`, or a later value), or `inactive` when absent; a `RepoNotFound` error is `deleted`.

**Inactive** means any stored status other than `active`, `throttled` and `desynchronized`. Those two come from sync 1.1 and describe hosting or relay state, not an action by the account holder, a moderator or the host, so they are kept for display (the person's page shows them as a neutral note) but refuse nothing. `takendown`, `suspended`, `deleted`, `deactivated`, `inactive` and any value this design does not know are inactive, so an unknown future status fails closed.

Only that result is written to `people.status` (and `status_checked_at`), recording `person.status.changed` when it differs. A transport failure or malformed answer keeps the previous status, so an unreachable PDS never locks a person out. Triggers are Jetstream identity and account events, sign-in, the proof-time account errors of D8 step 5.2, and the 6-hourly `Sweep` (D3), which also brings an inactive DID back once its PDS reports it active.

Sessions and tokens of a DID whose verified status is inactive are refused, and a sign-in by it is refused with `account_inactive` (D1, callback step 7) (spec: forge-access), within the 30-second cache window of D4. A forged Jetstream account event therefore costs one `getRepoStatus` fetch and changes nothing.

Sending `wantedDids` tells the Jetstream operator which DIDs use this forge. Operators who mind run their own Jetstream or set `TARTAN_JETSTREAM_URL` empty and rely on the sweep; the configuration docs say so.

Several gateway processes cause duplicate refreshes. These are idempotent and cheap. With more than 10,000 known DIDs (Jetstream's limit for `wantedDids`), the task sends `wantedCollections` with an empty `wantedDids`, logs a warning, and relies on its own filter; the 6-hourly sweep covers anything missed.

**Jetstream v2.** Jetstream also serves `/xrpc/network.bsky.jetstream.subscribeEvents` (atproto proposal 0015), which is server-push only, refuses `requireHello` and `options_update`, and takes `dids`, `collections` and `kinds` as query parameters. The forge targets v1 `/subscribe`, because under v2 up to 10,000 DIDs would have to go in the URL. Moving to v2 is a later option.

**Alternatives.**
- *Raw firehose (`subscribeRepos`)*: self-certifying, but there is no server-side filter, so the forge would process the whole network.
- *Tap*: verifies and filters, but adds a Go sidecar.
- Neither is needed, because every trigger is re-verified with a proof anyway.

### D11. The signing rule

The schema is `tartan.dev/types@v0.2.0` `#Signing`:

```cue
#Signing: {
	require: [...{
		refs: [_, ...] & [...=~"^refs/[^/ ]+(/[^/ ]+)*$"]
		signers?: [_, ...] & [...#DID]
	}]
}
```

Repositories use it as `signing: types.#Signing & {…}`. `tartan-config::check` parses `signing` next to `extensions`, refuses unknown keys, and also refuses `**` anywhere but as the final segment. `*` matches within one segment, so `refs/heads/release-*` is valid.

**The rule in force** is the `signing_rule` row, written with `config.applied`. A push is always judged by the rule from before it, so a push cannot lift its own rule.

**Coverage.** Whether a ref is covered is decided by the `rule` copied into its queued `signature_checks` row, not by the current `signing_rule` row. The configuration gate uses the row of the head update (D8, *The head update*: the `main` update with the greatest `seq`, whose `new` is `config_head.sha`).

**Compliance.** A ref is compliant after an update when the update's walk was not truncated and every object to judge (D8, *Two sets per update*) has a stored result that is `verified` and, if the rule lists `signers`, by one of them. Stored results made earlier for other refs count exactly as fresh ones, and their signer DID is checked against this ref's `signers`. The set to judge is never empty, because it always holds the tip: an update is compliant only when its tip, and every commit it adds, is `verified` by an allowed signer. A rewind (the ref moved backwards or was re-pointed at an ancestor, so `old..new` is empty) is therefore judged on the ancestor it lands on. Rewinding `main` to an unsigned commit, such as one from before the rule existed or a configuration commit A that was refused earlier, is a violation, and its configuration is refused with `signing: <oid> is unsigned`, so the rule cannot be lifted by rewinding past it. Rewinding to a head an allowed signer verified is compliant, so reverting after a violation keeps working. An update is never compliant merely because its objects were checked elsewhere. So pushing an unsigned commit to an unprotected branch and then fast-forwarding `main` to it is a violation on `main`, and so is a commit verified as a DID allowed on the other branch but absent from `main`'s `signers`.

Compliance is per update, not per history. If an unsigned configuration commit A is refused and an allowed signer then pushes B on top of it, the update `A..B` judges only B (its tip and the only commit it adds), so `main` becomes compliant and B's configuration (which includes A's change) is applied. That is intended: B's signature covers B's whole tree. The rule means "each update to the ref is signed by an allowed signer at its tip and every commit it adds", not "every commit in history is signed".

**Configuration gating.** When the rule covers `refs/heads/main`:
- While the head update's `signature_checks` row exists and its copied rule covers `main`, `reconcile` returns `Reconciled::Done` without applying and leaves `config_head.applied = 0`, unlike the existing `Done` path, which sets it to 1 (`crates/tartan-forge/src/actors/repo.rs`). So `on_start` recovery and the hand-off still see the head as unhandled. Rows of older `main` updates do not hold it back. It does not return `Later`, so it does not spend `reconcile_later`'s retry budget (1 s doubling to 64 s, about two minutes, `crates/tartan-forge/src/actors/repo.rs`), which is shorter than `verify_later`'s hour.
- The scheduled `verify` action re-triggers it: when it resolves the head update's row (judged or `unverifiable`), it runs `reconcile` in the same action (D8, *Hand-off*). Resolving an older `main` update's row does not. Because each retry is a Rivet schedule, this happens even when no other request reaches the actor. After a restart, `on_start` schedules `verify` first.
- Once the head update's row is resolved, `reconcile` reads that update's own `update_signing` row (matching `seq` and `new`), never the ref's latest `ref_signing` row, and if it is not compliant records `config.refused` with the problem `signing: <oid> is <status>`. Example: update W (`old..W`) waits on a PDS that is down, and update X (`W..X`) by another signer is then pushed and verified. X's configuration is applied or refused on X's update alone, at once, and the badge shows X; when W later resolves, it fills in W's `update_signing` row only.
- While it waits on the head update, `GET /-/api/orgs/{o}/repos/{r}/config/status` answers `{applied: {commit}|null, pending: {commit, waiting_on: {signature_check: <oid>, attempts, last_error}}|null}`, and the repository page shows "checking signatures…" or "retrying: <last_error>".

**Rejected.** *Refusing the push*: walgit has no pre-receive hook and its `policy.json` has no signature rules. An authenticating proxy in front of walgit could do it later, together with per-person push credentials.

### D12. Development and tests: `tartan-devnet`

`crates/tartan-devnet` (AGPL, `publish = false`) is a library and a binary (`tartan-devnet --listen 127.0.0.1:9400 --seed dev`). It serves, from one in-memory process:
- **A PLC directory.** `GET /{did}`, `/{did}/log/audit`, and genuinely signed genesis operations, so D9 runs unchanged.
- **A PDS.**
  - `com.atproto.sync.getRecord` (CARs from a jacquard-repo MST signed with each person's k256 key);
  - `com.atproto.repo.listRecords`, `createRecord` and `deleteRecord` (DPoP-bound tokens it issued);
  - `com.atproto.identity.resolveHandle`;
  - `com.atproto.sync.getRepoStatus`, answering each person's account status, and `RepoDeactivated`/`RepoTakendown` errors from `getRecord` and `listRecords` for an inactive repository;
  - `/.well-known/oauth-protected-resource`.
- **An authorization server.**
  - metadata, PAR, PKCE and DPoP nonces;
  - `private_key_jwt` checked against the forge's JWKS fetched over HTTP;
  - an authorize page with a "Sign in as @alice.test" button, and `auto_approve` for tests;
  - token and revocation endpoints;
  - support for the granular key scope, with requested scope tokens checked literally against the client metadata's declared `scope`, as the reference server does, so an undeclared token is refused with `invalid_scope`;
  - test switches to answer PAR or the token request with an OAuth error (`invalid_client`, `invalid_client_metadata`, `invalid_scope`) for the `client_rejected` and `scope_refused` tests, to grant a narrower scope than requested, and to make the PDS answer `createRecord`/`deleteRecord` with an error, for the `write_failed` test;
  - a record of each PAR body it received, so tests can assert the exact `scope` sent.
- **Jetstream.** A minimal Jetstream-compatible `/subscribe` that emits the devnet's own commits, so the consumer is tested too. It implements v1's filter semantics, including that each `options_update` replaces the whole filter (an empty list matches everything), and a test control makes a person write a record in an unrelated collection such as `app.bsky.feed.post`, so tests catch an update that drops `wantedCollections`.
- **Test controls.** Test-only endpoints under `/devnet/` add or remove key records directly in either collection (including the same key in both `org.tangled.key.sshKey` and `sh.tangled.publicKey`), change handles, tombstone DIDs, set a person's account status at the PDS, and make Jetstream emit an account or identity event whose payload disagrees with the PDS (for example `takendown` while the PDS says active).

People are derived deterministically from `--seed`, so DIDs are stable. The seed `dev` has at least `alice.test` (the owner in `dev.sh` and `demo.sh`) and `bob.test` (not an owner), so a demo can show read-only access and a signature by a signer the rule does not allow next to the owner's. `tartan-devnet did alice.test` prints a DID for `dev.sh` to put in `owners`.

**The forge's dev switches** compile only with feature `dev-identity`, and act only with `TARTAN_DEV_IDENTITY=1`:
- `TARTAN_PLC_URL`;
- `TARTAN_DEV_HANDLE_RESOLVER` (handles resolve through `com.atproto.identity.resolveHandle` there, since `.test` has no DNS);
- plain HTTP;
- private addresses;
- an HTTP `TARTAN_PUBLIC_URL` with a port.

A release build without the feature refuses to start when the variable is set. `scripts/e2e.sh` builds the test binaries with `--features dev-identity`.

**No public network in dev mode.** With `TARTAN_DEV_IDENTITY=1`:
- `TARTAN_PLC_URL` and `TARTAN_DEV_HANDLE_RESOLVER` are required; the forge refuses to start without them, naming them, instead of falling back to `plc.directory` or public DNS.
- `TARTAN_JETSTREAM_URL` has no default. It must be set (to the devnet's `/subscribe`) or set empty to disable Jetstream; leaving it unset refuses to start. So devnet DIDs are never sent to a public Jetstream as `wantedDids`.
- `dev.sh`, `demo.sh` and the test harness set all three to the devnet.

**Tests** sign commits in-process with `ssh-key` (`SshSig::sign`, Ed25519 and an `sk-`-style fixture) and write the objects with `git hash-object -w`. No ssh-keygen is needed.

**PLC verifier tests** use the devnet plus a few audit logs captured from plc.directory and committed as testdata. These are public data, and include a log with a rotation and a nullified operation.

**Alternatives.**
- *`@atproto/dev-env` or the Bluesky PDS container*: real implementations, but they need Node or Docker and Postgres-free PLC images that do not exist officially. They remain useful for a manual interoperability check (task 10.3).
- *Mocks at the trait level only*: they would leave OAuth, CAR and PLC parsing untested end to end.

### D13. Owners and the end of the admin token

`Deployment` gains `owners: Vec<Did>`, validated as in the spec. The binary refuses to start in any of these cases:
- `owners` is empty;
- `TARTAN_CONFIG` is unset;
- `TARTAN_ADMIN_TOKEN` is set.

When the identity actor starts, it calls `KnowDid(owner, "owner")` for each owner, so their keys get indexed. New CLI commands:
- `tartan oauth keygen`: prints a fresh ES256 private JWKS.
- `tartan token create|list|revoke`. `tartan token create --did <owner>` needs neither the owner's PDS nor the network, so it is the operator's way in when an owner's PDS is down.

The forge refuses to start when `TARTAN_OAUTH_KEYS_FILE` is readable by group or others (mode broader than 0600), as ssh does with private keys. At startup it logs each owner DID with its resolved handle (or "unresolved"), so a mistyped DID is caught by sight. That resolution, and the identity actor's `KnowDid`/`RefreshKeys` for owners, run in the background with the 10-second fetch timeout; neither delays nor blocks startup when the PLC directory or a PDS is down.

**Break-glass limit.** `tartan token create --did <owner>` cannot help when that owner's DID is verified inactive (D10, *Account status*), because tokens of inactive DIDs are refused. The operator must then list a different owner. identity.mdx says so. A forged Jetstream event cannot cause this, since status comes only from the PLC log and the PDS.

**The Rivet engine is an admin boundary.** Anyone who can reach the Rivet engine with `RIVET_TOKEN` can call the identity actor's `CreateToken` and `Authenticate` actions directly, which is how `tartan token create` works, so they hold owner rights. deploying.mdx and identity.mdx say the engine endpoint and `RIVET_TOKEN` must stay private and count as admin credentials.

### D14. People in CUE, extensions and the UI

**`tartan.dev/types@v0.2.0`:**
- adds `#DID` (`did:plc:[a-z2-7]{24}`, or `did:web:` host with `%3A` port only for `localhost`). The CUE type accepts the `localhost` port form so development data type-checks, but the forge refuses it in `owners`, signing-rule signers and person values unless dev-identity is on, since ATProto allows it only for local testing;
- sets `#Person.default?: #DID`;
- adds `#Signing`.

`v0.1.0` stays published. Issues and Kanban republish on `v0.2.0` (minor version bumps). Issues validates person values with the same DID regex and refuses anything else with `invalid`.

**Existing values.** Person values and authors stored before the upgrade (`admin`, `usr_dev`) are kept and rendered as plain text, so the history of a prototype forge is not destroyed. The first edit of such a property must set a DID.

**The `person` UI node.** `tartan-ui@1` gains `{"t":"person","did":…}`, built with `ui::person(did)` in the SDK. The host validates the DID. The web UI resolves all person nodes on a page with one `GET /-/api/people?did=…` and renders `@handle` (a shortened DID while the handle is `null`), with the DID in a tooltip and a link to `https://bsky.app/profile/<did>` disabled under dev-identity.

**Agent authors** render as `@handle (agent)`.

### D15. API and UI surface

| Route | Auth | Purpose |
| --- | --- | --- |
| `GET /-/auth/client-metadata.json`, `GET /-/auth/jwks.json` | none | OAuth client documents |
| `POST /-/auth/login` | none, Origin-checked, rate-limited | start sign-in (`handle`, `return_to`); sets the pre-auth cookie |
| `GET /-/auth/callback` | `state` + pre-auth cookie (+ initiating session for key writes) | finish sign-in or key write |
| `POST /-/auth/logout` | session | end session |
| `POST /-/auth/keys` | session (self), Origin-checked, top-level form POST | start key add or remove; sets the pre-auth cookie; every outcome lands on `/#/-/settings?keys=<outcome>` (D1, *Key-write outcomes*) |
| `GET /-/api/me` | any | `{did, handle, owner, kind, rights, dev_identity}` or `{anonymous: true, dev_identity}`; `dev_identity` tells the static UI to offer the devnet shortcut |
| `GET /-/api/people?did=…` | read | handles for ≤100 DIDs |
| `POST /-/api/people/resolve {handle}` | read | handle → DID, for person inputs |
| `GET /-/api/people/{did}/keys`, `POST …/keys/refresh` | read / self or owner | key index |
| `GET/POST /-/api/tokens`, `DELETE /-/api/tokens/{id}` | session / self or owner | access tokens |
| `GET /-/api/sessions`, `DELETE /-/api/sessions/{id}` | session | own sessions |
| `GET /-/api/orgs/{o}/repos/{r}/commits/{sha}/signature` | read | stored result + annotations + `proof_url`, or `pending` |
| `GET /-/api/orgs/{o}/repos/{r}/proofs/{sha256}` | read | proof bundle (only digests this repository references). Like the rest of `/-/api`, anonymous callers get 401; signing.mdx tells auditors to sign in or use a read token |
| `POST …/commits/{sha}/signature/recheck` | owner | re-queue an `unverifiable` result |
| `GET /-/api/orgs/{o}/repos/{r}/signatures?ref=` | read | ref head result and compliance, or `pending` |
| `GET /-/api/orgs/{o}/repos/{r}/config/status` | read | applied commit and any pending one with what it waits on |

**`return_to`.** The web UI routes in the fragment (`createWebHashHistory`), so `return_to` is a path-absolute reference on `TARTAN_PUBLIC_URL` such as `/#/acme/api/issues`. It must start with exactly one `/`, must not start with `//` or `/\` (browsers turn `\` into `/`), must contain no control characters, and must parse as a path-absolute relative reference; otherwise the callback redirects to `/`.

**Web UI changes** (all routes are hash routes):
- `/#/-/sign-in` page (handle field, `problem` messages from D1 *Failures*, `return_to`), following the `/-/settings` convention for forge pages so it never collides with the `/:org` route. Under dev-identity it also offers a "Sign in as @alice.test" shortcut that runs the full OAuth flow.
- An account menu in the header (handle, owner badge, sign out). A 401 redirects to sign-in. Because anonymous callers get 401 on all of `/-/api`, an anonymous visitor is sent straight to sign-in; there is no public read-only view until membership exists, and identity.mdx and web.mdx say so.
- **Settings** loses the admin-token field and gains:
  - *Access tokens*: create with secret shown once, list, revoke.
  - *Signing keys*: list, add or remove through D1 (a top-level form POST), a message for each `keys=<outcome>` from D1 *Key-write outcomes* (`scope_refused` shows the help for publishing with other clients), refresh with the time of the last refresh ("keys last refreshed 3 h ago"), help text for publishing with other clients, and the exact Git setup with the person's DID filled in: `git config gpg.format ssh`, `git config user.signingkey <key>`, `git config commit.gpgsign true`, `git config user.email <DID>`.
  - *Sessions*.
- A `PersonInput` (handle or DID, resolved to a DID, chips show handles) replaces the hard-coded `['admin']` suggestions in `IssueNewView` and `IssueView`. `PersonDisplay` renders the `person` node.
- **Repository overview:** each branch head gets a signature badge ("checking signatures…" or "retrying: <reason>" while pending), a banner names refs that are not compliant, and another says when a `main` configuration waits on a signature check.
- A new **commit signature view** at `/#/:org/:repo/commit/:sha/signature` shows the status, the verified signer separately from the claimed author and committer, the key, proof download and annotations (as neutral notes).
- **Signature labels.** One table drives the badge, the commit view and the docs:

  | Status / annotation | Label | Tone |
  | --- | --- | --- |
  | `verified` | Signed by @handle | green |
  | `verified` + `key_removed_at` | Signed by @handle, note "key removed on <date>" | green, neutral note |
  | `verified` + `did_deactivated_at` | Signed by @handle, note "account deactivated on <date>" | green, neutral note |
  | `unsigned` | Not signed | grey |
  | `unknown_key` | Signed with a key not published by any known person | amber |
  | `unknown_key`, committer DID pinned (from `detail`) | Key not published by @committer | amber |
  | `unknown_key`, signer's DID inactive (from `detail`) | Signer's account is not active | amber |
  | `ambiguous_key` | Key published by several people: @a, @b | amber |
  | `bad_signature` | Invalid signature | red |
  | `unsupported_format` | GPG/X.509 or key type not supported | grey |
  | `not_checked` | Not checked: too many commits in one push | grey |
  | `unverifiable` | Could not verify (owners see "Check again") | amber |
  | `pending` | Checking signatures… / Retrying: <last_error> | neutral |
- `web/src/lib/sandbox.ts`, the fake gateway behind the UI sandbox and tests, moves from `Bearer dev-admin-token` and `admin` persons to sessions, `/-/api/me` and DID persons.

### Event catalogue

| Source | Type | Data |
| --- | --- | --- |
| identity | `person.signed_in` | `did, handle` |
| identity | `person.handle.changed` | `did, old, new` |
| identity | `person.status.changed` | `did, status` |
| identity | `signing_key.added` / `.removed` | `did, fingerprint, collection, rkey, name?` |
| identity | `token.created` / `token.revoked` | `id, did, name, scopes, repos, expires_at` / `id, did, by` |
| repo:<id> | `ref.signatures.checked` | `ref, old, new, commits[{sha, kind, status, did?}], truncated` |
| repo:<id> | `ref.signature.violation` | `ref, old, new, rule, commits[{sha, status}]` (≤100) |

### Configuration (environment)

| Variable | Default | Notes |
| --- | --- | --- |
| `TARTAN_PUBLIC_URL` | required | the browser-facing origin that serves the web UI and proxies `/-/` and `/v2/` to the gateway (not the gateway's address); https without port or path, reachable over public HTTPS by the authorization servers of everyone who signs in (dev: `http://127.0.0.1:<web port>`; gateway-only tests: the gateway's origin) |
| `TARTAN_OAUTH_KEYS_FILE` | required | private ES256 JWKS, mode 0600 or narrower; first key signs |
| `TARTAN_STATE_DIR` | `$XDG_STATE_HOME/tartan`, else `$HOME/.local/state/tartan` | Git object cache (disposable); the forge refuses to start if none of the three is available |
| `TARTAN_JETSTREAM_URL` | `wss://jetstream2.us-east.bsky.network/subscribe`; no default under dev-identity | empty disables; the Jetstream operator learns which DIDs this forge watches |
| `TARTAN_TRUSTED_PROXIES` | empty | comma-separated CIDRs whose `X-Forwarded-For` is trusted for the client address (D4); dev scripts set `127.0.0.1/32` |
| `TARTAN_DEV_IDENTITY` | unset | `dev-identity` builds only |
| `TARTAN_PLC_URL`, `TARTAN_DEV_HANDLE_RESOLVER` | none | `dev-identity` builds only; required when `TARTAN_DEV_IDENTITY=1` |
| `TARTAN_ADMIN_TOKEN` | — | removed; setting it stops the forge |

## Risks / Trade-offs

- [The key-holding signer is not proved] An ATProto record proves that the DID published a public key, not that the DID holds the private key. Someone can publish another person's key and be shown as the signer of that person's commits when those commits name no DID. → A committer email that is a DID pins the signer (spec: `One signer per key`). Copies make results `ambiguous_key`. A rule's `signers` trusts each listed DID's published key set, not possession of the keys: an allowed signer who publishes someone else's public key makes that person's signed commits count as the allowed signer's. Owners choose `signers` accordingly. The docs tell people to set `user.email` to their DID. Proof-of-possession records were rejected because they would break reuse of existing Tangled keys; they can be added later as a `dev.tartan.*` attestation.
- [The PDS holds the #atproto key] The proof shows what the DID's current repository signer published, which is custodial in practice. → This is the strongest guarantee ATProto offers. The bundle keeps the evidence for later judgment.
- [Single-maintainer jacquard, breaking churn] → Exact pins. It is used behind `tartan-atproto` interfaces. Our own pre-pass covers the gaps we found. atrium is the documented fallback (D2).
- [Granular scope support varies by PDS] → Sign-in needs only `atproto`. If the server refuses the key scope, the UI explains how to publish keys elsewhere, and verification does not depend on the forge writing keys. An older authorization server might reject client metadata whose `scope` lists a granular `repo:` scope, which would break sign-in too; task 10.3 checks this, and the sign-in page reports such a refusal as `client_rejected` with the server's message.
- [Non-owners become read-only] Today anyone with the admin token can do everything. → Owners can be many DIDs, and tokens serve automation. Membership is the next change.
- [walgit cannot refuse pushes] A violating commit lands. → Violations are recorded and shown, and config application is gated on `main`. Rejection is left to a later push proxy.
- [Git object cache disk use] → Bounded LRU in `TARTAN_STATE_DIR`, disposable.
- [Jetstream is unauthenticated JSON] → Used only as a trigger, account events included: keys come from proof fetches and account status from the verified PLC log and `getRepoStatus` (D10, *Account status*), and the consumer ignores DIDs it does not know and collections other than the two key collections (D10).
- [A PDS can replay an old repository state] D8 step 5.4 bounds `rev` only from above. A malicious PDS could serve a signed CAR from before a key was deleted. → This is inside the custodial-PDS trust the design already accepts: the PDS holds the `#atproto` key and could sign anything. Deleting a key therefore does not stop a malicious PDS from proving it; rotating the `#atproto` key (a PLC operation) does. signing.mdx says so.
- [Removing a compromised key does not un-verify history] Results are snapshots (D8). → signing.mdx tells people that removing a key leaves past results `verified` with a note, and to rotate keys; see Open Questions.
- [Revocation lag across processes] Up to 30 seconds (D4, *Lookups*).
- [The PLC directory chooses forks] → Signatures are verified on the chosen chain, and the full log is kept in the bundle.
- [Forge not reachable from authorization servers] A private forge, or a wrong `TARTAN_PUBLIC_URL`, makes every sign-in fail at PAR. → `client_rejected` says the forge's configuration was refused and to tell the operator; deploying.mdx and identity.mdx state the public-HTTPS requirement; operator tokens still work.
- [Same key, different signer per ref] Choosing among several candidate signers uses the rule of the ref where the object is first checked, and the stored result is then reused on other refs. A commit can therefore be `verified` as one DID, or be `ambiguous_key`, depending on which ref was pushed first. This fails closed: on a ref whose `signers` exclude the stored DID it is a violation. A committer email that is a DID removes the ambiguity.
- [Committer DIDs grow the watch list] DIDs learned only as committers are capped at 50 per push and 5,000 forge-wide; a `committer`-only DID never verified as a signer is dropped from `known_as` after 30 days, so it leaves the sweep and Jetstream's `wantedDids`.
- [Shared sign-in capacity] One client could exhaust a global pending-request cap. → A per-address cap of 20 sits under the global 10,000, and the client address is derived only from trusted proxies (D4).
- [Breaking types change] → It is published as `v0.2.0`, and `v0.1.0` stays served.

## Migration Plan

1. Deploy the new binary with `owners` (operators find their DID from their handle with `tartan identity resolve <handle>`, or any ATProto tool), `TARTAN_PUBLIC_URL`, and `TARTAN_OAUTH_KEYS_FILE` (from `tartan oauth keygen`). Remove `TARTAN_ADMIN_TOKEN`. The forge refuses to start until all of this is done, so a half-migrated forge never runs open.
2. Replace scripts' admin token with `tartan token create --did <owner> --scopes admin …`.
3. List `tartan.dev/types@v0.2.0` and the new Issues and Kanban versions in `modules`, then bump repositories' dependencies when they want DID persons. Repositories on `v0.1.0` keep working.
4. Existing actor databases gain new tables only (`CREATE TABLE IF NOT EXISTS`). No existing table changes.
5. **Rollback.** Redeploy the previous binary with `TARTAN_ADMIN_TOKEN`. The new tables are ignored, and person values written as `did:plc` or portless `did:web` DIDs stay valid strings under `v0.1.0`'s regex. `did:web:localhost%3A…` values do not (`%` is outside that regex); they occur only in development data.

## Council notes

Round 1 raised eleven blocking objections; all were accepted after checking them against the code, and none was rejected. Two pairs were duplicates and were merged:
- *Browser binding of OAuth requests* (atproto and security lenses): D1 *Browser binding* and the `browser_sha256` and `session_id` columns. For key writes the callback requires the same session id, which also implies the same DID.
- *`TARTAN_PUBLIC_URL` is the browser-facing origin* (architecture and product-ops lenses): D1, D4 and D15. `scripts/demo.sh` today lets Astro take the next free port and reads it from the log; since the forge starts before the web server and needs the origin, demo.sh now chooses the port first and starts Astro on exactly that port (Vite `server.strictPort`), failing clearly if it is taken.

Facts checked: hyper-util 0.1.21 skips the resolver for IP literals (`connect/http.rs`); reqwest 0.12.28 has `ClientBuilder::no_proxy`; `Repo::reconcile_later` gives up after about two minutes; `web/astro.config.ts` proxies `/-/` and `/v2/` with `changeOrigin`; `web/src/router.ts` uses hash history with `/-/settings` before `/:org`.

### Round 2

Round 2 raised six blocking objections; all were accepted after checking them, and none was rejected. Two were duplicates and were merged.
- *Literal scope matching* (atproto): confirmed in `bluesky-social/atproto` `packages/oauth/oauth-provider/src/client/client.ts` (`declaredScopes = this.metadata.scope?.split(' ')`, then `declaredScopes.includes(scope)` or "Scope … is not declared in the client metadata"). The metadata now declares the create and delete tokens separately (D1), and tartan-devnet checks the same way (D12).
- *Signing rule bypass through another ref* (security): D8 *Two sets per update* and D11 *Compliance*. Objects are verified once but judged on every update of a covered ref.
- *Client address behind the proxy* (security and product-ops, merged): D4 *Client address*, `TARTAN_TRUSTED_PROXIES`, and a per-address pending cap under the global one (D3). `crates/tartan/src/main.rs` serves the router without connect-info today, and `web/astro.config.ts` proxies without `xfwd`.
- *Retries do not survive actor sleep* (architecture): confirmed in rivetkit-core 3.0.6 (`WaitUntil` has `blocks_idle_sleep: false`; default sleep timeout 30 s and grace 15 s). D8 *Durable retries* uses Rivet schedules, as `outbox.rs` does, and D3 gives the 6-hourly sweep to the identity actor as a scheduled action.
- *Misleading `server_unreachable`* (product-ops): D1 adds `client_rejected` and *Reachability*.

### Round 3

Round 3 raised six blocking objections; all were checked and accepted, and none was rejected.
- *Jetstream filter replaced on every update* (atproto): D10 sends `wantedCollections` with the full `wantedDids` in every `options_update`, and the consumer filters by known DID and key collection itself. tartan-devnet models replace-on-update (D12). Accepted on the cited jetstream source; the fix is safe whichever way the server behaves.
- *Rewind of a protected ref* (security): confirmed against `Repo::reconcile` (`crates/tartan-forge/src/actors/repo.rs`), which applies whatever head is unapplied. D8 now always judges the tip, and D11 drops the empty-set exemption.
- *Unbounded, concurrent `verify` schedules* (architecture): confirmed in rivetkit-core 3.0.6 (`actor/config.rs` `DEFAULT_MAX_SCHEDULES = 1_000`; `actor/schedule.rs` `at()` calls `ensure_schedule_capacity`; `cancel_schedule` and `list_scheduled_events` exist). D8 keeps one pending schedule per Repo actor, bounded passes and a `verifying` mutex. D3's sweep moves to `cron_every`, which replaces by name.
- *Per-purpose scope through jacquard* (implementation): confirmed in jacquard-oauth 0.12.2 (`client.rs` `start_auth` never reads `options.scopes`; `request.rs` `par()` sends `metadata.client_metadata.scope` and generates the DPoP key). D1 calls `request::par` with per-purpose metadata and stores the pending row after PAR.
- *Key-write outcomes* (product-ops): D1 *Key-write outcomes* routes every key-write result to the settings page with its own codes.
- *Raw 429/503 on a form POST* (product-ops): D1 *Failures* adds `rate_limited` and `busy`.

### Round 4

Round 4 raised three blocking objections; all were checked and accepted, and none was rejected.
- *Key removal reported `removed` while still published* (atproto): D1 *Removal is collection-aware* and *Key-write outcomes* judge `removed` by the refreshed index and add `still_published`, `refresh_failed` and `not_removable`; legacy `sh.tangled.publicKey` rows are marked "remove it there". The no-op `deleteRecord` for a missing record was taken from the council's reading of `packages/pds/src/api/com/atproto/repo/deleteRecord.ts`; the design is safe whichever way a PDS answers, because the outcome comes from the refreshed index.
- *Account status taken from Jetstream payloads* (security): D10 *Account status* learns status only from the verified PLC log and `com.atproto.sync.getRepoStatus` on the verified PDS, and the `Sweep` refreshes it. forge-access gains a requirement with "Takendown signer", "Forged account event" and "Reactivated" scenarios.
- *Out-of-order checks for one ref* (architecture): option (b), per-update results. Confirmed that `RecordRefEvents` always moves `config_head` to the latest `main` `new` (`crates/tartan-forge/src/actors/repo.rs`). D8 adds `update_signing` keyed `(seq, ref_name)` and a `seq` guard on `ref_signing`; the gate and hand-off concern only the head update. Option (a), per-ref FIFO, was not chosen because it would make a newer, verifiable configuration wait up to an hour on an older update's unreachable PDS.

Non-blocking suggestions applied: the identity actor's outbox (D3), no cross-process eviction (D4), `Done` leaving `applied = 0` (D11), rivetkit 3.0.6 high-level names (`ctx.cron().every`, `ctx.cron().list()`, `ctx.schedule().cancel`, `ctx.schedule().list()`, checked in `rivetkit-3.0.6/src/context.rs`), the bounded `RefreshKeys` from `verify` and the 5-minute run budget (D7, D8), the full `AuthRequestData` column and `iss` handling (D1, D3), `verify_issuer` served from cached evidence (D1), `login_hint` as a DID and an `https` `authorization_endpoint` (D1), legacy PLC normalisation prefixes (D9), proof-time account errors and `listRecords` errors keeping rows (D7, D8), collision-detecting SHA-1 and peeled tags (D8), the Jetstream frame cap (D10), the committer-DID caps and the signer-choice note (Risks), the Rivet engine boundary (D13), key-write `rate_limited`/`busy` and the signed-out redirect (D1), the reqwest duplicate (D2), README and runbook docs, a second devnet person, and the `user.email` warning (tasks). The `Origin`-mismatch suggestion is applied as a plain-text 403 body naming the expected origin plus a gateway warning log (D4).

### Round 5

Round 5 raised two blocking objections; both were checked and accepted, and none was rejected.
- *Stale `verify_schedule` row stalls the queue* (architecture): confirmed in rivetkit-core 3.0.6. `take_due_schedule_dispatches` (`actor/schedule.rs`) runs `claim_one_shots_sql`, a `DELETE FROM _rivet_schedule_events` (`internal_storage/queries.rs`), before dispatch, so `Schedule::get` of a fired event returns `None`; `finish_schedule_dispatch` only writes the history row and registers nothing on error. D8 *Durable retries* now validates the row with `ctx.schedule().get`, serialises the helper, sets `verify_rerun` instead of returning silently, no longer clears the row at the start of a run, and calls the final `ensure_verify_at` after releasing the mutex on every exit path. Task 8.2 adds the "Push during a run" test.
- *Inactive account sign-in loop* (product-ops): D1 callback step 7 refreshes status synchronously before a session exists and answers `account_inactive`; sign-in gains "Inactive accounts cannot sign in" with "Deactivated account" and "Stale inactive status" scenarios.

Non-blocking suggestions applied: `iss` always required and `authorization_response_iss_parameter_supported: true` required in AS metadata (D1); `throttled` and `desynchronized` shown but not refusing, unknown statuses failing closed (D10, forge-access); `status` on the sign-in page rendered only from the known list (D1); millisecond units and the absence of a framework timeout on scheduled actions (D8).

## Open Questions

- Which public Jetstream instance to default to. This is a configuration default only.
- Whether walgit upstream accepts a raw-object endpoint, which would let us drop the gix cache. The interface is already isolated.
- Whether a later change should let owners mark a key "revoked as compromised", so results it verified show a warning rather than a neutral note.
- Whether to expose operator diagnostics (Jetstream connection state and cursor lag, queued signature checks) through an owner-only status route rather than logs alone. Until then, identity.mdx and deploying.mdx list the log lines to check.

## Open objections

The review council did not reach unanimity. These blocking objections remain unresolved and must be settled before implementation of the affected parts (D8 step 5 and the commit-signatures spec; D1 problem and outcome redirects and the sign-in and signing-keys specs). They are recorded verbatim.

### atproto lens

**Issue.** D8 step 5 (Prove) does not say what happens when the proof is fetched but does not verify. Three cases are left open: the repository commit's `sig` fails against the resolved `#atproto` key (step 5.4), the signed MST does not contain `collection/rkey` or `verify_proofs` does not return the claim (step 5.5), and the record's key or `usages` do not match (step 5.6). The status list in commit-signatures/spec.md has no proof-failure status, and stored results are final except `unverifiable`. An implementer could therefore record a final `bad_signature` or `unknown_key` for a valid signature. That can happen legitimately right after an `#atproto` signing-key rotation or account migration, when the identity evidence used is stale, for example the 10-minute bundle reuse cache or the PLC directory lagging the PDS's re-signed commit. The ATProto sync spec says to refresh identity on a first signature failure, and to reject only when the signature is conclusively invalid.

**Required change.** Add a short, explicit mapping to D8 step 5 and a requirement or scenario in commit-signatures/spec.md, with matching unit tests in task 8.3:
(a) If step 5.4 or 5.5 fails, drop the bundle reuse cache for that DID, re-resolve the DID evidence without any cache (`RefreshIdentity`), and retry the proof once in the same attempt.
(b) If it still fails, count it as a transient failure with `last_error` such as `proof from <pds> did not verify against <did>'s #atproto key`. It is retried with the normal backoff and becomes `unverifiable` after one hour (re-checkable by an owner), never a final `bad_signature` or `unknown_key`.
(c) A record whose decoded key is not the signature's key, or whose `usages` lack `sign` (step 5.6), is `unknown_key` with a detail, and triggers `RefreshKeys`, since the record was replaced or never authorised signing.
Add a devnet test: rotate a person's `#atproto` key in PLC and serve the old-signed commit from the PDS for one attempt. The check must end `verified` after the PDS re-signs, and must never be finalised as `bad_signature`.

**Evidence.** atproto.com/specs/sync: "services should verify commit signatures ... using the current identity data. If the signature initially fails to verify, the service should refresh the identity metadata in case it had recently changed. Events with conclusively invalid signatures should be rejected." design.md lines 409-415 (step 5) give no outcome for a verification failure. commit-signatures/spec.md line 25 defines `unverifiable` only as "identity or proof could not be fetched after retries", and line 84 says other results never change.

### security lens

**Issue.** Anyone can build a forge link that makes the forge's own sign-in or settings page show their text, including a host to "go to". Failure redirects carry free text and host names in the URL fragment, and the static UI shows whatever it finds there. D1 callback step 7 sends `/#/-/sign-in?problem=account_inactive&status=<status>&host=<PDS host>`. The page then says "the account at <host> is <status>, reactivate it there". So `/#/-/sign-in?problem=account_inactive&status=deactivated&host=bsky-recovery.evil.example` tells a visitor, on the forge's own origin, to go and reactivate their account at a host the attacker picked. The same applies to `server_unreachable` and `client_rejected` (host, plus the server's `error`/`error_description`), `denied` (`error_description`, up to 300 characters), and the key-write outcomes `write_failed`, `refresh_failed` and `still_published` (PDS host, message, collection and rkey). Escaping stops XSS but not the spoofed text. The design recognises this threat for `status` alone ("so a crafted link cannot put arbitrary words on the forge's page"), but `host` and every server message stay attacker-controlled. Getting a real outcome id does not help either: an attacker can run a real flow against their own PDS or authorization server and get back any error_description they like, so whatever carries the details must be bound to the browser that ran the flow.

**Required change.** Problem and outcome redirects must carry only fixed-list codes (`problem`/`keys`, plus a known `status`) and the validated `return_to`. They must never carry a host, a server message, a collection or an rkey. Move those details into a short-lived server-side outcome record in the identity actor that only the browser which ran the flow can read. One way: give the outcome a random id and a 10-minute expiry, bind it to a fresh `__Host-tartan_outcome` cookie hash for sign-in (the pre-auth cookie is cleared at step 9) or to the session id for key writes, and serve it once through `GET /-/auth/outcome` with an Origin check. Without a matching binding, the page shows the generic message for the code. State this in D1 (*Failures*, *Key-write outcomes*, callback step 7) and in the sign-in and signing-keys specs, with a scenario such as "Crafted problem link": opening `/#/-/sign-in?problem=account_inactive&host=evil.example&error_description=…` shows no attacker-supplied host or text. Add matching tests to tasks 6.3, 7.4 and 10.4.

**Evidence.** In design.md: line 78 (step 7 puts `status` and `host` in the redirect, and says status alone is limited to a fixed list for this reason); lines 90-95 (`server_unreachable`/`client_rejected` "with the host", the server's `error_description` shown); line 92 (`denied` shows `error_description`); lines 106-110 (`still_published` carries collection and rkey; `refresh_failed` and `write_failed` name the PDS host and message). The UI is a static hash-routed SPA (D15), so anything it shows from a redirect comes from the URL. The sign-in spec, line 57, requires showing `error_description` as text, with no binding.
