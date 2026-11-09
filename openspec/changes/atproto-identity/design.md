# Design

## Context

See proposal.md for the motivation. Facts about the current code that shape the approach:

- **Gateway authentication.** `crates/tartan/src/gateway.rs` puts every `/-/api` route behind `require_admin`, which compares a bearer token with `TARTAN_ADMIN_TOKEN` in constant time. `extension_routes.rs` hard-codes `const VIEWER: &str = "admin"` for `tool`, `render` and `act` calls. The WIT `actor` record (`kind: user|agent|ext|system`, `id`, `on-behalf-of`) can already describe a person or an agent acting for one, so the extension ABI does not change.
- **Actors.** Each Rivet actor has its own SQLite database, set up with `CREATE TABLE IF NOT EXISTS` (`apply_schema`). Events are appended with `event_log::append` in the same transaction as the change. Process-wide dependencies live in `Services` (a `OnceLock`).
- **Git host.** walgit hosts Git. Every push is made with one static token (principal `tartan`), so the forge cannot tell who pushed. walgit's ref events carry only `old` and `new`. walgit has no API that returns raw objects and no pre-receive hook, so the forge cannot refuse a push.
- **Deployment configuration.** `crates/tartan-config/src/deployment.rs` parses package `forge` and refuses any key other than `modules` and `sources`.
- **The forge is private today.** `require_admin` is a `route_layer` over every `/-/api` route, `GET` included (`crates/tartan/src/gateway.rs`), so nobody without the admin token can read anything. An upgrade must not silently open a forge to every ATProto account.
- **Types.** `tartan.dev/types@v0.1.0` defines `#Person` as a free-form principal id. Issues and Kanban mirror that regex. The web UI keeps an admin token in `localStorage`.
- **Tests.** One Forge and one gateway run per test process. `scripts/e2e.sh` never touches the public network.

## Goals / Non-Goals

**Goals:**
- Trust a DID only on cryptographic evidence: a replayed did:plc log, or a did:web document fetched over TLS. Never trust a PDS's or the directory's word alone.
- Make every verification result reproducible offline from what the forge stored at push time.
- Keep the forge's own secrets to a minimum: no OAuth refresh tokens, and only hashes of session and token secrets.
- Exercise the real code paths in development and tests, with only URLs changed and nothing on the public network.

**Non-Goals:**
- Org and repository membership or per-repository roles. Until a later change adds membership, a non-owner can read only when the deployment configuration's `readers` lets them (D13), and can never write.
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

**Flow.** The person enters a handle or DID on the sign-in page (the forge trims whitespace, strips one leading `@` or an `at://` prefix, and lowercases a handle before resolving it, so a pasted `@Alice.Example ` works), which submits it as a top-level form `POST` (so the browser follows the 303 and stores the cookie). The signing-key settings page submits `POST /-/auth/keys` the same way, as a top-level form `POST`, never with `fetch`: a 303 to a cross-origin authorization endpoint cannot navigate the browser from a script. The forge then:
1. Resolves the DID and its PDS (D9).
2. Fetches `PDS/.well-known/oauth-protected-resource`, then `AS/.well-known/oauth-authorization-server`, and checks `issuer`.
3. Sends a PAR with `login_hint` and the purpose's scope (`atproto` for sign-in; see *jacquard integration*). For a key write, `login_hint` is the DID of the current session, never a handle, so a handle reassigned during the session cannot steer consent to another account (the `sub` check in callback step 6 would refuse it anyway). The authorization server metadata's `authorization_endpoint`, `pushed_authorization_request_endpoint` and `token_endpoint` must be `https` URLs (http only under dev-identity), else the request fails with `server_unreachable` before any redirect. They need not share the issuer's origin, which the ATProto OAuth profile does not require; the PAR and token endpoints are fetched only through the fetch-safe client (D9), whose host checks apply to them, and the authorization endpoint is only a browser redirect. jacquard's `par()` generates the `state`, the PKCE verifier and a fresh DPoP key, and the PAR request already carries the client assertion and a DPoP proof made with that key, handling AS nonces, as ATProto requires DPoP from PAR onward.
4. Only after PAR succeeds, stores the pending request in the identity actor: `state`, the whole serialized `AuthRequestData` `par()` returned (PKCE verifier, DPoP private key and AS nonce, `request_uri`, scopes, token and revocation endpoints, account DID), so `ClientAuthStore::get_auth_req_info` can return it intact; issuer, expected DID, purpose, the validated `return_to` (D15; it is never taken from the callback query), `browser_sha256`, the initiating session id for key writes, 10-minute expiry. A PAR failure therefore stores nothing.
5. Answers 303 to `authorization_endpoint?client_id&request_uri` and sets the pre-auth cookie.

On callback, the forge:
1. Consumes the `state`, so it is single-use, whatever the outcome.
2. Requires the pre-auth cookie, and requires its SHA-256 to equal the row's `browser_sha256`. For a key write it also requires the request's current session to be the session id stored in the row. Otherwise it fails and creates no session.
3. If the callback carries `error` (for example `access_denied`) instead of `code`, it stops there and reports it (see *Failures*).
4. Requires `iss` and checks it against the issuer. The ATProto OAuth profile requires authorization servers to advertise `authorization_response_iss_parameter_supported: true`, so login step 2 treats authorization server metadata without it as a malformed document (`server_unreachable`), and a callback without `iss` fails with `wrong_issuer`.
5. Exchanges the code with DPoP (handling server nonces) and an ES256 client assertion (`iss`=`sub`=client_id, `aud`=issuer, unique `jti`).
6. Requires `sub` to be the expected DID and the granted scope to contain `atproto`, and, for a key write, to grant the requested `repo:` permission as well (a grant without it is the key-write outcome `scope_refused`, not a later 403 from `createRecord`). The granted scope is parsed into permissions with jacquard-oauth's `Scopes` and compared by meaning (collection `org.tangled.key.sshKey` with the requested action), not as a literal string, so a server that normalises the granted string (for example `repo?collection=org.tangled.key.sshKey&action=create`, or a merged `action` list) is not misreported as `scope_refused`. jacquard-oauth does not compare `sub` with the expected DID, so this check is ours. Because every flow starts from a DID the forge resolved itself (D9) and whose PDS named this issuer, comparing `sub` with that stored DID is ATProto's identity-authentication check; jacquard's `exchange_code` still calls `verify_issuer`, which resolves `sub` again through our `OAuthResolver`; that resolver answers it from the evidence cached for this pending request, so a second PLC fetch cannot fail a sign-in that otherwise succeeded.
7. For a sign-in purpose, runs `RefreshIdentity{did}` (D10, *Account status*) synchronously, before any session exists; each of its fetches is bounded by the 10-second timeout (D9, *Fetch safety*), so the step takes at most about 20 seconds, well under rivetkit's 60-second `action_timeout` for the gateway's HTTP-dispatched call to the identity actor (D7). A transport failure or malformed answer keeps the previous status, as D10 says. If the refreshed DID document names a PDS other than the pending row's `pds` (a migration finished within the 10-minute window), the outcome is `wrong_issuer`: the authorization server that answered no longer speaks for the DID. If the resulting status is inactive (D10), the forge creates no session, records no `person.signed_in`, and the outcome is `account_inactive`, redirected to `/#/-/sign-in?problem=account_inactive&status=<status>&outcome=<id>` with `return_to` kept. `status` is put in the URL only when it is one of the statuses D10 names, otherwise `inactive`; the PDS host goes into the outcome record (*Outcome details*), never the URL. A stored inactive status that the PDS now reports active is overwritten by this refresh, so that sign-in succeeds. Without this step the callback would set a cookie that `Authenticate` refuses on the next request, and the UI's 401 redirect would loop back to sign-in with no message. Key writes skip this step: their session was already authenticated. Step 7 only decides the outcome: steps 8 and 9 always run before the response is sent, whatever it decided.
8. Revokes the tokens if the server lists a `revocation_endpoint`, and drops them, whatever step 7 decided.
9. Clears the pre-auth cookie. It is cleared on every callback outcome, success or failure. A sign-in failure whose outcome has details also sets the outcome cookie here (*Outcome details*).

The same machinery serves key writes (D7). There the purpose carries `{add_key: {name, public_key}}` or `{remove_key: {rkey, fingerprint}}`, the requested scope is `atproto` plus exactly one of `repo:org.tangled.key.sshKey?action=create` or `repo:org.tangled.key.sshKey?action=delete` (both declared verbatim in the metadata), and the callback makes one `com.atproto.repo.createRecord`/`deleteRecord` call to the PDS with the DPoP-bound token before dropping it. That call carries a DPoP proof with `ath`, tracks the PDS's DPoP nonce separately from the authorization server's, and answers a `use_dpop_nonce` 401 from the PDS with exactly one retry; the fetch-safe client passes the `DPoP-Nonce` and `WWW-Authenticate` headers through. `createRecord` leaves `validate` unset, because PDSes that do not know the `org.tangled` lexicon refuse `validate: true`.

**Removal is collection-aware.** The forge deletes only `org.tangled.key.sshKey` records, the one collection it declares a `repo:` token for. `POST /-/auth/keys` with `{remove: rkey}` looks up that rkey among the signed-in DID's unremoved `org.tangled.key.sshKey` index rows and copies its fingerprint into the purpose; an rkey that is not such a row (a legacy `sh.tangled.publicKey` rkey, another person's, or an unknown one) answers `keys=not_removable` without a pending row or PAR. The forge does not declare a scope for the legacy collection, which it never writes: that would need another declared `repo:` token and consent for a collection outside its purpose. The settings page marks legacy rows "published by another client; remove it there" and offers no remove button for them.

Steps 1 and 2 of the callback (the `state` lookup, the pre-auth cookie and the session check) run against our pending row *before* jacquard's `OAuthClient::callback` is called, because jacquard's `callback_core` deletes the auth request (`ClientAuthStore::delete_auth_req_info`) and exchanges the code straight away. `callback_core` also seeds the DPoP nonce from the authorization server's, so the PDS answers the first `createRecord` with `use_dpop_nonce`; jacquard's `dpop.rs` already retries once on `use_dpop_nonce`, and that single retry is the only one (we add none on top of it).

**Failures.** No sign-in failure answers a raw 500. Every *sign-in* failure answers 303 to the sign-in page with a problem code, `/#/-/sign-in?problem=<code>`, keeping the original `return_to` as a query parameter so "try again" returns the person to where they started, and the page explains it (key writes have their own outcomes; see *Key-write outcomes*). The redirect carries only fixed-list values, the validated `return_to` and, for an outcome with details, an `outcome` id; hosts and server messages are kept server-side and shown only to the browser that ran the flow (*Outcome details*). Every query value in the fragment is percent-encoded, so `return_to=/#/acme/api/issues?x=1` travels as `return_to=%2F%23%2Facme%2Fapi%2Fissues%3Fx%3D1`:
- `handle_not_found`: the handle or DID does not resolve. No pending request is stored.
- `server_unreachable` (details: the host): the PLC directory, the person's PDS or its authorization server could not be reached: a connection, DNS, TLS or timeout failure, or a response that is not a well-formed OAuth or ATProto document. The page says the person's server could not be reached, naming the host when the outcome record is readable (otherwise "your server"), not that the handle was not found. No pending request is stored.
- `client_rejected` (details: the host, and the server's `error` and `error_description`): PAR or the token endpoint answered an OAuth error about the client or its request, such as `invalid_client`, `invalid_client_metadata`, `unauthorized_client`, or `invalid_scope` for sign-in (for a key write, `invalid_scope` is `scope_refused`). The page says the person's server refused this forge's configuration and that the forge's operator should be told, and shows the server's `error` and `error_description` from the outcome record as escaped text (at most 300 characters). The usual causes are a `TARTAN_PUBLIC_URL` the server cannot reach over public HTTPS, a mistyped `TARTAN_PUBLIC_URL`, or a stale JWKS. The pending request, if one was stored, is deleted.
- `denied` (details: the server's `error_description`): the callback carries `error` (for example `access_denied`). The pending request is consumed, and the page says sign-in was cancelled or refused, showing the `error_description` from the outcome record as plain text (escaped, at most 300 characters).
- `expired`: the `state` is unknown or past its expiry. The page says the sign-in took too long and offers to try again.
- `browser_mismatch`, `wrong_issuer`, `wrong_subject`, `missing_scope`: the request fails, and the page offers to try again.
- `account_inactive` (a known `status` in the URL; details: the PDS host): the callback's status refresh (callback step 7) found the account inactive, for example `deactivated` (a deactivated Bluesky account can still complete OAuth at its PDS, so it can reactivate or migrate) or `takendown`. No session is created. The page's words depend on the status, because only a deactivation is the account holder's to undo: `deactivated` says to reactivate the account at its PDS and sign in again; `takendown` and `suspended` say to contact the hosting provider; `deleted` says the account no longer exists; any other value says the account is not active at its PDS. The PDS host is named only from the outcome record.
- `rate_limited`: the client address hit the gateway's sign-in rate limit or its 20 pending sign-ins (D3, *Housekeeping*). The page says to try again in a few minutes. No pending request is stored.
- `busy`: the forge-wide cap of 10,000 pending sign-ins is reached. The page says the forge is busy and to try again shortly. No pending request is stored.

Because `POST /-/auth/login` is a top-level form POST, these limits answer with the 303 above, never a bare 429 or 503 that the browser would show as an unexplained error page. The 303 also carries `Retry-After`, for non-browser callers.

A panic inside jacquard on hostile input (D2, *panic paths*) becomes `server_unreachable`, not a dropped connection.

**Outcome details.** The web UI is a static, hash-routed page (D15), so anything it reads from a redirect URL may have been written by whoever made the link. Escaping stops script injection, not spoofed text such as "reactivate your account at bsky-recovery.evil.example". Problem and outcome redirects therefore carry only fixed-list values: the code (`problem` or `keys`), a `status` from D10's list for `account_inactive`, an `outcome` id, and the validated `return_to`. They never carry a host, a server's `error` or `error_description`, a PDS message, a collection or an rkey. Those details go into a row of the identity actor's `auth_outcomes` table (D3), which only the browser that ran the flow can read:
- The id is 16 random bytes in base64url. The row holds the code, the details as JSON (each text at most 300 characters, a host at most 253), the client address and a 10-minute expiry.
- A sign-in outcome is bound to a fresh outcome cookie, `__Host-tartan_outcome` (`tartan_outcome` under dev-identity over HTTP): HttpOnly, Secure (not in dev over HTTP), SameSite=Lax, Path=/, Max-Age 600, holding 32 random bytes whose SHA-256 the row stores. It is set on the same 303 that carries the code, from `POST /-/auth/login` or from the callback. The pre-auth cookie cannot serve, because callback step 9 clears it.
- A key-write outcome is bound to the session that started the write (callback step 2 already required it), so it needs no cookie.
- The page reads the details with `POST /-/auth/outcome {id, code}`, which passes the `Origin` check of D4. It answers them only when the row exists, has not expired, has that code, and its binding matches (the outcome cookie's hash, or the current session). It deletes the row on that read and clears the outcome cookie. Otherwise it answers 404, and the page shows the generic message for the code, with no host or server text.
- It is a `POST` because the read consumes the row, and because browsers send `Origin` on a same-origin `fetch` only for methods other than `GET` and `HEAD` (Fetch standard, *append a request `Origin` header*), so an `Origin` check on a `GET` would refuse the forge's own page.
- Binding matters even for genuine ids: an attacker can run a real flow against their own PDS or authorization server and get back any `error_description` they choose, but the row that holds it answers only the attacker's own cookie or session.
- Rows are capped at 20 per client address and 10,000 in all; beyond a cap the oldest row is dropped, which only loses its details. They are deleted on read, on expiry and by the `Sweep`, are secret-free bookkeeping, and record no events.

So a crafted link such as `/#/-/sign-in?problem=account_inactive&status=deactivated&host=evil.example&error_description=…` shows the fixed text for a deactivated account and nothing from `host` or `error_description`.

**Key-write outcomes.** The purpose decides where an outcome lands. Every outcome of a key write (`POST /-/auth/keys` and its callback) answers 303 to the signing-key settings page, `/#/-/settings?keys=<outcome>`, never to `/#/-/sign-in`, because the person is already signed in. As for sign-in, the redirect carries only the code and, when the outcome has details, an `outcome` id; the details are bound to the initiating session and read through `POST /-/auth/outcome` (*Outcome details*). The settings page explains each one:
- `added`: the record was written and the index refreshed.
- `removed`: after `deleteRecord` succeeded, `RefreshKeys` ran and the purpose's fingerprint is no longer in any unremoved index row of that DID, in either collection. The outcome is judged by the refreshed index, not by the `deleteRecord` answer, because the reference PDS answers `deleteRecord` for a record that does not exist as a no-op success (`bluesky-social/atproto`, `packages/pds/src/api/com/atproto/repo/deleteRecord.ts`).
- `still_published`: `deleteRecord` succeeded, but after the refresh the fingerprint is still published by that DID (typically a legacy `sh.tangled.publicKey` record of the same key). The outcome record holds the remaining `collection` and `rkey`, and the page names that record when it can read them, saying the key still verifies commits until the record is removed with the client that wrote it.
- `refresh_failed`: `deleteRecord` succeeded but the refresh could not list the PDS, so the forge cannot confirm the key is gone. The page names the PDS host from the outcome record, says the key may still verify, and offers the refresh button.
- `denied`: the callback carries `error` (for example `access_denied`). "Key not added: you cancelled at your server", with the `error_description` from the outcome record as escaped text (at most 300 characters).
- `scope_refused`: PAR answered `invalid_scope` for a key-write purpose, or the granted scope lacks the requested `repo:` token (callback step 6). The page shows the help for publishing the `org.tangled.key.sshKey` record with another ATProto client. It is *not* `client_rejected`, because sign-in with `atproto` alone still works.
- `write_failed`: after consent, the PDS answered `createRecord`/`deleteRecord` with an error or could not be reached. The page names the PDS host and shows its message, both from the outcome record, as escaped text (at most 300 characters). Nothing is indexed.
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

The new actor type `identity` is keyed `[forge_id]` and logs to source `identity`. Like the Forge actor it always runs in the default pool: placements name only org and repository targets (spec: actor-placement), so it is never moved, and `Services` (owners, the object store, the fetch-safe client) is available wherever it runs. It keeps identity state out of the Forge actor's tables and serialises session, token and key-index writes. Schema:

```sql
CREATE TABLE IF NOT EXISTS people (
  did TEXT PRIMARY KEY, handle TEXT, handle_checked_at INTEGER,
  pds TEXT, atproto_key TEXT,                -- did:key of #atproto
  status TEXT NOT NULL DEFAULT 'active',     -- 'active', or the verified account status (D10, *Account status*);
                                             -- every value but active, throttled and desynchronized is inactive
  status_checked_at INTEGER,
  known_as TEXT NOT NULL,                    -- JSON set: owner, signed_in (readers only, D13), signer, committer
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
CREATE TABLE IF NOT EXISTS auth_outcomes (                -- failure details for the browser that ran the flow (D1)
  id TEXT PRIMARY KEY,                                  -- 16 random bytes, base64url; the redirect's `outcome`
  code TEXT NOT NULL, details TEXT NOT NULL,            -- JSON: host, error, error_description, message, collection, rkey
  browser_sha256 BLOB,                                  -- SHA-256 of the outcome cookie (sign-in)
  session_id TEXT,                                      -- initiating session (key writes)
  client_ip TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS signing_keys (                -- signing claims only: records whose usages
  did TEXT NOT NULL, collection TEXT NOT NULL, rkey TEXT NOT NULL,   -- include sign or are absent (D7)
  fingerprint TEXT NOT NULL, key_type TEXT NOT NULL, supported INTEGER NOT NULL,
  name TEXT, public_key TEXT NOT NULL, record_cid TEXT,
  first_seen_at INTEGER NOT NULL, removed_at INTEGER,
  PRIMARY KEY (did, collection, rkey, fingerprint));
CREATE INDEX IF NOT EXISTS signing_keys_by_fingerprint ON signing_keys (fingerprint);
CREATE TABLE IF NOT EXISTS jetstream (id INTEGER PRIMARY KEY CHECK (id = 1), cursor_us INTEGER);
```

**Housekeeping.**
- Expired `oauth_requests`, `auth_outcomes` and sessions are deleted when the actor wakes and by its scheduled `Sweep` (below).
- The DPoP key inside `auth_request` is the only private key stored, and only for at most 10 minutes.
- At most 20 `oauth_requests` are pending per client address, and at most 10,000 in all; beyond either cap, and beyond the gateway's per-address rate limit, `POST /-/auth/login` answers 303 to `/#/-/sign-in?problem=rate_limited` (per address) or `?problem=busy` (global), keeping `return_to` and adding `Retry-After` (D1, *Failures*). `POST /-/auth/keys` answers the same way on the settings page (`?keys=rate_limited` or `?keys=busy`).
- `purpose` holds the public key to add; it is capped at 4096 bytes and parsed before a row is stored (D1, *Key-write outcomes*). The pending row stores the client address (`client_ip`) for the per-address count. The gateway also rate-limits `POST /-/auth/login`, `POST /-/api/people/resolve` and `GET /-/api/people` lookups of DIDs not yet cached, per client address (D4, *Client address*), and the identity actor caps new DIDs discovered from committer emails at 50 per push and 5,000 forge-wide; the `Sweep` drops from `known_as` a DID known only as `committer` that has not verified a signature within 30 days, and a non-owner DID known only as `signed_in` that publishes no keys and has had no live session or token for 30 days.
- **Scheduled sweep.** The identity actor owns a recurring scheduled action, `Sweep`, registered on every start with Rivet's named recurring schedule `ctx.cron().every(CronEveryOptions { name: "sweep", interval: 15 min, action: "sweep", .. })` (rivetkit 3.0.6 `context.rs`; `ctx.cron().list()` lists entries). Re-registering the same name replaces the existing entry and does not count against the actor's 1000-schedule capacity, so starts never pile up duplicates, and it survives the actor sleeping. It deletes expired rows and runs `RefreshIdentity` then `RefreshKeys` for every DID in `known_as` whose `keys_refreshed_at` or `status_checked_at` is older than 6 hours, in batches, so account status is re-learned even with Jetstream disabled and an inactive DID becomes active again without another event. Rivet persists schedules and wakes the actor for them, so nothing depends on the actor staying awake.
- These rows are bookkeeping and secret-bearing state, and record no events (event-delivery carve-out). So are `sessions.last_used_at`, `people.handle_checked_at`, the Jetstream cursor and housekeeping deletes.

**Event source.** The identity actor owns an `Outbox` (`Outbox::new("identity")`) like the Forge and Repo actors: it has a `Flush` handler, calls `flush_later(SubscriberSource::Forge)` after each append and in `on_start`, and registering a forge subscriber also sends it `Flush`, so a new subscriber gets identity history without waiting for the next identity event. Extension installations never receive identity events, because `Outbox::reaches` requires a repository.

**Actions:**
- `BeginAuth` / `CompleteAuth`
- `PutOutcome` / `TakeOutcome{id, code, binding}` (D1, *Outcome details*)
- `Authenticate{secret_sha256, kind}`: returns the principal, or nothing
- `CreateSession`, `EndSession`, `ListSessions`
- `CreateToken`, `RevokeToken`, `ListTokens`
- `KnowDid{did, because}`
- `RefreshKeys{did}`, `RefreshIdentity{did}` (no other payload; D10, *Account status*)
- `LookupFingerprint{fp}`, `KeysOf{did}`, `People{dids}`
- `SignerStanding{pairs: [{did, fingerprint}]}` (at most 1000 pairs; the caller de-duplicates pairs and splits a larger set into batches of 1000): for each pair, whether an unremoved row of the DID publishes the fingerprint and whether the DID's status is inactive. It reads only the index and `people` and fetches nothing (D8, *Reused results*)
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
| reader: a non-owner the deployment configuration's `readers` admits (D13) | `read` |
| any other signed-in person | none: 403 on every `/-/api` route except `GET /-/api/me`, which answers `rights: []` |
| token | its scopes ∩ the rights its DID has now, limited to `repos` when that is set |

`readers` defaults to `"owners"`, so a forge upgraded without setting it stays as private as the admin token kept it: only owners read. A person without rights can still sign in and sign out (`/-/auth/*` is outside `/-/api`), so the web UI can say who they are and that the forge is private (D15). Their sign-in records no `person.signed_in` and does not make their DID known (D13), and `POST /-/auth/keys` from their session answers 303 to `/` without starting an authorization.

**Route classes:**
- `read`: `GET` routes, plus extension `render` calls. Because the session cookie is SameSite=Lax and so reaches top-level cross-site `GET`s, it matters that a `render` call cannot change state: the host already allows only a single `SELECT` or `WITH` query and refuses effects and calls during `render` (specs: extension-runtime, extension-calls), so `render` is no CSRF vector; writes go through `tool` and `act`, which need `write` and the `Origin` check.
- `write`: extension `tool` and `act` calls.
- `admin`: `POST /orgs`, `POST /orgs/{org}/repos`, `PUT /placements`, `PUT /subscribers/{name}`, `GET /subscribers/{name}/events` (the forge-wide event stream).
- Outside `authenticate`: `/-/hooks/git-host` (HMAC-authenticated walgit webhooks) and the public read-only CUE registry at `/v2/`, as today (`crates/tartan/src/gateway.rs`). The middleware is layered on the `/-/api` router only, so `CUE_REGISTRY` evaluation and walgit webhooks keep working.
- **Registry responses on the forge origin.** `/v2/` is now served from `TARTAN_PUBLIC_URL`, the origin that carries the session cookie, and its manifests are mirrored from operator-listed upstream `sources`. `registry.rs` `manifest()` sets `Content-Type` from the manifest's own `mediaType`, so an upstream could serve `text/html`. The gateway therefore answers manifests only with OCI or Docker manifest media types (falling back to `application/vnd.oci.image.manifest.v1+json`), and adds `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox; default-src 'none'` to every `/v2/` and `/-/api` response.

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
- A successful sign-in always issues a fresh session secret and ignores any session cookie the browser presented, so a cookie value fixed in advance is never adopted. If the browser presented a valid session cookie, that previous session is ended.
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
1. Run `com.atproto.repo.listRecords` on both collections from the DID's PDS, at most 10 pages of 100. A collection whose listing still has a `cursor` after 10 pages is incomplete: its new records are upserted, but no row of that collection is marked removed, and a warning is logged. So a DID that publishes more than 1000 key records cannot make the index look as if keys were removed.
2. Parse each key with `ssh-key`.
3. Keep only signing claims: records whose `usages` include `sign` or are absent (legacy records count as both), whose key parses. Records for `auth` only, with an empty `usages`, or with an unparsable key are not indexed. Diff the listing against the unremoved rows on `(did, collection, rkey, fingerprint)`. A row no longer listed (the record was deleted, its key replaced in place under the same rkey, or its `usages` changed so that it no longer allows signing) gets `removed_at` and a `signing_key.removed` event, and the DID's proof-reuse cache (D8 step 5) is dropped; a listed claim with no unremoved row gets `signing_key.added` (a removed row listed again has `removed_at` cleared). So a `putRecord` that replaces a key, or narrows it to `["auth"]`, records the old fingerprint's removal before the new one's addition, the old fingerprint keeps its `removed_at` for annotations (D8, *Annotations*), and a cached proof bundle cannot verify the replaced key.
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
  - **Serialised runs.** `verify` holds an actor-level `verifying` mutex for its whole run, as `reconcile` holds `reconciling`, because Rivet runs actions, scheduled fires included, concurrently (rivetkit 3.0.6 `start.rs`, `spawn_action_reply`). A wake that finds the mutex held sets an actor-level `verify_rerun` flag and returns; its own schedule is already gone, so it does not touch `verify_schedule`. The running pass, when it has worked its due rows, clears the flag and re-reads the queue while the flag was set and its budget allows. Two wakes never work the same row, so events and hand-offs are not duplicated (subscribers still see at-least-once redelivery, as for every event). Lock order: `verifying` is never acquired while `verify_schedule_lock` is held, and a run releases `verifying` before it calls `ensure_verify_at`, so the two locks are never nested.
  - **Bounded passes.** One run works every due row (`next_at <= now`) in `seq` order, at most 50 per run and for at most 5 minutes of wall clock; a run that stops early leaves the remaining due rows for the next run. Rows of one ref may finish out of order (an earlier update can still be retrying when a later one is judged); results are therefore kept per update, see *Per-update results*. Each attempt runs inside the `verify` action, which keeps the actor awake while it runs. A failed attempt writes `attempts`, `next_at` and `last_error` (a short reason such as `PDS pds.example unreachable`; never a secret or a token) and registers nothing itself. The run ends with exactly one `ensure_verify_at` on every exit path, errors and early returns included: the action wraps the pass so that, whatever the pass returned, it releases `verifying` and then calls `ensure_verify_at` at `now` if due rows remain beyond the 50 or the time budget, otherwise at the minimum `next_at` of the rows still queued, or not at all when the queue is empty. Calling it after releasing the mutex closes the last gap: a wake that arrived during the run and set `verify_rerun` too late to be seen has had its schedule consumed, so the validated helper finds no pending event and registers one. Rivet records an error returned by a scheduled action and registers nothing (rivetkit-core 3.0.6 `finish_schedule_dispatch`), which is why the pass's error must not skip this step. If `ensure_verify_at` itself fails (an SQLite error), the action returns that error, and the next push or `on_start` registers the schedule. Rivet puts no action timeout on scheduled fires (in rivetkit-core 3.0.6 `action_timeout` applies only to HTTP-dispatched actions, `registry/http.rs` and `dispatch.rs`, and a scheduled fire runs inside a keep-awake region), so the 5-minute budget is Tartan's own. A run cut short by shutdown leaves its rows queued, and `on_start` schedules them again. Liveness rests on one invariant: while any row is queued, either a run holds `verifying` (and will call `ensure_verify_at` on exit) or a `verify` event that Rivet still holds is pending at or before the earliest `next_at`;
  - the backoff is 5s × 2ⁿ capped at 5 min; the attempt that runs one hour or more after the first gives up (below);
  - **Giving up.** That attempt, and an attempt that finds `new` no longer served (*Reading objects*), resolve the update instead of leaving it queued, in one transaction like step 6 (*Store*):
    1. Every object to judge whose check completed in this attempt (a final status of `verified`, `unsigned`, `bad_signature`, `unknown_key`, `ambiguous_key`, `unsupported_format` or `not_checked`) is stored as it is. Each object whose check is still failing is stored as `unverifiable`, with `last_error` as its `detail`. Objects that already had a row keep it and are judged as reused results.
    2. If no walk succeeded (the Git host failed on every attempt, or `new` is no longer served), `listed` is unknown, so the objects to judge are the tip alone: `new`, and the annotated tag when `new` names one. A tip with no row is stored as `unverifiable`. A tip that already has a row keeps it, but the update is still non-compliant on a covered ref, because the commits it adds were never listed; its problem is `<new> is unverifiable`.
    3. The update is then judged under D11 (*Compliance*), so on a ref its copied rule covers it is non-compliant. The transaction upserts `update_signing` (with the check row's `old` and copied `rule`), writes `update_objects` rows for the update's `unverifiable` objects (always the tip when no walk succeeded), upserts `ref_signing` under the `seq` guard, appends `ref.signatures.checked` and, when the copied rule covers the ref, `ref.signature.violation`.
    4. It deletes the `signature_checks` row. When this is the head update, the hand-off runs `reconcile` as for any resolved head update, which refuses its configuration with `signing: <oid> is unverifiable` (D11).

    An owner's re-check of such a tip (*Statuses are final*) re-queues the update, and its next attempt walks `old..new` in full, as the first attempt would have. Results that verified within an attempt that later failed transiently are not stored until the update resolves, so each retry proves them again; the 10-minute proof-reuse window (step 5) keeps that to cached bundles in the usual case.
  - `Repo::on_start` calls `ensure_verify_at(min next_at)` if any row is queued, through the same validated helper (a stored id whose event Rivet no longer holds is cleared), so a schedule lost to a crash is recovered on the next start and a surviving one is not duplicated.
- **Per-update results.** Each judged update writes its own row in `update_signing`, keyed `(seq, ref_name)`, with its `new`, its compliance and its violation. `ref_signing` (the ref's current state, read by `signatures?ref=`, badges and the non-compliance banner) carries the `seq` of the update it describes and is upserted only when the new row's `seq` is at least the stored one. A late result for an older update therefore fills in its own `update_signing` row but never overwrites the result of a newer update.
- **The head update.** `config_head` always holds the `new` of the latest `main` update (`RecordRefEvents`, `crates/tartan-forge/src/actors/repo.rs`), so the *head update* is the `refs/heads/main` update with the greatest `seq`. The configuration gate and the hand-off concern only that update.
- **Hand-off to reconciliation.** When `verify_later` resolves the head update's `signature_checks` row (judged, or by giving up), whatever older `main` rows are still queued, it calls `reconcile` once inside the same `verify` action and, only if that returns `Later`, `reconcile_later`. So the final decision for `main` happens inside a scheduled wake. `reconcile_later` itself still runs in `wait_until`, which does not survive sleep; that is existing behaviour, and `Repo::on_start` recovers it by resuming any unapplied head. `Repo::on_start` schedules `verify` before resuming `reconcile_later`. See D11, *Configuration gating*.
- **Pending is visible.** While a row is queued, `GET …/commits/{sha}/signature` for its `new` answers `{status: "pending", attempts, next_at, last_error}`, and so does `GET …/signatures?ref=` while the ref's latest update is queued (an older queued update does not hide the latest result). A 404 is only for an oid that is neither stored nor queued. Objects below `new` in a queued update also answer `pending` once the walk has listed them; before that they answer 404.

**Two sets per update.** Each queued update keeps two sets apart:
- *Objects to judge*: always the commit `new` resolves to (the tip), plus the annotated tag if `new` is one, plus every commit in `old..new` (a walk from `new` hiding only `old`), newest first, up to 1000. The tip is included even when `old..new` is empty, so a rewind of a ref to an ancestor is judged on that ancestor. When `old` is zero (a new ref), nothing is hidden, so the walk covers the ref's history up to the same 1000-commit bound. A walk that stops at the bound sets `truncated`, and the commits beyond it are `not_checked`.
- *Objects to verify*: the objects to judge that have no row in `object_signatures` yet, the tip included. Objects already verified for another ref are not verified again, but they are still judged, with their stored result, against this ref's rule.
- *Reused results*: the objects to judge that already had a row when this check started. A reused `verified` result counts only while it is still *standing* when this update is judged (D11, *Compliance*): some unremoved `signing_keys` row of its `did` (the index holds signing claims only) still publishes its `fingerprint`, and the DID's status is not inactive (D10). The check asks the identity actor once per update with `SignerStanding{pairs: [{did, fingerprint}]}` (D3), which reads the index and `people.status` and fetches nothing. A reused result that is not standing makes the update non-compliant; its `object_signatures` row is left unchanged, so history is not rewritten. Results written by this check's own verification are judged as they are.

**Reading objects.**
- `GitHost` gains `read_new_objects(owner, repo, new, hidden: &[oid], limit, skip: &dyn Fn(oid) -> bool) -> Walk{listed: Vec<(oid, kind)>, raw: Vec<RawObject{oid, kind, bytes}>, truncated}`. `listed` is the set to judge; `raw` holds bytes only for listed oids that `skip` does not exclude (those already in `object_signatures`).
- The walgit implementation fetches `new` over smart HTTP with the Git host token, using gix, into a bare cache repository at `TARTAN_STATE_DIR/git/<repo id>.git`. The token reaches gix as a per-fetch `http.extraHeader` (or a credential helper in memory); it is never written into the cache repository's config.
- It walks from `new` hiding only `old`, newest first, up to 1000 commits. `listed` always contains the tip commit (and the annotated tag `new` names, if any), even when the walk yields nothing because `new` is an ancestor of `old`.
- If walgit no longer serves `new` (the ref was force-pushed again before the check ran and `new` is unreachable from every tip), the fetch fails with a "not our ref" style error. That is not transient: the check gives up at once (*Durable retries*, *Giving up*) with `last_error` `object <oid> is no longer on the host`, rather than retrying for an hour.
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
  seq INTEGER NOT NULL, ref_name TEXT NOT NULL, old TEXT, new TEXT NOT NULL,
  rule TEXT,                                          -- copied from the signature_checks row, for re-checks
  compliant INTEGER NOT NULL, problem TEXT,          -- e.g. "<oid> is unsigned"
  checked_at INTEGER NOT NULL, PRIMARY KEY (seq, ref_name));
CREATE TABLE IF NOT EXISTS update_objects (         -- the unverifiable objects each judged update listed
  seq INTEGER NOT NULL, ref_name TEXT NOT NULL, oid TEXT NOT NULL,
  PRIMARY KEY (seq, ref_name, oid));
CREATE INDEX IF NOT EXISTS update_objects_by_oid ON update_objects (oid);
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
   - For an annotated tag: split at the last line that starts with any signature marker git knows (`-----BEGIN PGP SIGNATURE-----`, `-----BEGIN PGP MESSAGE-----`, `-----BEGIN SIGNED MESSAGE-----` for X.509, or `-----BEGIN SSH SIGNATURE-----`), as git's `parse_signed_buffer` does, which we do by hand because `gix-object` splits only PGP. A PGP- or X.509-signed tag is then `unsupported_format`, not `unsigned`, and a tag message that quotes a marker line is still parsed correctly. A tag that carries a `gpgsig` or `gpgsig-sha256` header (which newer git can write in tags) is handled like a commit's header. Fixtures cover both forms.
   - A commit that carries both `gpgsig` and `gpgsig-sha256` follows git's rule: the header for the repository's hash algorithm is the signature, and both headers are removed from the signed data. A fixture covers this.
   - No signature means `unsigned`. PGP or X.509 armor means `unsupported_format`. More than one `gpgsig` (or `gpgsig-sha256`) header is `bad_signature`.
   - `read_new_objects` recomputes each object's oid from its bytes with collision-detecting SHA-1 (the `sha1-checked` crate, which current gix-hash uses behind its `sha1` feature, as Git uses sha1dc; task 1.2 confirms the feature name for gix 0.90) and refuses a mismatch or a detected collision, so a SHAttered-style pair cannot have a signature on one object judged against the other.
   - A tag `new` names is peeled through nested tags; each tag on the way is an object to judge. If the peeled object is not a commit (a tree or blob), the update's tip is `not_checked`, which is never compliant on a covered ref.
2. **Parse.** `SshSig::from_pem`. Reject any namespace other than `git`, and any hash other than sha256 or sha512 (`bad_signature`). An unsupported key algorithm is `unsupported_format`.
3. **Verify.** `PublicKey::verify("git", signed_data, &sig)`. For `sk-*` keys, also require `flags & 0x01`, as ssh-keygen does. Failure is `bad_signature`.
4. **Find candidates.** Run `LookupFingerprint(SHA256 fingerprint)` against unremoved, supported rows. The index holds signing claims only (D7, *Index refresh* step 3): a record that publishes the key for `auth` only is never indexed, so an auth-only copy of someone's key cannot make their commits `ambiguous_key`.
   - If the committer email (the tagger for a tag) is a DID, call `KnowDid` and `RefreshKeys` if needed, and keep only that DID. If it does not publish the key, the result is `unknown_key`.
   - Otherwise, with several candidates, keep the only one allowed for this ref by the `rule` copied into the queued `signature_checks` row (never the current `signing_rule` row, so a rule applied after the push cannot choose the signer), else the result is `ambiguous_key`.
   - No candidates is `unknown_key`.
5. **Prove** (`tartan-atproto::prove_record`), reusing a bundle fetched at most 10 minutes earlier for the same record. The reuse cache for a DID is dropped when `signing_key.removed`, an identity event or an account event arrives for it, and whenever `RefreshIdentity` finds a changed `#atproto` key or PDS for it, whatever triggered the refresh (sign-in, the sweep or this step), so a revoked key stops verifying at once:
   1. Resolve the DID with evidence (D9). An inactive verified status (D10, *Account status*) means `unknown_key`, with detail.
   2. `GET {pds}/xrpc/com.atproto.sync.getRecord?did&collection&rkey`, at most 1 MiB. The reference PDS answers a missing record with 200 and a CAR proving that it does not exist (the signed commit and the MST path without a leaf; `packages/pds/src/api/com/atproto/sync/getRecord.ts` streams `repo.getRecords` and never sends `RecordNotFound`), which step 5.5 handles. A `RecordNotFound` error, which other PDSes may send, is taken as the same signal without a proof: it runs *Record proved absent* below, storing no non-existence bundle. `RepoDeactivated`, `RepoTakendown` and `RepoSuspended` give `unknown_key` with that detail and trigger `RefreshIdentity`, so an account deactivated while Jetstream is off is caught at proof time and not only by the sweep.
   3. Parse the CAR. Recompute every block's sha2-256 multihash and reject on any mismatch. Allow only CIDv1 with dag-cbor (0x71) or raw codecs. Cap the CAR at 2000 blocks.
   4. Decode the root commit. Require `version == 3`, `did == DID`, and a `rev` not more than 5 minutes in the future. Verify `sig` over the DAG-CBOR of the commit without `sig`, using atrium-crypto with the DID's `#atproto` key. This enforces low-S.
   5. Get the record CID from the signed MST: `jacquard_repo::Mst::load(Arc::new(MemoryBlockStore::new_from_blocks(blocks)), commit.data, None).get("collection/rkey")`. Then call `jacquard_repo::commit::proof::verify_proofs(car, [RecordClaim{collection, rkey, cid: Some(cid)}], did, key)` and require the claim in `verified`. Neither call takes depth or fan-out caps; the 2000-block and 1 MiB CAR caps bound the work, since content addressing rules out cycles. The indexed `signing_keys.record_cid` is only a hint: a mismatch refreshes the index. `verify_proofs` is async and parses the CAR again internally, so each CAR is parsed twice; with the caps above that is acceptable, and the pre-pass of step 5.3 must stay, because it is the only place every block is re-hashed.
      - **Absent record.** If `Mst::get` returns `None` and every block on the MST path from `commit.data` to where the key would sit is present, call `verify_proofs` with `RecordClaim{collection, rkey, cid: None}` (jacquard-repo 0.12.2 documents `cid: None` as "claiming record does not exist") and require that claim in `verified`. Because steps 5.3 and 5.4 have already checked the blocks and the commit signature by the DID's `#atproto` key, this is a signed proof that the record is gone, and it runs *Record proved absent* below. If a block on the path is missing, the proof is incomplete: that is a transient failure, without the identity refresh (the PDS answered inconsistently).
   6. Require the record block's re-hashed CID to equal that CID. Decode it strictly, check `$type` equals the collection, and require the record's key to equal the signature's key and its `usages` to include `sign` or be absent (legacy `sh.tangled.publicKey` records count as both usages). Strict decoding checks field types only and does not enforce the published `org.tangled.key.sshKey` schema's `minLength: 1` on `usages` (an array keyword that looks like a slip for `minItems`); an empty `usages` array decodes and makes no signing claim, so it fails this check like `["auth"]`.

   **When the proof does not verify.** A failed proof is not conclusive on its own, because stale identity evidence makes a valid record look invalid: right after an `#atproto` key rotation or an account migration, the reuse cache, or a PLC directory that lags the PDS's re-signed commit, gives the wrong key. The ATProto sync specification asks for the same handling: "If the signature initially fails to verify, the service should refresh the identity metadata in case it had recently changed. Events with conclusively invalid signatures should be rejected." So:
   - If step 5.4 (the commit's `version`, `did`, `rev` or `sig`) or step 5.5 (a `verify_proofs` claim, for presence or absence, that is not in `verified`) fails, the forge drops the reuse cache for that DID, runs `RefreshIdentity{did}`, which re-resolves the DID evidence with no cache and updates the stored key, PDS and status, and retries steps 5.1 to 5.6 once in the same attempt with that fresh evidence.
   - If the retry fails at step 5.3, 5.4 or 5.5 again, the attempt is a transient failure (*Durable retries*). `last_error` is a short reason such as `proof from pds.example did not verify against did:plc:…'s #atproto key`. The row is retried with the normal backoff and becomes `unverifiable` after one hour, which an owner can re-check. It is never stored as `bad_signature` or `unknown_key`. A CAR that fails step 5.3 (a block hash mismatch, a disallowed codec, too many blocks), or a record block that fails the CID, strict decoding or `$type` checks of step 5.6, is the same kind of transient failure without the identity refresh: the PDS answered inconsistently.
   - If step 5.6 finds that the record's decoded key is not the signature's key, or that its `usages` lack `sign`, the record was replaced or never authorised signing. `RefreshKeys{did}` runs, and step 4 runs once more for this object with the refreshed index. If that finds no other record of the key, the result is `unknown_key` with a detail such as `record org.tangled.key.sshKey/3l… no longer holds this key` or `record does not allow signing`.
   - **Record proved absent.** When step 5.5 proves the record does not exist (a verified `cid: None` claim), or the PDS answers `RecordNotFound` (step 5.2), the absence is conclusive, not a proof failure: no `RefreshIdentity` retry and no transient failure. The forge marks that index row removed (`removed_at`, a `signing_key.removed` event), drops the DID's proof-reuse cache, and runs step 4 once more for this object with the refreshed index. If no other record of the DID publishes the key, the result is `unknown_key` at once with the detail `record <collection>/<rkey> deleted (rev <rev>)` (or `record <collection>/<rkey> not found` for `RecordNotFound`), and the non-existence bundle (same format, `record.cid` null) is stored as its proof. If another record publishes it, that record is proved from step 5.1. A missing path block is not proof of absence and stays transient (step 5.5). So a deleted key record stops verifying on the first attempt, and a `main` configuration signed with it is refused without waiting for the retry hour.
   - `bad_signature` comes only from steps 1 to 3, the SSH signature itself, which no identity data can change.
6. **Store.**
   - Write the bundle to the object store at `proofs/sha256/<hex>.json`, beside the bucket's existing `registry/` and `packages/` prefixes. It is content-addressed and immutable; an existing object is not rewritten. The Repo actor reaches the store, the `tartan-atproto` verifying resolver and fetch-safe client, and `TARTAN_STATE_DIR` through `tartan_forge::Services` (`crates/tartan-forge/src/services.rs`), which today holds only `forge_id`, `git_host` and `extensions`. It gains the `Arc<dyn ObjectStore>` that `crates/tartan/src/main.rs` already builds (`PrefixStore::new(object_store, "tartan")`, so `proofs/` sits beside `registry/` and `packages/`), the resolver and client, and the state directory, and every placement pool's process receives them through `install_services`.
   - Upsert `object_signatures` for the objects verified, judge the update (D11, *Compliance*) using the stored result of every object to judge and the standing of reused results, upsert its `update_signing` row (with the check row's `old` and copied `rule`), replace that update's `update_objects` rows with one row per judged object whose stored result is `unverifiable` (plus the tip when the update gave up without a walk, *Giving up*), upsert `ref_signing` under the `seq` guard, append `ref.signatures.checked` (listing every judged object), and if the rule is broken, `ref.signature.violation`.
   - Delete the `signature_checks` row, all in one transaction.
   - `update_objects` holds only `unverifiable` objects because only they can be re-checked (*Statuses are final*): any other result is final, so a row for it would never be read. Storage therefore grows with unverifiable objects, not with history length, although a new ref lists up to 1000 ancestors.

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

An `unknown_key` result from a proved-absent record (step 5, *Record proved absent*) stores the same format with `record.cid` set to `null`, so an auditor can check offline that the signer's own signed repository no longer held the record at `repo_commit.rev`.

The proof download route serves only digests referenced by that repository's `object_signatures`.

**Statuses are final.** The only exception is `unverifiable`, which an owner can re-queue with `POST …/commits/{sha}/signature/recheck`. The route accepts an oid that has an `update_objects` row, which by construction is an `unverifiable` object or the tip of an update that gave up without a walk; any other oid answers 409. The re-check runs in one transaction:
1. Find every update that judged the object: the `update_objects` rows with that `oid` (through `update_objects_by_oid`). If there are none, nothing changes and the route answers 409.
2. Delete the `unverifiable` rows in `object_signatures` of every object those updates list in `update_objects`, so the re-queued checks verify them again rather than reuse them, and one re-check retries everything an update gave up on. Add the updates that list any of those objects, and repeat until no update or object is added (the closure over `update_objects`, which holds only unverifiable objects and so stays small).
3. For each of those updates, insert a `signature_checks` row from its `update_signing` row: the same `seq`, `ref_name`, `old`, `new` and copied `rule`, with `attempts` 0 and `next_at` now. Each update is judged again by the rule it was first judged by, never by the current `signing_rule`, so a re-check cannot judge a push by a rule applied after it. While the rows are queued, the reads answer `pending`.
4. If one of them is the current head update (above), its copied rule covers `refs/heads/main` and its `update_signing` row is not compliant, reset `config_head.applied` to 0: its configuration was refused and may now be applied.
5. After the transaction commits, call `ensure_verify_at(now)`.

When the re-queued rows are judged, step 6 overwrites their `update_signing` rows and replaces their `update_objects` rows, `ref_signing` is upserted under the same `seq` guard (an equal `seq` replaces it), and the hand-off runs `reconcile` when one of them is the head update. The other objects of those updates keep their stored rows and are judged as reused results.

**Annotations are computed on read.** They come from the identity actor (`signing_keys.removed_at`, `people.status`), so history is never rewritten.

**Alternatives for reading objects.**
- *Read walgit's packs from the bucket*: rejected. It depends on walgit's private manifest format.
- *Add `GET /api/object/{sha}?raw` and a range walk to walgit upstream*: the better long-term source. `GitHost::read_new_objects` hides the choice, so swapping later changes one implementation.
- *Shelling out to `git`*: rejected. It adds a runtime tool dependency.

### D9. Resolving DIDs with evidence

**did:plc:**
1. `GET {plc}/{did}/log/audit`, at most 1 MiB and 1000 entries.
2. Consider the non-nullified entries in order. The first must be a genesis operation: `prev` null, type `plc_operation` (or legacy `create`). Check `did == "did:plc:" + base32lower(sha256(dagcbor(signed_op)))[..24]`.
3. For each operation, recompute its CID and require it to equal the entry's `cid`. Require `prev` to equal the previous accepted operation's CID. Verify `sig` (base64url, low-S, p256 or k256) by one of the previous operation's `rotationKeys` (for genesis, its own). As the PLC specification requires, `sig` must be unpadded base64url with zero trailing padding bits; any other encoding of the same bytes is refused, like a high-S signature. The specification does not say whether historical high-S or non-canonical operations are accepted, so the captured real-world logs (task 2.3) are the check that the rule matches the directory.
4. The DID document comes from the last accepted operation:
   - `verificationMethods.atproto` gives the `#atproto` key;
   - the first `at://` entry of `alsoKnownAs` gives the handle claim (compared lowercased);
   - `services.atproto_pds` gives the PDS.
5. A `plc_tombstone` as the last valid operation leaves no document: no key, handle or PDS. The DID's account status is then `deleted` (D10, *Account status*), matching a PDS's `RepoNotFound`.

A legacy `create` genesis (2022-era) is normalised before step 4, as did-method-plc's `normalizeOp` does: its rotation keys are `[recoveryKey, signingKey]`, its `#atproto` key is `signingKey`, its handle is `handle` with `at://` added when absent, and its PDS is `service` with `https://` added when it has no scheme. The DID hash in step 2 is still taken over the original legacy operation's bytes. A captured legacy log is a test fixture.

We accept the directory's choice of fork (nullified entries). That choice is limited to operations signed by legitimate rotation keys within 72 hours, and we keep the whole log in the bundle so an auditor can re-judge it. The production directory is `https://plc.directory`.

**did:web:**
- Only hostname-level did:web is accepted, as ATProto requires: a DID with a path (`did:web:host:path`) is refused, and a port (`%3A`) is accepted only for `localhost` under dev-identity, matching the `#DID` type (D14).
- `GET https://{host}/.well-known/did.json`, at most 64 KiB, with `id == did`.
- The PDS is the service with id `#atproto_pds` and type `AtprotoPersonalDataServer`, and the signing key is the verification method with id `#atproto` and type `Multikey`, as the ATProto DID specification requires; a document whose `#atproto_pds` service has another type has no PDS.
- Legacy `EcdsaSecp256r1VerificationKey2019` / `EcdsaSecp256k1VerificationKey2019` methods with uncompressed multibase keys are accepted, as ATProto still asks of resolvers. atrium-crypto's `parse_multikey` needs a multicodec prefix, so `tartan-atproto` decodes these forms itself.

**Handles.** jacquard-identity resolves DNS TXT and HTTPS in parallel, prefers DNS, and fails on conflicting TXT records. A handle is accepted only when it matches the document's first `at://` `alsoKnownAs` entry, case-insensitively. DNS TXT lookups go through hickory-resolver (jacquard-identity's `dns` feature), not the fetch-safe HTTP client: it uses the system nameservers, and under dev-identity handles resolve only through `TARTAN_DEV_HANDLE_RESOLVER` with no DNS query, so a test asserting that nothing leaves localhost covers handle resolution too.

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
- redirects followed only for handle-resolution and did:web `GET`s; PAR, token, revocation, `createRecord` and `deleteRecord` never follow a redirect, because a 307 would resend the client assertion and the DPoP-bound body to another host;
- a 10-second timeout;
- body caps enforced while streaming.

### D10. Jetstream as a trigger only

Each gateway process runs a task that:
- connects to `TARTAN_JETSTREAM_URL` (default `wss://jetstream2.us-east.bsky.network/subscribe`; empty disables it; under dev-identity there is no default, see D12);
- on reconnect, rewinds the stored cursor by 5 seconds, as Jetstream recommends (refreshes are idempotent);
- connects to the legacy v1 `/subscribe` endpoint with `wantedCollections=org.tangled.key.sshKey&wantedCollections=sh.tangled.publicKey&requireHello=true`, and then sends the filter as an `options_update` message, never with the DIDs in the URL (10,000 DIDs would not fit in a query string);
- **every `options_update` carries the whole filter**: `wantedCollections` `[org.tangled.key.sshKey, sh.tangled.publicKey]` together with the full `wantedDids` list. The first update after connecting does this, and so does every later change to the DID set. Jetstream's v1 `/subscribe` replaces the connection's whole filter atomically on each `options_update` (bluesky-social/jetstream `internal/subscribe/doc.go`; `filter.go` `ParseUpdatePayload` builds a fresh `Filter`, and an empty list matches everything), so an update without `wantedCollections` would clear the collection filter and deliver every commit by every known DID;
- relies on Jetstream delivering identity and account events whatever `wantedCollections` says, filtered only by `wantedDids` (the Jetstream README: "Regardless of desired collections, all subscribers receive Account and Identity events");
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

Several gateway processes cause duplicate refreshes. These are idempotent and cheap. They also share the one `jetstream` cursor row, so a slower process can move it back, or a faster one past events a slower one has not handled yet. That is safe for the same reason: a replayed event repeats an idempotent refresh, and a skipped one is caught by the 6-hourly sweep. With more than 10,000 known DIDs (Jetstream's limit for `wantedDids`), the task sends `wantedCollections` with an empty `wantedDids`, logs a warning, and relies on its own filter; the 6-hourly sweep covers anything missed. Near the cap an `options_update` is about 320 KB (10,000 DIDs of about 32 bytes). If the server closes the connection or answers with an error after a full update, the task falls back the same way (an empty `wantedDids` and its own filter) for that connection and logs a warning; the devnet `/subscribe` accepts updates up to the cap, and a devnet test sends one at 10,000 DIDs.

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

**Coverage.** Whether a ref is covered is decided by the `rule` copied into its queued `signature_checks` row, not by the current `signing_rule` row. The configuration gate uses the row of the head update (D8, *The head update*). `config_head` has no `seq`, so `reconcile` finds it as the `refs/heads/main` row with the greatest `seq` across `signature_checks` and `update_signing` whose `new` equals `config_head.sha`. When there is no such row (a `config_head` left unapplied from before this change was deployed), its copied rule is none, so it is not gated. A deletion of `main` never moves `config_head` (`crates/tartan-forge/src/actors/repo.rs`, `RecordRefEvents`), so deleting and recreating `main` cannot lift the rule by applying an empty configuration; the recreating push is an ordinary new-ref update, judged on its tip and history.

**Compliance.** A ref is compliant after an update when the update's walk was not truncated and every object to judge (D8, *Two sets per update*) has a stored result that is `verified` and, if the rule lists `signers`, by one of them. A result made on an earlier update (D8, *Reused results*) counts only if it is still standing when this update is judged: some unremoved signing-usage row of its DID still publishes its fingerprint (the row that verified it, or another record of the same DID), and the DID's status is not inactive (D10). Otherwise the object is non-compliant for this update, with the problem `<oid> was verified with a key since removed` or `<oid> signer's account is not active`, which goes into `update_signing.problem` and `ref.signature.violation`. The stored result itself is not changed and still reads `verified` with its annotation. A reused result's signer DID is also checked against this ref's `signers`. Results verified within the update being judged count as they are. So a commit verified on an uncovered branch with a key that is then deleted cannot be fast-forwarded into `main` afterwards: removing a compromised key protects covered refs from every later update, not only from objects verified after the removal. The set to judge is never empty, because it always holds the tip: an update is compliant only when its tip, and every commit it adds, is `verified` by an allowed signer. A rewind (the ref moved backwards or was re-pointed at an ancestor, so `old..new` is empty) is therefore judged on the ancestor it lands on. Rewinding `main` to an unsigned commit, such as one from before the rule existed or a configuration commit A that was refused earlier, is a violation, and its configuration is refused with `signing: <oid> is unsigned`, so the rule cannot be lifted by rewinding past it. Rewinding to a head an allowed signer verified is compliant while that result still stands, so reverting after a violation keeps working; rewinding to a head signed only with a key that has since been removed needs a new signed commit on top. An update is never compliant merely because its objects were checked elsewhere. So pushing an unsigned commit to an unprotected branch and then fast-forwarding `main` to it is a violation on `main`, and so is a commit verified as a DID allowed on the other branch but absent from `main`'s `signers`.

Compliance is per update, not per history. If an unsigned configuration commit A is refused and an allowed signer then pushes B on top of it, the update `A..B` judges only B (its tip and the only commit it adds), so `main` becomes compliant and B's configuration (which includes A's change) is applied. That is intended: B's signature covers B's whole tree. The rule means "each update to the ref is signed by an allowed signer at its tip and every commit it adds", not "every commit in history is signed".

**Configuration gating.** When the rule covers `refs/heads/main`:
- While the head update's `signature_checks` row exists and its copied rule covers `main`, `reconcile` returns `Reconciled::Done` without applying and leaves `config_head.applied = 0`, unlike the existing `Done` path, which sets it to 1 (`crates/tartan-forge/src/actors/repo.rs`). So `on_start` recovery and the hand-off still see the head as unhandled. Rows of older `main` updates do not hold it back. It does not return `Later`, so it does not spend `reconcile_later`'s retry budget (1 s doubling to 64 s, about two minutes, `crates/tartan-forge/src/actors/repo.rs`), which is shorter than `verify_later`'s hour.
- The scheduled `verify` action re-triggers it: when it resolves the head update's row (judged, or by giving up after an hour or on an object no longer served, D8 *Giving up*), it runs `reconcile` in the same action (D8, *Hand-off*). Resolving an older `main` update's row does not. Because each retry is a Rivet schedule, this happens even when no other request reaches the actor. After a restart, `on_start` schedules `verify` first.
- Once the head update's row is resolved, `reconcile` reads that update's own `update_signing` row (matching `seq` and `new`), never the ref's latest `ref_signing` row, and if it is not compliant records `config.refused` with the problem `signing: <oid> is <status>`. A head update that gave up is always resolved this way: its `update_signing` row exists and is not compliant, so its configuration is refused with `signing: <oid> is unverifiable`, and an owner's re-check reopens it (D8, *Statuses are final*, step 4). Example: update W (`old..W`) waits on a PDS that is down, and update X (`W..X`) by another signer is then pushed and verified. X's configuration is applied or refused on X's update alone, at once, and the badge shows X; when W later resolves, it fills in W's `update_signing` row only.
- While it waits on the head update, `GET /-/api/orgs/{o}/repos/{r}/config/status` answers `{applied: {commit}|null, pending: {commit, waiting_on: {signature_check: <oid>, attempts, last_error}}|null}`, and the repository page shows "checking signatures…" or "retrying: <last_error>".

**Rejected.** *Refusing the push*: walgit has no pre-receive hook and its `policy.json` has no signature rules. An authenticating proxy in front of walgit could do it later, together with per-person push credentials.

### D12. Development and tests: `tartan-devnet`

`crates/tartan-devnet` (AGPL, `publish = false`) is a library and a binary (`tartan-devnet --listen 127.0.0.1:9400 --seed dev`). It serves, from one in-memory process:
- **A PLC directory.** `GET /{did}`, `/{did}/log/audit`, and genuinely signed genesis operations, so D9 runs unchanged.
- **A PDS.**
  - `com.atproto.sync.getRecord` (CARs from a jacquard-repo MST signed with each person's k256 key). For a record that does not exist it answers as the reference PDS does: 200 with a non-existence CAR (the signed commit and the MST path, no leaf). A test switch makes it answer `RecordNotFound` instead, and another drops one block from the path, so both answers and an incomplete proof are tested;
  - `com.atproto.repo.listRecords`, `createRecord` and `deleteRecord` (DPoP-bound tokens it issued);
  - `com.atproto.identity.resolveHandle`;
  - `com.atproto.sync.getRepoStatus`, answering each person's account status, and `RepoDeactivated`/`RepoTakendown` errors from `getRecord` and `listRecords` for an inactive repository;
  - `/.well-known/oauth-protected-resource`.
- **An authorization server.**
  - metadata, PAR, PKCE and DPoP nonces;
  - `private_key_jwt` checked against the forge's JWKS fetched over HTTP;
  - an authorize page that acts as the person named by the request's `login_hint` ("Sign in as @bob.test"), or lists the seeded people when there is none, and `auto_approve` for tests;
  - token and revocation endpoints;
  - support for the granular key scope, with requested scope tokens checked literally against the client metadata's declared `scope`, as the reference server does, so an undeclared token is refused with `invalid_scope`;
  - test switches to answer PAR or the token request with an OAuth error (`invalid_client`, `invalid_client_metadata`, `invalid_scope`) for the `client_rejected` and `scope_refused` tests, to grant a narrower scope than requested, and to make the PDS answer `createRecord`/`deleteRecord` with an error, for the `write_failed` test;
  - a record of each PAR body it received, so tests can assert the exact `scope` sent.
- **Jetstream.** A minimal Jetstream-compatible `/subscribe` that emits the devnet's own commits, so the consumer is tested too. It implements v1's filter semantics, including that each `options_update` replaces the whole filter (an empty list matches everything), and a test control makes a person write a record in an unrelated collection such as `app.bsky.feed.post`, so tests catch an update that drops `wantedCollections`.
- **Test controls.** Test-only endpoints under `/devnet/` add or remove key records directly in either collection (including the same key in both `org.tangled.key.sshKey` and `sh.tangled.publicKey`), change handles, tombstone DIDs, set a person's account status at the PDS, rotate a person's `#atproto` key with a signed PLC operation while the PDS keeps serving commits signed with the old key for a set number of `getRecord` answers before it re-signs, and make Jetstream emit an account or identity event whose payload disagrees with the PDS (for example `takendown` while the PDS says active).

People are derived deterministically from `--seed`, so DIDs are stable. The seed `dev` has exactly the people `demo.sh` uses: `alice.test` (the owner in `dev.sh` and `demo.sh`), and `bob.test`, `carol.test`, `dave.test` and `erin.test` (not owners), so the demo's issues can be assigned to DIDs and the demo can show read-only access and a signature by a signer the rule does not allow next to the owner's. `demo.sh` sets `readers: "signed_in"` so that bob can read; `dev.sh` keeps the private default, so signing in there as anyone but alice shows the private-forge page.

**Signing keys for people.** Each seeded person also gets a deterministic Ed25519 SSH signing key, derived from `--seed` and the handle, published at startup as an `org.tangled.key.sshKey` record with `usages: ["sign"]`. Commands:
- `tartan-devnet did <handle>` prints the person's DID (`dev.sh` and `demo.sh` put alice's in `owners`, and `demo.sh` writes DIDs into its issues).
- `tartan-devnet ssh-key <handle> --out <path>` writes that person's private key in OpenSSH format with mode 0600 (and `<path>.pub`), so a script or a person can sign with `git -c gpg.format=ssh -c user.signingkey=<path> -c user.email=<DID> commit -S`. Git signs SSH commits by running `ssh-keygen -Y sign` (Git 2.34 or later), so running-locally.mdx lists OpenSSH's `ssh-keygen` as a prerequisite of the demo, and `demo.sh` checks for it and for that Git version before it starts.

The devnet keeps its state in memory. Seeded people and their seeded keys are recreated identically on every start, so a demo that reuses its state directory still verifies; keys added or removed through the settings page, handle changes and rotations do not survive a restart, and the next `RefreshKeys` records their removal. demo.mdx says so.

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

**Tests** sign commits in-process with `ssh-key` (`SshSig::sign`, Ed25519 and an `sk-`-style fixture) and write the objects with `git hash-object -w`. No ssh-keygen is needed for tests; only `demo.sh`, which signs through `git commit -S`, needs it.

**The demo's signing story.** `demo.sh` adds to the demo repository's `tartan.cue` a rule `signing: types.#Signing & {require: [{refs: ["refs/heads/main", "refs/heads/release/*"], signers: [<alice's DID>]}]}`. It pushes the configuration commit signed by alice (committer email her DID) and waits for `config.applied`. It then pushes a commit signed by bob (committer email his DID) to `release/1.0`, a covered branch, so `main` stays compliant with its configuration applied, while the repository page shows "Signed by @alice.test" on `main` and a violation banner for `release/1.0` naming @bob.test. The first push is judged by the rule from before it (none), so it applies the rule; bob's push is judged by it.

**PLC verifier tests** use the devnet plus a few audit logs captured from plc.directory and committed as testdata. These are public data, and include a log with a rotation and a nullified operation.

**Alternatives.**
- *`@atproto/dev-env` or the Bluesky PDS container*: real implementations, but they need Node or Docker and Postgres-free PLC images that do not exist officially. They remain useful for a manual interoperability check (task 10.3).
- *Mocks at the trait level only*: they would leave OAuth, CAR and PLC parsing untested end to end.

### D13. Owners and the end of the admin token

`Deployment` gains `owners: Vec<Did>`, validated as in the spec, and `readers`, which says who else may read:

```cue
package forge

owners: ["did:plc:abcdefghijklmnopqrstuvwx"]
readers: "owners"          // default: only owners read; everyone else gets a private-forge page
// readers: "signed_in"    // any ATProto account that signs in may read every org and repository
// readers: ["did:plc:…"]  // owners plus these DIDs may read
```

`readers` is optional and defaults to `"owners"`, which keeps an upgraded forge as closed as the admin token kept it. A list must hold distinct valid DIDs, validated like `owners`. Like `owners`, it changes only when the forge restarts with a new configuration, and rights are computed per request from it (D4), so a DID removed from it loses read access within the 30-second cache window. The binary refuses to start in any of these cases:
- `owners` is empty;
- `TARTAN_CONFIG` is unset;
- `TARTAN_ADMIN_TOKEN` is set.

When the identity actor starts, it calls `KnowDid(owner, "owner")` for each owner, so their keys get indexed. It reads the owner list from `tartan_forge::Services`, which gains `owners` (from the deployment configuration, alongside the fields task 8.1 adds), because the actor runs in a runner process, not in the gateway. On start it also drops the `owner` tag from DIDs no longer listed, so a removed owner leaves the sweep and `wantedDids` once nothing else keeps it known. Rights are still computed from the in-process deployment configuration (D4), so this is watch-list hygiene, not authorization. At startup the forge logs the read mode in force: `readers: owners` ("only owners can read; other people who sign in see a private-forge page"), `readers: signed_in` ("any ATProto account can sign in and read every org and repository"), or the number of listed reader DIDs.

**Who becomes known.** A sign-in records `person.signed_in` and calls `KnowDid(did, "signed_in")` only for a DID with `read` rights (an owner or a reader). A stranger who signs in to a private forge gets a session (so the web UI can name them and say the forge is private) but leaves no event, no `known_as` entry and no `wantedDids` entry. So on a private forge strangers cannot grow the watch list (Risks, *Signed-in DIDs grow the watch list*), and signing in does not make them candidate signers who could publish a copy of someone's key (Risks, *Same key, different signer per ref*).

New CLI commands:
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
| `POST /-/auth/outcome` | outcome cookie or session, Origin-checked | read and consume a failure's details once (D1, *Outcome details*) |
| `POST /-/auth/keys` | session (self), Origin-checked, top-level form POST | start key add or remove; sets the pre-auth cookie; every outcome lands on `/#/-/settings?keys=<outcome>` (D1, *Key-write outcomes*) |
| `GET /-/api/me` | any | `{did, handle, owner, kind, rights, dev_identity}` or `{anonymous: true, dev_identity}`; `rights` is `[]` for a signed-in person the forge does not let read (D4); `dev_identity` tells the static UI to offer the devnet shortcut |
| `GET /-/api/people?did=…` | read | handles for ≤100 DIDs |
| `POST /-/api/people/resolve {handle}` | read | handle → DID, for person inputs |
| `GET /-/api/people/{did}/keys`, `POST …/keys/refresh` | read / self or owner | key index |
| `GET/POST /-/api/tokens`, `DELETE /-/api/tokens/{id}` | session / self or owner | access tokens |
| `GET /-/api/sessions`, `DELETE /-/api/sessions/{id}` | session | own sessions |
| `GET /-/api/orgs/{o}/repos/{r}/commits/{sha}/signature` | read | stored result + annotations + `proof_url`, or `pending` |
| `GET /-/api/orgs/{o}/repos/{r}/proofs/{sha256}` | read | proof bundle (only digests this repository references). Like the rest of `/-/api`, anonymous callers get 401; signing.mdx tells auditors to sign in or use a read token |
| `POST …/commits/{sha}/signature/recheck` | owner (`write`; a repository-limited owner token for that repository is allowed, since the route is repository-scoped) | re-queue an `unverifiable` result (D8, *Statuses are final*); 409 for any other status or when no update judged the object |
| `GET /-/api/orgs/{o}/repos/{r}/signatures?ref=` | read | ref head result and compliance, or `pending` |
| `GET /-/api/orgs/{o}/repos/{r}/config/status` | read | applied commit and any pending one with what it waits on |

**`return_to`.** The web UI routes in the fragment (`createWebHashHistory`), so `return_to` is a path-absolute reference on `TARTAN_PUBLIC_URL` such as `/#/acme/api/issues`. It must start with exactly one `/`, must not start with `//` or `/\` (browsers turn `\` into `/`), must contain no control characters, and must parse as a path-absolute relative reference; otherwise the callback redirects to `/`.

**Web UI changes** (all routes are hash routes):
- `/#/-/sign-in` page (handle field, `problem` messages from D1 *Failures*, hosts and server text only from `POST /-/auth/outcome`, never from the URL, `return_to`), following the `/-/settings` convention for forge pages so it never collides with the `/:org` route. Under dev-identity it also offers a "Sign in as @alice.test" shortcut that runs the full OAuth flow.
- **Private-forge page.** The UI reads `owner` and `rights` from `GET /-/api/me` once per page load and keeps them in one store. When a signed-in person's `rights` is empty (the forge's `readers` does not admit them, D13), every route except `/#/-/sign-in` shows one page instead of the forge: "This forge is private; ask an owner for access.", naming the person by handle, with a sign-out button. It makes no other `/-/api` call, so it never shows a raw 403.
- **Read-only presentation.** Until membership exists only owners may write (spec: forge-access), so on a forge whose `readers` admits other people, a signed-in reader is the usual visitor. When `rights` holds `read` but lacks `write`:
  - the first-party write controls are hidden or disabled: the New issue button and `IssueNewView`, edits in `IssueView` (title, body, state, properties), Kanban drag and drop and extension `act` buttons, and org and repository creation (`CreateSlugForm` in `OrgsView` and `OrgView`); when `rights` lacks `admin`, org and repository creation is hidden as well;
  - token creation offers only the `read` scope (a token's rights are its scopes intersected with its DID's rights, D4, so `write` or `admin` would be inert);
  - a notice under the header reads "Read-only: only forge owners can make changes until membership exists.";
  - a 403 from a write call (a `tool` or `act` call, or an org or repository creation, including calls the UI could not know in advance) is shown as that same explained message instead of the response body. A 403 is mapped only when `/-/api/me` reports the caller lacks the needed right; any other 403 (such as the CSRF `Origin` mismatch of D4, whose body names the expected origin) keeps the server's text.
- An account menu in the header (handle, owner badge, sign out). A 401 redirects to sign-in. Because anonymous callers get 401 on all of `/-/api`, an anonymous visitor is sent straight to sign-in; there is no public read-only view until membership exists, and identity.mdx and web.mdx say so.
- **Settings** loses the admin-token field and gains:
  - *Access tokens*: create, list, revoke. The `tartan_pat_` secret is shown exactly once, with a copy button and a note that it cannot be retrieved later; the forge stores only its hash (D5).
  - *Signing keys*: list, add or remove through D1 (a top-level form POST), a message for each `keys=<outcome>` from D1 *Key-write outcomes*, with hosts, messages and record names only from `POST /-/auth/outcome` (`scope_refused` shows the help for publishing with other clients), refresh with the time of the last refresh ("keys last refreshed 3 h ago"), help text for publishing with other clients, and the exact Git setup with the person's DID filled in: `git config gpg.format ssh`, `git config user.signingkey <key>`, `git config commit.gpgsign true`, `git config user.email <DID>`.
  - *Sessions*.
- A `PersonInput` (handle or DID, resolved to a DID, chips show handles) replaces the hard-coded `['admin']` suggestions in `IssueNewView` and `IssueView`. `PersonDisplay` renders the `person` node.
- **Repository overview:** each branch head gets a signature badge ("checking signatures…" or "retrying: <reason>" while pending), a banner names refs that are not compliant, and another says when a `main` configuration waits on a signature check.
- A new **commit signature view** at `/#/:org/:repo/commit/:sha/signature` shows the status, the verified signer separately from the claimed author and committer, the key, proof download and annotations (as neutral notes).
- **Signature labels.** One table drives the badge, the commit view and the docs:

  | Status / annotation | Label | Tone |
  | --- | --- | --- |
  | `verified` | Signed by @handle | green |
  | `verified` + `key_removed_at` | Signed by @handle, note "key removed on <date>" | green, neutral note |
  | `verified` + `did_deactivated_at` | Signed by @handle, note "account deactivated on <date>" (or "account deleted on <date>" for `deleted`, a tombstoned DID included) | green, neutral note |
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
| repo:<id> | `ref.signature.violation` | `ref, old, new, rule, commits[{sha, status, problem?}]` (≤100; `problem` set when a `verified` result no longer stands) |

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
- [Non-owners become read-only or locked out] Today anyone with the admin token can do everything. → Owners can be many DIDs, and tokens serve automation. Membership is the next change.
- [Opening a forge to every ATProto account] Sign-in needs `TARTAN_PUBLIC_URL` on public HTTPS, so with `readers: "signed_in"` anyone with a Bluesky or other ATProto account can read every org, repository, issue and proof bundle. → `readers` defaults to `"owners"`, so an upgrade keeps today's privacy; opening the forge is an explicit line in the deployment configuration, the startup log states the mode in force (D13), and configuration.mdx and deploying.mdx say what `signed_in` exposes.
- [walgit cannot refuse pushes] A violating commit lands. → Violations are recorded and shown, and config application is gated on `main`. Rejection is left to a later push proxy.
- [Git object cache disk use] → Bounded LRU in `TARTAN_STATE_DIR`, disposable.
- [Jetstream is unauthenticated JSON] → Used only as a trigger, account events included: keys come from proof fetches and account status from the verified PLC log and `getRepoStatus` (D10, *Account status*), and the consumer ignores DIDs it does not know and collections other than the two key collections (D10).
- [A PDS can replay an old repository state] D8 step 5.4 bounds `rev` only from above. A malicious PDS could serve a signed CAR from before a key was deleted. → This is inside the custodial-PDS trust the design already accepts: the PDS holds the `#atproto` key and could sign anything. Deleting a key therefore does not stop a malicious PDS from proving it; rotating the `#atproto` key (a PLC operation) does. signing.mdx says so.
- [Removing a compromised key does not un-verify history] Results are snapshots (D8). → signing.mdx tells people that removing a key leaves past results `verified` with a note, and to rotate keys; see Open Questions. What is displayed is not what is admitted: a later update of a covered ref counts a stored result only while its key still stands (D11, *Compliance*).
- [Retired keys block rewinds and late merges] Because reused results must still stand, after a key is retired (deleted or rotated), a covered ref cannot be rewound or fast-forwarded to commits signed only with it; a new commit signed with a current key is needed on top. signing.mdx says so. Standing is read from the index, which learns of a deletion through Jetstream within seconds, at proof time, or through the 6-hourly `Sweep`; with Jetstream disabled, the person's refresh button (`POST /-/api/people/{did}/keys/refresh`) closes the gap at once.
- [Revocation lag across processes] Up to 30 seconds (D4, *Lookups*).
- [The PLC directory chooses forks] → Signatures are verified on the chosen chain, and the full log is kept in the bundle. A malicious or compromised directory could also serve an older prefix of the log, from before a `#atproto` or rotation-key rotation, and it would still verify. That is inherent to did:plc; the bundle keeps the evidence for an audit, not prevention.
- [Forge not reachable from authorization servers] A private forge, or a wrong `TARTAN_PUBLIC_URL`, makes every sign-in fail at PAR. → `client_rejected` says the forge's configuration was refused and to tell the operator; deploying.mdx and identity.mdx state the public-HTTPS requirement; operator tokens still work.
- [Same key, different signer per ref] Choosing among several candidate signers uses the rule of the ref where the object is first checked, and the stored result is then reused on other refs. A commit can therefore be `verified` as one DID, or be `ambiguous_key`, depending on which ref was pushed first. This fails closed: on a ref whose `signers` exclude the stored DID it is a violation. A committer email that is a DID removes the ambiguity. It can also be used to grief on a forge with `readers: "signed_in"`, where anyone can sign in and so become a known DID (on a private forge strangers are never made known, D13), then publish a copy of an owner's public key; the owner's commits that name no DID and are first checked on an uncovered ref then become `ambiguous_key`, which is final, and a later fast-forward of a covered ref to them is a violation. The defence is the same: owners set `user.email` to their DID, as signing.mdx asks of everyone. signing.mdx also warns that such a result is final, so the commit reaches a covered ref only as a new commit, since only `unverifiable` can be re-checked.
- [Committer DIDs grow the watch list] DIDs learned only as committers are capped at 50 per push and 5,000 forge-wide; a `committer`-only DID never verified as a signer is dropped from `known_as` after 30 days, so it leaves the sweep and Jetstream's `wantedDids`.
- [Signed-in DIDs grow the watch list] On a forge with `readers: "signed_in"`, DIDs known only as `signed_in` have no cap: someone with their own PDS or did:web and many addresses can sign in as many DIDs, each adding sweep work and a `wantedDids` entry. On a private forge, or one listing its readers, strangers who sign in are never made known (D13). → The per-address sign-in limits slow this, and above 10,000 known DIDs Jetstream falls back to the consumer-side filter (D10). The `Sweep` also drops from `known_as` a DID known only as `signed_in` that is not an owner, publishes no keys, and has had no live session or token for 30 days.
- [Shared sign-in capacity] One client could exhaust a global pending-request cap. → A per-address cap of 20 sits under the global 10,000, and the client address is derived only from trusted proxies (D4).
- [Breaking types change] → It is published as `v0.2.0`, and `v0.1.0` stays served.

## Migration Plan

1. Deploy the new binary with `owners` (operators find their DID from their handle with `tartan identity resolve <handle>`, or any ATProto tool), `TARTAN_PUBLIC_URL`, and `TARTAN_OAUTH_KEYS_FILE` (from `tartan oauth keygen`). Remove `TARTAN_ADMIN_TOKEN`. The forge refuses to start until all of this is done, so a half-migrated forge never starts. Decide who may read: leaving `readers` out keeps the default `"owners"`, so the migrated forge is as private as the admin token kept it, and other people who sign in see only a private-forge page; set `readers` to a list of DIDs to admit named people, or to `"signed_in"` to let any ATProto account read everything. The startup log names the mode in force.
2. Replace scripts' admin token with `tartan token create --did <owner> --scopes admin …`.
3. List `tartan.dev/types@v0.2.0` and the new Issues and Kanban versions in `modules`, then bump repositories' dependencies when they want DID persons. Repositories on `v0.1.0` keep working.
4. Existing actor databases gain new tables only (`CREATE TABLE IF NOT EXISTS`). No existing table changes.
5. A dev or demo forge seeded before this change keeps its bare-name person values as plain text (D14). Deleting the `.dev` or demo state directory gives the DID-based demo of task 11.1.
6. **Rollback.** Redeploy the previous binary with `TARTAN_ADMIN_TOKEN`. The new tables are ignored, and person values written as `did:plc` or portless `did:web` DIDs stay valid strings under `v0.1.0`'s regex. `did:web:localhost%3A…` values do not (`%` is outside that regex); they occur only in development data.

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

### Round 6

Round 6 raised two blocking objections; both were checked and accepted, and none was rejected.
- *Proof failures had no outcome* (atproto): confirmed in the ATProto sync specification (atproto.com/specs/sync: "If the signature initially fails to verify, the service should refresh the identity metadata in case it had recently changed. Events with conclusively invalid signatures should be rejected."). D8 step 5 now says what each failure means: a failed commit signature or MST proof drops the reuse cache, runs `RefreshIdentity` and retries once; a second failure is transient and ends `unverifiable` after an hour, never `bad_signature` or `unknown_key`; a record that no longer holds the key, or does not allow signing, is `unknown_key` after `RefreshKeys` and one more lookup. commit-signatures gains "A proof that does not verify is not final" and tartan-devnet gains a control that rotates a person's `#atproto` key while the PDS serves the old signature.
- *Spoofed text from crafted redirect links* (security): D1 *Outcome details*. Redirects carry only fixed-list codes, a known `status`, an `outcome` id and `return_to`; hosts, server messages and record names live in the identity actor's `auth_outcomes` rows, bound to an outcome cookie (sign-in) or the initiating session (key writes). One deviation from the suggested shape: the details are read with `POST /-/auth/outcome`, not `GET`, because a same-origin `GET` `fetch` carries no `Origin` header (Fetch standard), so the requested Origin check could not pass on a `GET`; the `POST` also makes the consuming read explicit.

Non-blocking suggestions applied: a tombstoned DID's status is `deleted` (D9, atproto-identity, the label table); `RefreshIdentity` from any trigger drops the proof cache on a key or PDS change (D8); the callback's PDS-changed race gives `wrong_issuer` and step 7's timing is stated (D1); "steps 8 and 9 always run" (D1); `account_inactive` words keyed by status (D1, tasks 6.6 and 10.4); percent-encoded redirect values with a round-trip test (D1, tasks 6.3 and 10.4); task 6.2's `iss` wording aligned with callback step 4; a JWKS test for no `d` member (task 6.2); the `verifying`/`verify_schedule_lock` order (D8); a test barrier for "Push during a run" (task 8.2); `Services` wiring (D8, task 8.1); the shared Jetstream cursor (D10); the `ambiguous_key` griefing case and the stale PLC prefix (Risks); the outbox retry budget (Open Questions); MPL-2.0 in the licence allowlist (task 1.1); the web files and a final `dev-admin-token` grep (tasks 10.4 and 11.3); and a settings-page key followed by a green signed push in the end-to-end tests (task 11.2). Not applied: a separate `status_unconfirmed` code, a per-state pre-auth cookie name, a `seq` column on `config_head`, and per-object run budgets; they are refinements the implementation can take up without changing the plan's contracts.

### Round 7

Round 7 raised three blocking objections; all were checked and accepted, and none was rejected.
- *A deleted key record ended `unverifiable`* (atproto): confirmed in `bluesky-social/atproto` `packages/pds/src/api/com/atproto/sync/getRecord.ts`, which throws only when the repository has no root and otherwise streams `repo.getRecords(storage, commit, [{collection, rkey}])`, so a missing record is answered with 200 and a CAR without a leaf; and in docs.rs jacquard-repo 0.12.2, where `RecordClaim.cid: Option<Cid>` documents `None` as "claiming record does not exist". D8 step 5 now treats a verified non-existence claim (or a `RecordNotFound` error) as conclusive (*Record proved absent*: key row removed, proof cache dropped, one more lookup, `unknown_key` at once, the non-existence bundle stored), and a missing path block as transient. commit-signatures gains "PDS proves the record absent"; the devnet answers like the reference PDS, with switches for `RecordNotFound` and a missing block (tasks 3.1, 3.3, 8.3).
- *demo.sh could not seed DID persons* (product-ops): confirmed in `scripts/demo.sh` (bare-name assignees, `tartan.dev/types@v0` pinned at `v0.1.0`, `seed()` under `set -e`) and `crates/tartan/tests/kanban.rs` (`"owner": "alice"`). The devnet seed `dev` now holds all five demo people (D12); task 11.1 resolves their DIDs with `tartan-devnet did`, bumps the demo's module deps, and checks the board's `@handle` group labels; task 10.2 converts the kanban fixture.
- *The signing demo had no task* (product-ops): the scope was kept rather than removed. The devnet publishes a deterministic Ed25519 signing key per person and `tartan-devnet ssh-key` writes it (D12); `demo.sh` adds a `signing` rule on `main` and `release/*` with alice as signer, pushes the configuration signed by alice and a commit signed by bob to `release/1.0` (D12, *The demo's signing story*; task 11.1); running-locally.mdx lists `ssh-keygen` and Git 2.34 or later. Bob's commit goes to a covered release branch rather than `main`, so `main` keeps its configuration applied while the violation is still shown.

Non-blocking suggestions applied: canonical unpadded base64url PLC signatures (D9); the Jetstream fallback for a refused large `options_update` and a devnet test at 10,000 DIDs (D10, task 7.3); a `listRecords` listing cut at 10 pages never removes rows (D7, task 7.2); candidates only from rows with signing usage (D8 step 4, task 8.3); tag signatures split at any git marker, and `gpgsig` headers in tags (D8 step 1, task 2.7); fresh session secrets at sign-in (D6, task 6.4); no redirects on OAuth and record-write POSTs (D9); `/-/hooks/git-host` and `/v2/` outside `authenticate`, and `/v2/` media types and response headers hardened (D4, task 6.4); signed-in watch-list growth (Risks, D3, task 5.1); a startup log line about open read access, and `owners` in `Services` with stale owner tags dropped (D13, task 5.1); the event-delivery carve-out widened to signature-check bookkeeping; the repo-config "any other key" wording limited to `signing` and its rules; the `verify_proofs` double parse noted (D8); hickory's DNS path under dev-identity (D9); the gix SHA-1 feature confirmed rather than assumed, and a jacquard-repo cost threshold (task 1.2); `server.strictPort` in `web/astro.config.ts` and dev.sh not starting Astro (task 11.1); devnet restarts (D12, task 10.6); the authorize page acting on `login_hint` (D12); waiting for the identity actor before seeding (task 11.1); and the migration note for pre-change demo state. Not applied: `swapCommit`/`swapRecord` on key deletes (the outcome is judged by the refreshed index anyway); refusing unknown top-level configuration keys (a compatibility change for repositories that export helper fields, left for a later change); stating whether extensions may subscribe to the new `ref.signature*` events (this change has no extension-events delta; they reach repository installations through `Outbox::reaches` like other repository events); splitting tasks 6.2, 6.3, 7.4 and 8.2 into sub-tasks; and documenting two-tab outcome-cookie overwrites.

### Round 8

Round 8 raised three blocking objections; all were checked and accepted, and none was rejected.
- *Removed keys still admitted staged commits* (security): confirmed that D8 *Two sets per update* and D11 *Compliance* counted stored results from other refs "exactly as fresh ones", and that the commit-signatures requirement said the same, so a commit verified on an uncovered branch before its key record was deleted could later be fast-forwarded into `main` and its configuration applied. D8 adds *Reused results* and D11 *Compliance* now counts a result from an earlier update only while it still stands (an unremoved signing-usage row of its DID publishes the fingerprint, and the DID is not inactive), read with the identity actor's new `SignerStanding` (D3). The stored row is never changed, so displayed history is unchanged. The deterministic index check was chosen over re-proving each reused object, which would add network fetches and transient failures to every merge. commit-signatures gains "Key removed before the fast-forward" and "Signer deactivated before the fast-forward", config-reconciliation gains "Configuration signed with a since-removed key", Risks gains the retired-key rewind note, and tasks 8.3 and 9.3 gain the tests.
- *The `unverifiable` re-check could not be built* (architecture): confirmed that `update_signing` had no `old`, no copied `rule` and no object list, and that the `signature_checks` row is deleted when the check is stored. `update_signing` gains `old` and `rule`, a new `update_objects` table (indexed by `oid`) records what each update judged, and D8 *Statuses are final* gives the re-check as one transaction: delete the `unverifiable` row, re-queue each update with its original `old`, `new` and copied rule, reset `config_head.applied` only for a refused head update, then `ensure_verify_at(now)`. The membership table was preferred to scanning `ref.signatures.checked` events. Confirmed that `Repo::reconcile` sets `applied = 1` on the refused path (`crates/tartan-forge/src/actors/repo.rs`), so the reset is needed. Tasks 8.2 and 8.4 gain the table and the two-ref re-check test; commit-signatures gains "Owner re-checks an unverifiable commit".
- *Read-only UI undefined* (product-ops): confirmed that `web/src/lib/api.ts` surfaces the response text as the error message. D15 adds *Read-only presentation* driven by `GET /-/api/me` `rights` (write controls hidden or disabled, the notice, `read`-only token scopes, and an explained 403 that leaves other 403s such as the `Origin` mismatch untouched); forge-access gains "The web UI shows read-only access"; tasks 10.4, 10.5 and 11.1 state the tests and the `@bob.test` browser check.

Non-blocking suggestions applied: step 5.6 accepts absent `usages` (legacy records count as both) and decodes an empty array as no signing claim, without enforcing the schema's `minLength` on an array (D8, task 8.3); hostname-only did:web with a port only for `localhost` under dev-identity (D9); the `render` CSRF note, citing the existing render guard of extension-runtime and extension-calls (D4); sign-in input normalisation (D1); the identity actor in the default pool (D3); the repository-limited owner token allowed on re-check (D15, task 8.4); `tartan token create` reading `TARTAN_CONFIG` and the Rivet variables, and `tartan identity resolve` needing neither (task 5.4); `demo.sh --keep` reusing keys without repeating signed pushes (task 11.1); and the violation banner ordering (task 10.5). Not applied: a minimal first slice (a sequencing choice for whoever implements, which does not change the plan's contracts); a `Reconciled::Waiting` variant, the deletion-head gate fallback, the ssh-key feature note, splitting task 8.2, recording jacquard-repo cost numbers, absence-proof `rev` comparison, a larger PLC sample, and guarding devnet seed DIDs in production; they are refinements the implementation or a later round can take up.

### Round 9

Round 9 raised two blocking objections; both were checked and accepted, and none was rejected.
- *The give-up path wrote nothing* (architecture): confirmed that D8 *Durable retries* and *Reading objects* said only that a check "records `unverifiable`", while step 6 (*Store*), the only writer of `update_signing` and `update_objects`, presumed a successful walk, so `reconcile`, the re-check and `ref_signing` had nothing to read for exactly the updates the re-check exists for. D8 adds *Giving up*: one transaction like step 6 that stores completed results as they are and still-failing objects as `unverifiable`, judges the tip alone when no walk ever succeeded, writes `update_signing`, `update_objects`, `ref_signing`, `ref.signatures.checked` and any violation, deletes the check row and hands a head update to `reconcile`, which refuses it with `signing: <oid> is unverifiable` (D11). A re-check re-queues the update and walks it in full. commit-signatures gains "Git host down for an hour", config-reconciliation gains "Head check gives up", and tasks 8.2 and 9.3 gain the tests.
- *Upgrading opened every forge to every ATProto account* (product-ops): confirmed in `crates/tartan/src/gateway.rs` that `require_admin` is a `route_layer` over every `/-/api` route, reads included. The deployment configuration gains `readers` (`"owners"` by default, `"signed_in"`, or a list of DIDs; D13, forge-cue-registry). On the default, a signed-in non-owner gets 403 on `/-/api` except `GET /-/api/me`, which reports `rights: []`, and the web UI shows a private-forge page (D4, D15, forge-access "Non-owner on a private forge" and "Non-owner on an open forge"). The startup log states the mode, Migration Plan step 1 asks operators to choose it, and `demo.sh` sets `readers: "signed_in"` so its `@bob.test` read-only check still holds (task 11.1).

Non-blocking suggestions applied: the granted `repo:` scope compared by meaning through jacquard-oauth's `Scopes` (D1 callback step 6, task 7.4); the authorization, PAR and token endpoints required to be `https` but no longer to share the issuer's origin, which the ATProto OAuth profile does not ask (D1 login step 3, task 6.2); `#atproto_pds` of type `AtprotoPersonalDataServer` and `#atproto` of type `Multikey` on did:web (D9, task 2.4); the Jetstream README quoted for identity and account events (D10); key records edited in place, with the index holding signing claims only and keyed by fingerprint, so a replaced or auth-narrowed record records `signing_key.removed` and drops the proof cache (D3, D7, D8 step 4, signing-keys "Key record replaced in place", tasks 3.3 and 7.2); the tie-break by the copied rule (D8 step 4); how `reconcile` finds the head update, the no-row case and the deletion of `main` (D11); `update_objects` limited to `unverifiable` objects, with the re-check's closure over it (D8); `SignerStanding` pairs de-duplicated and batched (D3); strangers on a private forge never made known (D13, Risks); the token secret shown once (D15, task 10.4); the `ambiguous_key` finality note for signing.mdx (Risks, task 8.5); and the offline note and ports for running-locally.mdx (task 10.6). Not applied: storing results verified inside a transiently failed attempt before the update resolves (it would blur which results count as this check's own under *Reused results*; the proof-reuse window bounds the cost, as D8 *Giving up* says); content-addressed CARs separate from bundles (bundles are bounded at 1 MiB each and deduplication can be added behind the store later); a shorter `ref.signatures.checked` payload (a subscriber-visible schema change better made with a consumer in mind); resolving the default Jetstream instance (still an Open Question; task 4.4 documents the privacy trade-off); and stating that runner processes outside the default pool receive `TARTAN_CONFIG` (task 5.1 now says so as a check rather than a design change).

## Open Questions

- Which public Jetstream instance to default to. This is a configuration default only.
- Whether walgit upstream accepts a raw-object endpoint, which would let us drop the gix cache. The interface is already isolated.
- Whether a later change should let owners mark a key "revoked as compromised", so results it verified show a warning rather than a neutral note.
- Whether the outbox's delivery retries (`outbox.rs`, `schedule_retry`) should be deduplicated like `verify`. Each failed flush registers its own schedule against the same 1000-schedule budget, and the identity actor adds another outbox that can fail repeatedly while a subscriber is down. This predates this change.
- Whether to expose operator diagnostics (Jetstream connection state and cursor lag, queued signature checks) through an owner-only status route rather than logs alone. Until then, identity.mdx and deploying.mdx list the log lines to check.
