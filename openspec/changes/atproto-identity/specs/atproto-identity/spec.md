# Spec Delta

## Purpose

How the forge knows a person: their ATProto DID is the identifier, their handle is only for display, and every fact about a DID that the forge relies on is checked rather than taken from an HTTP response on trust.

## ADDED Requirements

### Requirement: People are identified by DID
The forge SHALL identify a person only by their DID, `did:plc` or `did:web`, and SHALL store, compare and emit DIDs, never handles. A DID of any other method SHALL be refused. A `did:web` DID SHALL name a hostname only, without a path.

#### Scenario: Unsupported method
- **WHEN** a sign-in or an owner entry names `did:key:z6Mk…`
- **THEN** it is refused as an unsupported DID method

#### Scenario: Path-based did:web
- **WHEN** an owner entry names `did:web:example.com:alice`
- **THEN** the deployment configuration is invalid

### Requirement: A did:plc document is derived from its verified operation log
For a `did:plc` DID the forge SHALL fetch the operation log (`/log/audit`) and accept it only when the genesis operation hashes to the DID and each later operation it uses links to the previous one and carries a low-S signature by one of that operation's rotation keys. The DID document SHALL be built from the last such operation.

#### Scenario: Forged log
- **WHEN** the PLC directory returns a log whose last operation is signed by a key that is not a rotation key of the operation before it
- **THEN** the DID does not resolve and nothing that depends on it is trusted

#### Scenario: Tombstoned DID
- **WHEN** the last valid operation is a `plc_tombstone`
- **THEN** the DID resolves as deactivated

### Requirement: A did:web document is fetched from its host
For a `did:web` DID the forge SHALL fetch `https://<host>/.well-known/did.json` and accept it only when its `id` is the DID. The forge SHALL keep the bytes it fetched when they are used as evidence.

#### Scenario: Mismatched id
- **WHEN** `did:web:example.com` serves a document whose `id` is `did:web:other.example`
- **THEN** the DID does not resolve

### Requirement: The signing key and PDS come from the DID document
The forge SHALL take the person's repository signing key from the first `verificationMethod` whose id ends in `#atproto`, of type `Multikey` (or the legacy `EcdsaSecp256r1VerificationKey2019`/`EcdsaSecp256k1VerificationKey2019`), controlled by the DID, with a p256 or k256 key, and the PDS from the first `service` whose id ends in `#atproto_pds` with type `AtprotoPersonalDataServer`. A document without both SHALL not resolve.

#### Scenario: No PDS
- **WHEN** a DID document has an `#atproto` key but no `#atproto_pds` service
- **THEN** the DID does not resolve and sign-in with it fails

### Requirement: Handles are verified in both directions
A handle SHALL resolve to a DID through DNS TXT `_atproto.<handle>` and HTTPS `/.well-known/atproto-did`, preferring DNS when they differ and failing when DNS names several DIDs. A handle SHALL be shown for a DID only when it resolves to that DID and is the first `at://` entry of the DID document's `alsoKnownAs`.

#### Scenario: Handle not claimed back
- **WHEN** `alice.example` resolves to a DID whose document claims `at://bob.example`
- **THEN** the forge shows that DID as `handle.invalid`, not as `alice.example`

### Requirement: Handles are refreshed, DIDs are kept
The forge SHALL keep each known person's last verified handle, re-verify it in the background when it is older than 24 hours and the person is shown, and at once on an identity event. A transient failure SHALL keep the last verified handle; only a handle that no longer resolves to the DID, or that the document no longer claims, SHALL become `handle.invalid`. A handle change SHALL record `person.handle.changed` and SHALL NOT change any stored DID.

#### Scenario: Handle changes
- **WHEN** a person changes their handle and the forge receives their identity event
- **THEN** `person.handle.changed` is recorded and pages show the new handle for the same DID

#### Scenario: DNS unreachable
- **WHEN** a stale handle's re-verification fails because DNS cannot be reached
- **THEN** the last verified handle is still shown

### Requirement: People are shown by handle
`GET /-/api/people?did=…` (up to 100 DIDs) SHALL answer from the forge's cache without waiting on the network: each DID's last verified handle, `handle.invalid`, or `null` when none is known yet, scheduling lookups in the background. The web UI SHALL show `@handle`, or a shortened DID while the handle is `null`, with the DID on hover; a `tartan-ui@1` node `{"t":"person","did":…}` SHALL render the same way.

#### Scenario: Extension view
- **WHEN** an extension renders `{"t":"person","did":"did:plc:…"}` for a person whose handle is `alice.example`
- **THEN** the page shows `@alice.example`

#### Scenario: Unknown DID
- **WHEN** a page shows an issue assignee whose DID the forge has never resolved
- **THEN** a shortened DID such as `did:plc:abcd…wxyz` is shown and a lookup is started

#### Scenario: Invalid DID in a node
- **WHEN** a `person` node's `did` is not a DID
- **THEN** the host refuses the document and shows its error chip

### Requirement: Identity fetches are bounded and public
Every identity, OAuth and PDS fetch SHALL use HTTPS to a public address, ignore proxy environment variables, time out after 10 seconds and refuse bodies over 1 MiB (64 KiB for DID documents and metadata). Before each request and redirect hop an IP-literal host SHALL pass the public-address check, and DNS answers SHALL be checked the same way. Loopback, private, link-local, CGNAT, ULA, multicast, unspecified, IPv4-mapped, NAT64 and documentation ranges are not public.

#### Scenario: PDS on a private address
- **WHEN** a DID document names `https://10.0.0.5` as its PDS
- **THEN** the forge refuses to contact it and the DID does not resolve

#### Scenario: IP literals
- **WHEN** a URL to fetch is `http://127.0.0.1`, `https://[::ffff:10.0.0.5]` or `https://169.254.169.254`
- **THEN** the forge refuses it without connecting

#### Scenario: Proxy in the environment
- **WHEN** `HTTPS_PROXY` is set in the forge's environment
- **THEN** identity fetches still connect directly and apply the address check

### Requirement: A development identity network is opt-in
The forge SHALL accept plain HTTP, private addresses, a configured PLC directory and handle resolver only when built with `dev-identity` and started with `TARTAN_DEV_IDENTITY=1`. Then `TARTAN_PLC_URL` and `TARTAN_DEV_HANDLE_RESOLVER` SHALL be required, and `TARTAN_JETSTREAM_URL` SHALL have no default and be set or empty; otherwise the forge SHALL not start, naming them. A build without the feature SHALL refuse to start when `TARTAN_DEV_IDENTITY` is set.

#### Scenario: Release build
- **WHEN** a release build starts with `TARTAN_DEV_IDENTITY=1`
- **THEN** it exits with an error naming the variable

#### Scenario: Development forge
- **WHEN** a `dev-identity` build starts with `TARTAN_DEV_IDENTITY=1` and `TARTAN_PLC_URL` pointing at `tartan-devnet`
- **THEN** people of the development network can sign in and sign commits without the public network

#### Scenario: Development forge without a directory
- **WHEN** a `dev-identity` build starts with `TARTAN_DEV_IDENTITY=1` and no `TARTAN_PLC_URL`
- **THEN** it exits with an error naming `TARTAN_PLC_URL`, without contacting the public directory
