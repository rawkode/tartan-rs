# Spec Delta

## Purpose

`tt`, the forge's command-line interface: signing in to a forge from a terminal, keeping its credentials, managing access tokens, resolving identities, and the operator commands that work without anyone signed in. Signing Git objects with `tt sign` is its own capability, `tt-sign`.

## ADDED Requirements

### Requirement: One command-line interface
The workspace SHALL build a `tt` binary (crate `crates/tt`, AGPL-3.0-or-later) as the forge's only command-line interface: user commands that talk to a forge's HTTP API, `tt sign` for Git signatures, and `tt admin` for operator commands. `tt` SHALL NOT depend on `tartan-forge`, wasmtime, `tartan-config` or `cuengine`. The `tartan` server binary SHALL take no subcommands.

#### Scenario: Command list
- **WHEN** someone runs `tt --help`
- **THEN** it lists `login`, `logout`, `status`, `token`, `identity`, `sign` and `admin`

#### Scenario: Server binary
- **WHEN** an operator runs `tartan token create`
- **THEN** it exits with an error naming `tt admin token create`

### Requirement: Signing in to a forge from the terminal
`tt login [--forge <url>]` SHALL listen on a loopback port, open the forge's CLI approval page with a random `state` and a PKCE S256 challenge, and, on the redirect back, exchange the one-time code and its verifier at `POST /-/auth/cli/token` for a CLI session secret (`tartan_cli_` and 43 base64url characters). Codes SHALL be single-use and expire after 60 seconds.

#### Scenario: Approve in the browser
- **WHEN** a signed-in owner runs `tt login --no-signing --forge https://forge.example` and approves the request
- **THEN** `tt status` shows their handle and DID on that forge

#### Scenario: Code replayed
- **WHEN** a CLI code that was already exchanged is sent to `POST /-/auth/cli/token` again
- **THEN** it is refused and no session is created

#### Scenario: Wrong verifier
- **WHEN** a CLI code is exchanged with a verifier whose S256 hash is not the stored challenge
- **THEN** it is refused, the code is consumed, and no session is created

### Requirement: The CLI approval page
The web UI SHALL show the approval at `/#/-/cli-login`, naming the person by handle and the device name `tt` sent, sending a person who is not signed in to sign in first and back. Approval SHALL be `POST /-/auth/cli/approve` from a session, Origin-checked, and SHALL redirect only to `http://127.0.0.1:<port>` on the requested port.

#### Scenario: Not signed in
- **WHEN** a person opens the approval link without a forge session
- **THEN** they are sent to sign in and returned to the approval page afterwards

#### Scenario: Non-loopback target
- **WHEN** an approval asks to redirect to `http://evil.example:8080`
- **THEN** the forge refuses it and issues no code

#### Scenario: Cross-site approval
- **WHEN** another site posts to `/-/auth/cli/approve` with the person's cookie
- **THEN** the forge answers 403 and issues no code

### Requirement: CLI sessions are sessions
A CLI session SHALL act as its person with the rights of a browser session, SHALL be sent as a bearer token and so need no `Origin`, SHALL expire 30 days after creation or 7 days after its last use, SHALL appear in `/-/api/sessions` with its device name, and SHALL end with `tt logout` or from the sessions page. It SHALL be allowed to manage access tokens.

#### Scenario: Token from the CLI
- **WHEN** a person signed in with `tt login` runs `tt token create --name ci --scopes read --expires-in-days 30`
- **THEN** the token is created for their DID and its secret is printed once

#### Scenario: Ended from the browser
- **WHEN** a person ends their CLI session on the sessions page
- **THEN** the next `tt` request with it is refused with 401 and `tt` says to run `tt login`

### Requirement: First run signs in for signing too
Once the CLI session is stored, unless `--no-signing` is given or a signing session for its DID exists, `tt login` SHALL run `tt sign --login` for that DID with the same `--forge`, `--no-open`, `--port` and `--client`, sending the DID as `login_hint`, requiring `sub` to equal it, and making it the default signing DID only if none is set or `--set-default` is given. If that fails or is cancelled, the CLI session SHALL be kept, `tt` SHALL name `tt sign --login` and exit non-zero.

#### Scenario: First run
- **WHEN** a person on a fresh machine runs `tt login`, approves the forge request and consents at their authorization server
- **THEN** `tt status` shows the forge session and a signing session for the same DID, and that DID is the default signing DID

#### Scenario: Signing consent cancelled
- **WHEN** a person approves the forge request but cancels the consent at their authorization server
- **THEN** the forge CLI session is kept, `tt` says signing is not set up and to run `tt sign --login`, and it exits non-zero

#### Scenario: No signing
- **WHEN** a person runs `tt login --no-signing`
- **THEN** a CLI session is created and no PDS authorization is started

#### Scenario: Signing session already present
- **WHEN** a person who already has a signing session for their DID runs `tt login`
- **THEN** only the forge CLI session is created

#### Scenario: Token login never signs in for signing
- **WHEN** a CI job runs `tt login --with-token`
- **THEN** no PDS authorization is started

### Requirement: Defaults change only when asked
`tt` SHALL write the default forge and default signing DID in `$XDG_CONFIG_HOME/tt/config.toml` only when unset, or with `--set-default`: `tt login` the default forge, `tt sign --login` (and `tt login`'s first run) the default signing DID. `tt sign --login` SHALL never change the default forge, and no other command SHALL write either default. `tt login` with no `--forge`, `TT_FORGE` or default forge SHALL fail before any request, naming `--forge`.

#### Scenario: Second forge does not replace the default
- **WHEN** a person whose default forge is `https://forge.example` runs `tt login --forge https://other.example`
- **THEN** they are signed in to both, the default forge is still `https://forge.example`, and `tt` names `--set-default`

#### Scenario: Signing login for another forge
- **WHEN** a person with a default forge and a default signing DID runs `tt sign --login --forge <another URL> --handle <another handle>`
- **THEN** a signing session for the other DID is stored and neither default changes

#### Scenario: Choosing the default explicitly
- **WHEN** a person runs `tt login --forge https://other.example --set-default`
- **THEN** `https://other.example` becomes the default forge

#### Scenario: Login with no forge
- **WHEN** a person on a fresh machine runs `tt login` with no `--forge` and no `TT_FORGE`
- **THEN** it fails before any request with a message naming `--forge`

### Requirement: The demo and tests keep tt's state to themselves
`scripts/demo.sh` and every test that runs `tt` SHALL give each person or test its own `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME`, under the demo's state directory or the test's temporary directory, with `TT_CREDENTIAL_STORE=file`, and SHALL configure Git signing only with `git -c`, so they never read or change the developer's `tt` defaults, credentials, queue, cache or Git configuration.

#### Scenario: Demo on a developer's machine
- **WHEN** `scripts/demo.sh` runs with `HOME` set to a fresh directory
- **THEN** afterwards `$HOME/.config/tt`, `$HOME/.local/state/tt`, `$HOME/.cache/tt` and `$HOME/.gitconfig` do not exist

### Requirement: Signing in with a token
`tt login --with-token` SHALL read a `tartan_pat_` token from standard input and act with that token's rights. `tt token` commands SHALL then be refused locally, with a message, because tokens cannot manage tokens. It SHALL NOT start a signing login.

#### Scenario: Remote machine
- **WHEN** a CI job pipes a read token into `tt login --with-token --forge https://forge.example`
- **THEN** `tt status` shows the token's principal and `tt token create` exits with an error saying a session is needed

### Requirement: Login from a remote shell
`tt login` and `tt sign --login` SHALL accept `--no-open`, printing the URL to open instead of opening a browser, and `--port <n>`, fixing the loopback port so it can be forwarded over SSH.

#### Scenario: Forwarded port
- **WHEN** a person runs `tt login --no-open --port 8765` on a server and forwards local port 8765 to it
- **THEN** opening the printed URL on their laptop completes the login on the server, and the signing login that follows reuses port 8765 and prints its own URL

### Requirement: Credentials are kept private
`tt` SHALL keep forge secrets, OAuth refresh tokens and DPoP private keys in the operating system's credential store, or, where none is available or `TT_CREDENTIAL_STORE=file`, in files under `$XDG_STATE_HOME/tt` with mode 0600. It SHALL never write them to Git configuration or its own configuration file, and SHALL refuse a credential file readable by group or others.

#### Scenario: Headless Linux
- **WHEN** `tt login` runs where no Secret Service is reachable
- **THEN** the credentials are written to a mode 0600 file and `tt` says where

#### Scenario: Loosened permissions
- **WHEN** a credential file's mode is changed to 0644
- **THEN** `tt` refuses to use it and names the file

### Requirement: Managing access tokens
`tt token create --name <n> --scopes <list> [--repos <list>] --expires-in-days <d>`, `tt token list` and `tt token revoke <id>` SHALL call `/-/api/tokens` with the CLI session and SHALL print a created token's secret once.

#### Scenario: Revoke
- **WHEN** a person runs `tt token revoke <id>` for one of their tokens
- **THEN** the token is revoked and `tt token list` shows it revoked

### Requirement: Signing out
`tt logout [--forge <url>] [--did <DID>]` SHALL end the CLI session at the forge when a forge is configured or named, revoke every stored signing OAuth session at its authorization server (only that DID's with `--did`) whether or not a forge is configured, and delete the stored credentials, even when the forge cannot be reached. It SHALL say the local credentials are deleted only when it deleted some. Its help SHALL point to the compromise procedure, which does not start with `tt logout`.

#### Scenario: Forge down
- **WHEN** a person runs `tt logout` while the forge is unreachable
- **THEN** the local credentials are deleted and `tt` says the forge session could not be ended

#### Scenario: Signing session without a forge
- **WHEN** a person who ran only `tt sign --login --client localhost` runs `tt logout`
- **THEN** the signing session is revoked at its authorization server, its stored credential is deleted, and `tt status` lists no signing session

### Requirement: Operator commands
`tt admin token create|list|revoke --did <DID>` SHALL reach the identity actor keyed by `TARTAN_FORGE_ID` (default `forge`) through Rivet (`RIVET_ENDPOINT`, `RIVET_TOKEN`, `RIVET_NAMESPACE`), read no deployment configuration, and need neither a PDS nor a signed-in person. The identity actor SHALL refuse a DID not in the forge's `owners`, and `tt admin` SHALL report that refusal. `tt admin oauth keygen` SHALL print a fresh ES256 private JWKS.

#### Scenario: Seeding the demo
- **WHEN** `scripts/demo.sh` runs `tt admin token create` for the development owner
- **THEN** its seeding requests succeed with that token

#### Scenario: Non-default forge id
- **WHEN** an operator runs `tt admin token create` with `TARTAN_FORGE_ID=acme`, against a forge started with the same id
- **THEN** the token is created in that forge's identity actor and the gateway accepts it

#### Scenario: Not an owner
- **WHEN** an operator runs `tt admin token create --did` with a DID not in the running forge's `owners`
- **THEN** the identity actor refuses it, and `tt admin` exits with an error naming the DID and creates nothing

### Requirement: Resolving identities
`tt identity resolve <handle-or-DID>` SHALL print the DID, the verified handle and the PDS, verified as the forge verifies them, without a forge, owners, OAuth keys or Rivet.

#### Scenario: Finding an owner DID
- **WHEN** an operator runs `tt identity resolve alice.example`
- **THEN** it prints alice's DID, which can be pasted into `owners`
