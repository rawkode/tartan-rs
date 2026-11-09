# Spec Delta

## Purpose

`tt sign`: Git's signing and verification program for Tartan signatures. A signature is a `dev.tartan.signature` record written to the signer's own ATProto repository over a narrowly scoped OAuth session; the commit or tag carries only a reference to that record.

## ADDED Requirements

### Requirement: Setting up Git
`tt sign --setup [--global]` SHALL set, in the repository's configuration (the user's with `--global`), `gpg.format=x509`, `gpg.x509.program` (`tt` when the `tt` on `PATH` is this binary, else its absolute path, or `--program <path>`), `user.signingKey` to the signing DID, `commit.gpgSign` and (unless `--no-tags`) `tag.gpgSign` to true, and add the DID to `tt.trustedSigner`. It SHALL run `tt sign --login` first when no signing session is stored.

#### Scenario: Setup in a repository
- **WHEN** a person with a signing session runs `tt sign --setup` in a repository
- **THEN** `git commit` there produces a Tartan-signed commit without `-S`

#### Scenario: tt not on PATH
- **WHEN** `tt sign --setup` runs from a `tt` that is not the one on `PATH`
- **THEN** `gpg.x509.program` is set to that binary's absolute path

#### Scenario: Replacing another signing setup
- **WHEN** `tt sign --setup --global` runs where `gpg.format` is `ssh` and `user.signingKey` names an SSH key
- **THEN** it warns, naming both old values, before replacing them

### Requirement: Which DID setup writes
`tt sign --setup` SHALL write as `user.signingKey` the DID given with `--did`, else the default signing DID, else the DID of the only stored signing session. With several sessions and no default it SHALL refuse, naming `--did`; with none it SHALL run `tt sign --login` first and use that DID. It SHALL never change `config.toml`.

#### Scenario: Several sessions, no default
- **WHEN** a person with signing sessions for two DIDs and no default signing DID runs `tt sign --setup`
- **THEN** it refuses, naming `--did`, and writes no Git configuration

#### Scenario: Explicit DID
- **WHEN** a person runs `tt sign --setup --did <second DID>` while another DID is the default
- **THEN** `user.signingKey` is the second DID and the default signing DID is unchanged

### Requirement: Installing the pre-push hook
`--setup --hook` SHALL install at `git rev-parse --git-path hooks/pre-push` a hook marked `# tt-sign-hook v1` that runs `tt sign --flush --hook`, then `pre-push.tt-chained` with Git's arguments and input, exiting with its status. A marked hook SHALL be rewritten in place; an unmarked one renamed to `pre-push.tt-chained`, refusing if that exists. It SHALL refuse with `--global`, and a shared or tracked hooks path without `--hooks-path-ok`.

#### Scenario: Existing hook kept
- **WHEN** a repository already has a `pre-push` hook and the person runs `tt sign --setup --hook`
- **THEN** a push runs `tt sign --flush --hook` and then the original hook with the same arguments and standard input

#### Scenario: Hook installed twice
- **WHEN** a person with an existing `pre-push` hook runs `tt sign --setup --hook` twice
- **THEN** there is one `tt` hook, the original hook is intact as `pre-push.tt-chained`, and a push runs each exactly once

#### Scenario: Chained hook refuses
- **WHEN** the chained original hook exits non-zero while the PDS is unreachable
- **THEN** the push is refused, as it was before `tt` was installed

#### Scenario: Shared hooksPath
- **WHEN** `core.hooksPath` points to a directory outside the repository's common Git directory, or to a tracked `.husky/` directory, and the person runs `tt sign --setup --hook`
- **THEN** `tt` refuses, naming `--hooks-path-ok`, and changes nothing

#### Scenario: Global hook refused
- **WHEN** a person runs `tt sign --setup --global --hook`
- **THEN** `tt` refuses, saying hooks are per repository, and changes no configuration

### Requirement: Undoing the setup
`tt sign --setup --undo [--global]` SHALL unset each setting `--setup` writes whose value is still the one `tt` wrote, keep values changed since, and in a repository remove a marked `pre-push` hook and rename `pre-push.tt-chained` back.

#### Scenario: Leaving Tartan signing
- **WHEN** a person who ran `tt sign --setup --hook` over an existing hook runs `tt sign --setup --undo`
- **THEN** Git no longer signs with `tt`, and the original `pre-push` hook is back in place

### Requirement: Setting up verification only
`tt sign --setup --verify-only [--global]` SHALL set only `gpg.x509.program`, SHALL NOT write `gpg.format`, `user.signingKey`, `commit.gpgSign` or `tag.gpgSign`, and SHALL need no signing session.

#### Scenario: Reviewer
- **WHEN** a person without a signing session runs `tt sign --setup --verify-only` and then `git log --show-signature`
- **THEN** Tartan signatures are verified by `tt` and no login is started

#### Scenario: Reviewer who signs with SSH
- **WHEN** a person with `gpg.format=ssh` and `commit.gpgSign=true` runs `tt sign --setup --verify-only --global` and then `git commit`
- **THEN** those settings are unchanged and the commit is signed with SSH, not by `tt`

### Requirement: tt speaks git's signing program interface
`tt` SHALL treat an invocation whose first argument is `--status-fd=<n>` or `--keyid-format=long` as `tt sign`, parsed before clap. `tt sign` SHALL sign when given `--status-fd=<n>` with `-bsau <key>` (or `-b`, `-s`, `-a`, `-u <key>`, `--detach-sign`, `--armor`, `--local-user <key>`), verify when given `--status-fd=<n> --verify <file> -`, and otherwise exit non-zero without prompting, after reading standard input to its end.

#### Scenario: Git signs a commit
- **WHEN** Git runs `tt --status-fd=2 -bsau did:plc:…` with a commit payload on standard input
- **THEN** `tt` writes a signature to standard output and `[GNUPG:] BEGIN_SIGNING` then `[GNUPG:] SIG_CREATED` on file descriptor 2, and the commit is created

#### Scenario: Unknown option
- **WHEN** `tt` is run as `tt --status-fd=2 --clearsign`
- **THEN** it exits non-zero with a message on standard error and writes no signature

### Requirement: tt signs only as Git's x509 program
Because Git passes the same sign argv for `openpgp` and `x509`, `tt` SHALL, before signing, read `git config --get gpg.format` in Git's environment and, unless it is `x509`, read standard input to its end, write no record, and exit non-zero with "tt is Git's x509 program; set gpg.format=x509 (run `tt sign --setup`)". On verification, a first argument `--keyid-format=long` SHALL get the same message and `ERRSIG`.

#### Scenario: Configured as the OpenPGP program
- **WHEN** Git runs `tt --status-fd=2 -bsau <key>` while `gpg.format` is unset or `openpgp` (for example `git -c gpg.program=tt commit -S`)
- **THEN** `tt` exits non-zero with that message, writes no record, and no commit is created

#### Scenario: Verified as OpenPGP
- **WHEN** Git runs `tt --keyid-format=long --status-fd=1 --verify <file> -`
- **THEN** `tt` writes `[GNUPG:] ERRSIG`, exits 1, and says to set `gpg.format=x509`

### Requirement: A signature is a record in the signer's repository
To sign, `tt` SHALL read the payload from standard input (at most 16 MiB) and create, in the signing DID's repository under a fresh TID record key, a `dev.tartan.signature` record holding only `namespace` `git`, `algorithm` `hmac-sha256`, `digest` (HMAC-SHA256 of the exact payload bytes keyed with a fresh 32-byte random salt) and `createdAt` (whole seconds). The salt SHALL appear only in the signature.

#### Scenario: Record content
- **WHEN** a person signs a commit in a private repository
- **THEN** the record in their repository holds a salted digest of the payload and a timestamp, and no salt, repository name, object id, author or message

#### Scenario: Guessed private merge
- **WHEN** someone who sees a person's public signature record builds the exact payload of that person's conflict-free private merge of two public commits, with the default message and the record's timestamp
- **THEN** they cannot match it to the record, because the record's digest is keyed with a salt that only the merge commit's signature holds

#### Scenario: Same payload signed twice
- **WHEN** `tt` signs the same payload twice
- **THEN** the two signatures carry different salts and name different records with different digests

#### Scenario: Rebase signs only what it creates
- **WHEN** a person with `commit.gpgSign` runs `git rebase --force-rebase --exec true HEAD~50`
- **THEN** 50 records are written, while a rebase that Git fast-forwards writes none and keeps the existing signatures

### Requirement: Choosing the signing account
The key Git passes SHALL choose the account: a DID selects that DID's stored signing session; anything else (Git passes the committer identity when `user.signingKey` is unset) selects the default signing DID's session, else the only stored signing session. With no usable session, signing SHALL fail with a message on standard error naming `tt sign --login`.

#### Scenario: Two accounts
- **WHEN** a person has signing sessions for two DIDs and commits with `-c user.signingKey=<second DID>`
- **THEN** the record is written to the second DID's repository

#### Scenario: Not logged in
- **WHEN** Git 2.40 or later runs `tt` to sign and no signing session is stored
- **THEN** the commit fails and Git shows `tt`'s message telling the person to run `tt sign --login`

#### Scenario: Not logged in, older Git
- **WHEN** Git before 2.40 runs `tt` to sign and no signing session is stored
- **THEN** the commit fails with Git's "gpg failed to sign the data", and `tt status` names the failure

### Requirement: The signature names the record
On success `tt` SHALL write to standard output exactly `-----BEGIN SIGNED MESSAGE-----`, `Tartan-Signature: 1`, `Record: <at:// URI with the DID>`, `CID: <record CID>`, `Salt: <43 base64url characters>`, `-----END SIGNED MESSAGE-----`, each ending in LF, and to the status descriptor `[GNUPG:] BEGIN_SIGNING` first, then `[GNUPG:] SIG_CREATED D 0 8 00 <unix time> <at:// URI>`, never as the first line. A record whose returned CID differs SHALL fail signing.

#### Scenario: Envelope
- **WHEN** a commit is signed
- **THEN** `git cat-file commit HEAD` shows a `gpgsig` header holding those six lines, naming the record by DID, never by handle

#### Scenario: Git 2.19
- **WHEN** Git 2.19 signs a commit with `tt`
- **THEN** it finds `SIG_CREATED` after the `BEGIN_SIGNING` line and creates the commit

### Requirement: The new record is written alone
`tt` SHALL write the record being signed in an `applyWrites` call holding only that create, bounded to 15 seconds, and SHALL never send queued records in the same call, so a queued record can never make a new signature fail or be queued.

#### Scenario: Poisoned queue
- **WHEN** the queue holds a record the PDS already has and the person signs a new commit online
- **THEN** the new record is published at once and the queued one is dropped as published by the next drain

### Requirement: Writes that cannot be made now are queued
When the PDS cannot be reached, times out, or answers 5xx or 429, or a token refresh fails transiently, and `sign.offline` is `queue` (the default), `tt` SHALL queue the record and still return the signature, whose CID it computes locally. With `fail` it SHALL fail. Authorization errors SHALL always fail. While a stored rate-limit or two-minute back-off window is open, `tt` SHALL queue at once without contacting the PDS, and SHALL never wait inside Git.

#### Scenario: Offline commit
- **WHEN** a person commits with signing on and no network
- **THEN** the commit is created with a signature and the record waits in the queue

#### Scenario: Rate limited
- **WHEN** the PDS answers a record write with 429 and `ratelimit-reset` an hour away, and the person makes ten more commits
- **THEN** every commit is created at once, no further request reaches the PDS before the reset, and eleven records wait in the queue

#### Scenario: Revoked session
- **WHEN** the authorization server refuses the signing session's refresh token
- **THEN** signing fails with a message naming `tt sign --login` and the possibility that the session was used on another machine, and nothing is queued

### Requirement: Refresh failures are classified
Only `invalid_grant`, or a session the authorization server refused or revoked, SHALL be an authorization error. An unreachable authorization server, a 5xx, or a token-endpoint `invalid_client_metadata` or `invalid_client` while the session is otherwise valid SHALL be transient: the record is queued as for an unreachable PDS, the back-off window opens, and the last failure names the forge's client-metadata URL as unreachable, not `tt sign --login`.

#### Scenario: Forge down while signing
- **WHEN** the access token has expired, the authorization server cannot load the forge's client metadata, and the person runs `git commit -S`
- **THEN** the commit is created with its record queued, `tt status` names the forge's client metadata as the cause, and once the metadata is reachable again the next signing or `tt sign --flush` publishes the queue

### Requirement: Queued records are reconciled before they are resent
Before sending a queued record, `tt` SHALL read it with `com.atproto.repo.getRecord`: present with the same CID, it SHALL be dropped as published; with another CID, dropped and reported; absent, sent in `applyWrites` of at most 200 creates. A call that times out or answers 5xx SHALL leave its records queued and unconfirmed, to be read again first.

#### Scenario: Timeout after commit
- **WHEN** the PDS commits a signature record but the answer is lost, and the person then signs another commit online
- **THEN** both records end up published, neither is created twice, and the queue is empty after the next drain

#### Scenario: Replaced while queued
- **WHEN** a queued record's key holds a record with another CID
- **THEN** it is dropped from the queue and `tt status` reports that its signature cannot verify

### Requirement: The queue is locked
Every queue operation (appending, draining, `tt sign --flush`, the `pre-push` hook, revoking a queued record) SHALL hold an exclusive per-DID lock from reading the queue to rewriting it. A signing that finds the lock held SHALL skip its drain instead of waiting.

#### Scenario: Two processes at once
- **WHEN** one process signs while offline and another signs online and drains the queue at the same moment
- **THEN** every signature either process returned names a record that is published or still in the queue

### Requirement: The queue is published
After a successful write, `tt` SHALL drain the queue within the remaining time. `tt sign --flush` SHALL publish what the PDS allows and exit 0 only when the queue is empty, else print how many remain, why, and when to retry, and exit 1. `tt sign --flush --hook`, run by the `pre-push` hook, SHALL do the same but always exit 0. `tt status` SHALL count the queue.

#### Scenario: Back online
- **WHEN** a person who committed offline makes another signed commit online
- **THEN** the new record is written in its own call, the queued one in a later call, and the queue is empty

#### Scenario: Flush while rate limited
- **WHEN** `tt sign --flush` runs with records queued and the PDS answering 429
- **THEN** it prints the remaining count and the reset time and exits 1

#### Scenario: Push while the PDS is down
- **WHEN** a person pushes with the `pre-push` hook installed, records queued and the PDS unreachable
- **THEN** the hook prints the remaining count and the reason, and the push proceeds

#### Scenario: Pushed before flushing
- **WHEN** a commit whose record is still queued is pushed, and `tt sign --flush` runs ten minutes later
- **THEN** the forge's check, retrying meanwhile, ends `verified`

### Requirement: Signing uses a narrowly scoped OAuth session
`tt sign --login` SHALL authorize as a public native ATProto OAuth client whose `client_id` is the configured forge's `/-/auth/tt/client-metadata.json`, with a loopback redirect, PAR, PKCE S256 and DPoP, asking for exactly `atproto repo:dev.tartan.signature?action=create`. `tt` SHALL refresh tokens under an exclusive file lock, because refresh tokens are single-use.

#### Scenario: Scope asked
- **WHEN** a person runs `tt sign --login`
- **THEN** the PAR request's `scope` is exactly `atproto repo:dev.tartan.signature?action=create`

#### Scenario: Parallel signing
- **WHEN** two `tt` processes sign at once with an expired access token
- **THEN** the token is refreshed once and both signatures succeed

#### Scenario: Granular scope refused
- **WHEN** the authorization server refuses the `repo:` scope
- **THEN** `tt sign --login` fails, explaining that the person's server does not support the scope, and requests no broader scope

#### Scenario: No forge configured
- **WHEN** a person runs `tt sign --login` with no default forge, no `--forge` and no `TT_FORGE`
- **THEN** it fails with "no forge configured: run `tt login --forge <url>` or pass `--forge`" and starts no authorization

### Requirement: Forges not on public HTTPS
`tt sign --login --client localhost` SHALL authorize with ATProto's `http://localhost` client_id, declaring both loopback redirect URIs and the `atproto` and both `repo:dev.tartan.signature` scope tokens in its query. When the authorization server answers `invalid_client_metadata` or `invalid_redirect_uri` at the first PAR, `tt sign --login` SHALL suggest `--client localhost`.

#### Scenario: VPN-only forge
- **WHEN** a person's forge is reachable only over a VPN and `tt sign --login` is refused with `invalid_client_metadata`
- **THEN** the message suggests `tt sign --login --client localhost`, and that login succeeds where the authorization server allows the localhost client

### Requirement: Agents sign from a persistent host
An agent running anywhere but its person's own machine SHALL sign as its own ATProto account with a session made once by `tt sign --login --no-open --port` on a persistent host. A signing session SHALL NOT be shared between machines; `tt` SHALL name that cause when a refresh is refused. Ephemeral CI without a persistent credential store is not supported.

#### Scenario: Bot account
- **WHEN** an agent on a build host signs as its bot DID and the rule lists that DID
- **THEN** its pushes to the covered ref are compliant

#### Scenario: Copied session
- **WHEN** a signing session copied to a second machine refreshes, and the first machine then signs after its access token expired
- **THEN** the first machine's signing fails, naming sessions shared between machines among the causes and `tt sign --login`

### Requirement: The forge publishes the CLI's client metadata
The forge SHALL serve `<TARTAN_PUBLIC_URL>/-/auth/tt/client-metadata.json` with `client_id` equal to that URL, `application_type` `native`, `token_endpoint_auth_method` `none`, `redirect_uris` `http://127.0.0.1/callback` and `http://[::1]/callback`, `grant_types` `authorization_code` and `refresh_token`, `dpop_bound_access_tokens` true, and `scope` `atproto repo:dev.tartan.signature?action=create repo:dev.tartan.signature?action=delete`.

#### Scenario: Metadata matches its URL
- **WHEN** an authorization server fetches the CLI's `client_id`
- **THEN** it receives `application/json` whose `client_id` equals that URL and that declares both `repo:` tokens verbatim

### Requirement: Revoking a signature
`tt sign revoke <commit-or-at-uri>` SHALL authorize once with exactly `atproto repo:dev.tartan.signature?action=delete`, delete the named record, revoke those tokens at the authorization server, and, when signed in to a forge, withdraw it there if signed in with a CLI session as the record's DID, else ask the forge to re-prove it. A record still in the local queue SHALL instead be removed from the queue with no authorization.

#### Scenario: Compromised session
- **WHEN** a person whose signing session was stolen revokes every app password and app session with repository write access at their PDS, and then revokes a forged signature
- **THEN** the record is deleted, the old session can no longer create records, and the forge stops counting that signature toward its signing rules

#### Scenario: Re-created before the forge looks
- **WHEN** someone holding another write credential re-creates a record with the same CID right after `tt sign revoke` deleted it
- **THEN** the forge still marks the record withdrawn, because the signer's own call needed no proof of absence

#### Scenario: Procedure text
- **WHEN** a person runs `tt sign revoke --help`
- **THEN** it says that any credential with write access to the account's repository can sign, and to revoke every app password and app session with repository write access, then revoke signatures, then run `tt sign --login`

#### Scenario: Revoking with an access token
- **WHEN** a person signed in to the forge only with `tt login --with-token` revokes their own signature
- **THEN** the record is deleted, the forge is asked to re-prove it, and `tt` says that withdrawing on the forge needs `tt login`

#### Scenario: Queued signature
- **WHEN** a person revokes a signature whose record is still queued
- **THEN** the record is removed from the queue and no OAuth flow starts

### Requirement: Re-checking a late signature
`tt sign recheck <commit> --repo <org>/<repo>` SHALL ask the signed-in forge to re-check the commit's `unverifiable` result as its signer, after publishing the commit's record from the queue if it is still there.

#### Scenario: Published after the forge gave up
- **WHEN** a person whose commit's record was queued for eight days runs `tt sign recheck <commit> --repo acme/api`
- **THEN** the record is published, the forge re-checks the commit, and it ends `verified`

### Requirement: The signature lexicon
The repository SHALL publish the `dev.tartan.signature` Lexicon: a record with key `tid` and required `namespace` (string, known value `git`, at most 64 bytes), `algorithm` (string, known value `hmac-sha256`, at most 32 bytes), `digest` (bytes, exactly 32) and `createdAt` (datetime).

#### Scenario: Valid record
- **WHEN** the Lexicon validates a record `tt` wrote
- **THEN** it is valid, and a record without `digest` is not

### Requirement: tt verifies signatures for Git
With `--status-fd=<n> --verify <file> -`, `tt` SHALL prove the record back to its DID as the forge does and check its CID, and its digest against HMAC-SHA256 of the payload keyed with the signature's salt. It SHALL write `[GNUPG:] NEWSIG` first, then `GOODSIG <DID> <name> (<DID>)` (`<name>` the verified `@handle`, else the DID) with a trust line (exit 0), `BADSIG` for a wrong record or digest, or `ERRSIG` when the proof cannot be fetched.

#### Scenario: verify-commit
- **WHEN** a person runs `git verify-commit HEAD` on a Tartan-signed commit
- **THEN** it succeeds and prints that the commit is signed by `@handle` with the DID

#### Scenario: Tampered commit
- **WHEN** a signed commit's message is changed and its signature kept
- **THEN** `git verify-commit` fails with a bad signature

#### Scenario: Record still queued
- **WHEN** a person verifies their own commit whose record is still in the local queue
- **THEN** verification fails with `ERRSIG` and a message saying the record has not been published yet

#### Scenario: Non-canonical Salt or CID encoding
- **WHEN** `tt` verifies a signature whose `Salt` has non-zero trailing bits or whose `CID` is written in another multibase, but which decodes to the bytes of a valid signature
- **THEN** it writes `BADSIG` and exits 1, whether or not the canonical form is in the cache

#### Scenario: Another X.509 signature
- **WHEN** `tt` verifies a `-----BEGIN SIGNED MESSAGE-----` block that is not a Tartan signature, such as gitsign's
- **THEN** it writes `[GNUPG:] NEWSIG` then `[GNUPG:] ERRSIG unsupported`, exits 1, and suggests `git -c gpg.x509.program=gitsign verify-commit`

### Requirement: Trust comes from a local list
`tt` SHALL write `[GNUPG:] TRUST_UNDEFINED 0 tartan` for a proved signature, and `[GNUPG:] TRUST_FULLY 0 tartan` only when the proved DID is listed in `git config --get-all tt.trustedSigner`. Status lines SHALL carry only syntax-checked values without whitespace or newlines.

#### Scenario: Untrusted signer
- **WHEN** `git merge --verify-signatures` merges a commit signed by a proved DID not in `tt.trustedSigner`
- **THEN** Git refuses it as untrusted, while `git verify-commit` on it exits 0

#### Scenario: Trusted signer
- **WHEN** the signer's DID is in `tt.trustedSigner`
- **THEN** `git merge --verify-signatures` and `git -c gpg.minTrustLevel=fully verify-commit` accept the commit

### Requirement: Local verification is cached briefly
`tt` SHALL cache a proved verification, keyed by record URI, CID and HMAC-SHA256(salt, payload), for at most 24 hours under `$XDG_CACHE_HOME/tt`, and SHALL skip the cache when `TT_NO_CACHE=1`. The trust line SHALL never be cached.

#### Scenario: Log with signatures
- **WHEN** a person runs `git log --show-signature` twice within a minute
- **THEN** the second run fetches nothing from the network

#### Scenario: Salt altered after a cached verification
- **WHEN** an object that verified within the last 24 hours is presented again with a different `Salt:` line
- **THEN** the cache misses and `tt` writes `BADSIG`
