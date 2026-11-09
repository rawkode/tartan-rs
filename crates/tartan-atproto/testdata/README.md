# tartan-atproto test data

Public ATProto data captured on 2026-11-09, used as test vectors. Nothing here is secret.

## `plc/`: audit logs from `https://plc.directory/<did>/log/audit`

| File | DID | Why |
| --- | --- | --- |
| `atproto-com.json` | `did:plc:ewvi7nxzyoun6zhxrhs64oiz` | a modern log (`plc_operation` genesis, two updates) |
| `legacy-create-and-rotation.json` | `did:plc:ragtjsm2j2vknwkz3zp4oxrd` | a 2022 legacy `create` genesis, and a rotation-key change in later operations |
| `nullified.json` | `did:plc:2s2mvm52ttz6r4hocmrq7x27` | one nullified operation |
| `nullified-nontrivial.json` | `did:plc:ana7n2ppeqk57pay7azhjs24` | five nullified operations across two forks |
| `nullified-tombstone.json` | `did:plc:7iqnimg5hvigsckpbqrqktoh` | a nullified `plc_tombstone` |
| `tombstone.json` | `did:plc:6adr3q2labdllanslzhqkqd3` | a tombstoned DID |

The nullification and tombstone DIDs were registered on plc.directory as test cases by the
[go-didplc](https://github.com/did-method-plc/go-didplc) project (its `testdata/`); these files
are the directory's own answers for them.

## `pds/`: answers from PDSes

| File | Request |
| --- | --- |
| `germ-declaration.getRecord.json` | `com.atproto.repo.getRecord` for `at://did:plc:oephuov4bswzkdwpkydie7we/com.germnetwork.declaration/self` on `cortinarius.us-west.host.bsky.network` (the reference PDS): a record with `$bytes` values and the CID the PDS reports |
| `rawkode-profile.getRecord.json` | `com.atproto.repo.getRecord` for `at://did:web:rawkode.dev/app.bsky.actor.profile/self` on `https://rawkode.dev` |
| `rawkode-profile.sync.getRecord.car` | `com.atproto.sync.getRecord` for the same record: a presence proof |
| `rawkode-absent-signature.sync.getRecord.car` | `com.atproto.sync.getRecord` for `academy.rawkode.tartan.signature/3lbmxyz2abc2k`, which does not exist: a 200 answer with a non-existence proof |

The PDS at `https://rawkode.dev` is not the reference implementation: it stores a blob's `ref`
as the literal map `{"$link": "…"}` instead of a CID link (see `ipld.rs`).

## `web/`

`rawkode.dev.did.json` is `https://rawkode.dev/.well-known/did.json`, the document of
`did:web:rawkode.dev`.
