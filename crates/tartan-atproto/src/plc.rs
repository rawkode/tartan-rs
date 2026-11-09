//! Verifying a `did:plc` audit log (design D9, *did:plc*).
//!
//! The PLC directory is not trusted: the log it serves is replayed and every operation the DID
//! document rests on is checked.
//!
//! 1. The log is at most [`MAX_LOG_BYTES`] and [`MAX_LOG_ENTRIES`] entries, and every entry
//!    names the DID.
//! 2. Of the entries the directory has not nullified, in order, the first is a genesis operation
//!    (`prev` null, type `plc_operation` or the 2022-era `create`) whose DAG-CBOR hashes to the
//!    DID: `did:plc:` + the first 24 characters of base32(sha256(signed operation)).
//! 3. Each operation's CID is recomputed and must equal the entry's `cid`; its `prev` must be
//!    the CID of the operation accepted before it; its `sig` must be unpadded base64url in its
//!    one canonical spelling and a low-S signature, over the operation without `sig`, by a
//!    rotation key of the previous operation (the genesis operation's own keys for the genesis).
//! 4. The DID document comes from the last operation; a `plc_tombstone` there leaves none.
//!
//! The directory's choice among forks (which entries are nullified) is accepted: it is limited
//! to operations signed by legitimate rotation keys within 72 hours, and the whole log is kept
//! as evidence so an auditor can judge it again.
//!
//! A legacy `create` genesis is normalised as did-method-plc's `normalizeOp` does: its rotation
//! keys are `[recoveryKey, signingKey]`, its `#atproto` key is `signingKey`, its handle is
//! `handle` (with `at://` added) and its PDS is `service` (with `https://` added when it has no
//! scheme). The DID hash is still taken over the original legacy operation.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cid::multibase;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::Mode;
use crate::did::{Did, DidMethod};
use crate::document::{AtprotoIdentity, normalise_pds_endpoint};
use crate::ipld::{cid_for_dag_cbor, json_to_dag_cbor};
use crate::keys::AtprotoKey;

/// The largest audit log accepted.
pub const MAX_LOG_BYTES: usize = 1024 * 1024;
/// The most entries an audit log may have.
pub const MAX_LOG_ENTRIES: usize = 1000;

/// What a verified audit log says about a DID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlcState {
    /// The DID is active with this document.
    Active(AtprotoIdentity),
    /// The last valid operation is a `plc_tombstone`: no document, and the account status is
    /// `deleted` (design D10).
    Tombstoned,
}

/// A verified audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlcEvidence {
    /// The CID of the last accepted operation.
    pub operation_cid: String,
    /// The state the log ends in.
    pub state: PlcState,
}

/// Why an audit log was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlcError {
    /// Not a `did:plc` DID.
    #[error("not a did:plc DID")]
    NotPlc,
    /// Larger than [`MAX_LOG_BYTES`] or longer than [`MAX_LOG_ENTRIES`].
    #[error("the audit log is too large")]
    TooLarge,
    /// Not a JSON array of audit entries.
    #[error("the audit log is malformed: {0}")]
    Malformed(String),
    /// An entry names another DID.
    #[error("an audit entry names another DID")]
    WrongDid,
    /// No operation is left after removing nullified entries.
    #[error("the audit log has no valid operation")]
    Empty,
    /// The first operation is not a genesis operation.
    #[error("the first operation is not a genesis operation")]
    NotGenesis,
    /// The genesis operation does not hash to the DID.
    #[error("the genesis operation does not hash to the DID")]
    GenesisHash,
    /// An entry's `cid` is not the operation's CID.
    #[error("operation {0}: the CID does not match the operation")]
    CidMismatch(usize),
    /// An operation's `prev` is not the previous accepted operation's CID.
    #[error("operation {0}: prev does not link to the previous operation")]
    BrokenChain(usize),
    /// An operation's `sig` is not canonical unpadded base64url.
    #[error("operation {0}: the signature is not canonical base64url")]
    SignatureEncoding(usize),
    /// No rotation key of the previous operation signed this one (or the signature is high-S).
    #[error("operation {0}: not signed by a rotation key of the previous operation")]
    BadSignature(usize),
    /// An operation of an unknown type, or an operation after a tombstone.
    #[error("operation {0}: unexpected operation")]
    UnexpectedOperation(usize),
}

#[derive(Deserialize)]
struct Entry {
    did: String,
    operation: Value,
    cid: String,
    #[serde(default)]
    nullified: bool,
}

/// Verifies `log` (the bytes of `GET {plc}/{did}/log/audit`) for `did`.
pub fn verify_audit_log(did: &Did, log: &[u8], mode: Mode) -> Result<PlcEvidence, PlcError> {
    if did.method() != DidMethod::Plc {
        return Err(PlcError::NotPlc);
    }
    if log.len() > MAX_LOG_BYTES {
        return Err(PlcError::TooLarge);
    }
    let entries: Vec<Entry> =
        serde_json::from_slice(log).map_err(|error| PlcError::Malformed(error.to_string()))?;
    if entries.len() > MAX_LOG_ENTRIES {
        return Err(PlcError::TooLarge);
    }
    if entries.iter().any(|entry| entry.did != did.as_str()) {
        return Err(PlcError::WrongDid);
    }

    let mut previous: Option<(String, Operation)> = None;
    for (index, entry) in entries.iter().enumerate().filter(|(_, e)| !e.nullified) {
        let object = entry
            .operation
            .as_object()
            .ok_or_else(|| PlcError::Malformed(format!("operation {index} is not an object")))?;
        let operation = Operation::parse(object, index)?;

        let signed = json_to_dag_cbor(&entry.operation)
            .map_err(|error| PlcError::Malformed(error.to_string()))?;
        let cid = cid_for_dag_cbor(&signed).to_string();
        if cid != entry.cid {
            return Err(PlcError::CidMismatch(index));
        }

        let rotation_keys = match &previous {
            None => {
                if operation.prev.is_some() || !operation.can_be_genesis() {
                    return Err(PlcError::NotGenesis);
                }
                let hash = Sha256::digest(&signed);
                let encoded = multibase::Base::Base32Lower.encode(hash);
                if format!("did:plc:{}", &encoded[..24]) != did.as_str() {
                    return Err(PlcError::GenesisHash);
                }
                operation.rotation_keys()
            }
            Some((previous_cid, previous_operation)) => {
                if matches!(previous_operation.kind, Kind::Tombstone)
                    || matches!(operation.kind, Kind::Create { .. })
                {
                    return Err(PlcError::UnexpectedOperation(index));
                }
                if operation.prev.as_deref() != Some(previous_cid.as_str()) {
                    return Err(PlcError::BrokenChain(index));
                }
                previous_operation.rotation_keys()
            }
        };

        let signature =
            decode_signature(&operation.sig).ok_or(PlcError::SignatureEncoding(index))?;
        let mut unsigned = object.clone();
        unsigned.remove("sig");
        let unsigned = json_to_dag_cbor(&Value::Object(unsigned))
            .map_err(|error| PlcError::Malformed(error.to_string()))?;
        let signed_by_rotation_key = rotation_keys.iter().any(|key| {
            AtprotoKey::from_did_key(key).is_ok_and(|key| key.verify(&unsigned, &signature).is_ok())
        });
        if !signed_by_rotation_key {
            return Err(PlcError::BadSignature(index));
        }
        previous = Some((cid, operation));
    }

    let (operation_cid, last) = previous.ok_or(PlcError::Empty)?;
    let state = match last.kind {
        Kind::Tombstone => PlcState::Tombstoned,
        _ => PlcState::Active(last.identity(did, mode)),
    };
    Ok(PlcEvidence {
        operation_cid,
        state,
    })
}

/// Decodes a PLC `sig`: unpadded base64url whose re-encoding is identical, so trailing bits are
/// zero and no other spelling of the same bytes is accepted.
fn decode_signature(text: &str) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    (URL_SAFE_NO_PAD.encode(&bytes) == text).then_some(bytes)
}

struct Operation {
    prev: Option<String>,
    sig: String,
    kind: Kind,
}

enum Kind {
    Operation {
        rotation_keys: Vec<String>,
        atproto_key: Option<String>,
        also_known_as: Vec<String>,
        pds: Option<(String, String)>,
    },
    Create {
        signing_key: String,
        recovery_key: String,
        handle: String,
        service: String,
    },
    Tombstone,
}

impl Operation {
    fn parse(object: &Map<String, Value>, index: usize) -> Result<Self, PlcError> {
        let malformed = |what: &str| PlcError::Malformed(format!("operation {index}: {what}"));
        let string = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
        let prev = match object.get("prev") {
            None | Some(Value::Null) => None,
            Some(Value::String(prev)) => Some(prev.clone()),
            Some(_) => return Err(malformed("prev is not a string")),
        };
        let sig = string("sig").ok_or_else(|| malformed("missing sig"))?;
        let kind = match object.get("type").and_then(Value::as_str) {
            Some("plc_operation") => {
                let strings = |key: &str| -> Result<Vec<String>, PlcError> {
                    object
                        .get(key)
                        .and_then(Value::as_array)
                        .ok_or_else(|| malformed(key))?
                        .iter()
                        .map(|v| v.as_str().map(str::to_owned).ok_or_else(|| malformed(key)))
                        .collect()
                };
                let rotation_keys = strings("rotationKeys")?;
                let also_known_as = strings("alsoKnownAs")?;
                let methods = object
                    .get("verificationMethods")
                    .and_then(Value::as_object)
                    .ok_or_else(|| malformed("verificationMethods"))?;
                let services = object
                    .get("services")
                    .and_then(Value::as_object)
                    .ok_or_else(|| malformed("services"))?;
                let atproto_key = methods
                    .get("atproto")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let pds = services.get("atproto_pds").and_then(|service| {
                    Some((
                        service.get("type")?.as_str()?.to_owned(),
                        service.get("endpoint")?.as_str()?.to_owned(),
                    ))
                });
                Kind::Operation {
                    rotation_keys,
                    atproto_key,
                    also_known_as,
                    pds,
                }
            }
            Some("create") => Kind::Create {
                signing_key: string("signingKey").ok_or_else(|| malformed("signingKey"))?,
                recovery_key: string("recoveryKey").ok_or_else(|| malformed("recoveryKey"))?,
                handle: string("handle").ok_or_else(|| malformed("handle"))?,
                service: string("service").ok_or_else(|| malformed("service"))?,
            },
            Some("plc_tombstone") => Kind::Tombstone,
            _ => return Err(PlcError::UnexpectedOperation(index)),
        };
        Ok(Operation { prev, sig, kind })
    }

    fn can_be_genesis(&self) -> bool {
        !matches!(self.kind, Kind::Tombstone)
    }

    fn rotation_keys(&self) -> Vec<String> {
        match &self.kind {
            Kind::Operation { rotation_keys, .. } => rotation_keys.clone(),
            Kind::Create {
                signing_key,
                recovery_key,
                ..
            } => vec![recovery_key.clone(), signing_key.clone()],
            Kind::Tombstone => Vec::new(),
        }
    }

    fn identity(&self, did: &Did, mode: Mode) -> AtprotoIdentity {
        match &self.kind {
            Kind::Operation {
                atproto_key,
                also_known_as,
                pds,
                ..
            } => AtprotoIdentity {
                did: did.clone(),
                handle: also_known_as.iter().find_map(|aka| handle_claim(aka)),
                pds: pds
                    .as_ref()
                    .filter(|(kind, _)| kind == "AtprotoPersonalDataServer")
                    .and_then(|(_, endpoint)| normalise_pds_endpoint(endpoint, mode)),
                signing_key: atproto_key
                    .as_deref()
                    .and_then(|key| AtprotoKey::from_did_key(key).ok()),
            },
            Kind::Create {
                signing_key,
                handle,
                service,
                ..
            } => {
                let service = if service.contains("://") {
                    service.clone()
                } else {
                    format!("https://{service}")
                };
                let handle = if handle.starts_with("at://") {
                    handle.clone()
                } else {
                    format!("at://{handle}")
                };
                AtprotoIdentity {
                    did: did.clone(),
                    handle: handle_claim(&handle),
                    pds: normalise_pds_endpoint(&service, mode),
                    signing_key: AtprotoKey::from_did_key(signing_key).ok(),
                }
            }
            Kind::Tombstone => AtprotoIdentity {
                did: did.clone(),
                handle: None,
                pds: None,
                signing_key: None,
            },
        }
    }
}

/// The handle an `alsoKnownAs` entry claims, lowercased, if it is an `at://` entry.
pub(crate) fn handle_claim(also_known_as: &str) -> Option<String> {
    also_known_as
        .strip_prefix("at://")
        .filter(|handle| !handle.is_empty() && !handle.contains('/'))
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/testdata/plc/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).expect(&path)
    }

    fn did_of(log: &[u8]) -> Did {
        let entries: Vec<Value> = serde_json::from_slice(log).expect("json");
        let did = entries[0]["did"].as_str().expect("did");
        Did::parse(did, Mode::Production).expect("did")
    }

    fn verify(log: &[u8]) -> Result<PlcEvidence, PlcError> {
        verify_audit_log(&did_of(log), log, Mode::Production)
    }

    fn active(evidence: &PlcEvidence) -> &AtprotoIdentity {
        match &evidence.state {
            PlcState::Active(identity) => identity,
            PlcState::Tombstoned => panic!("tombstoned"),
        }
    }

    fn tamper(log: &[u8], edit: impl FnOnce(&mut Vec<Value>)) -> Vec<u8> {
        let mut entries: Vec<Value> = serde_json::from_slice(log).expect("json");
        edit(&mut entries);
        serde_json::to_vec(&entries).expect("json")
    }

    #[test]
    fn modern_log() {
        let log = fixture("atproto-com.json");
        let evidence = verify(&log).expect("valid");
        let identity = active(&evidence);
        assert_eq!(identity.handle.as_deref(), Some("atproto.com"));
        assert!(identity.signing_key.is_some());
        assert!(identity.pds.is_some());
    }

    #[test]
    fn legacy_create_with_rotation() {
        let log = fixture("legacy-create-and-rotation.json");
        let evidence = verify(&log).expect("valid");
        let entries: Vec<Value> = serde_json::from_slice(&log).expect("json");
        assert_eq!(entries[0]["operation"]["type"], "create");
        assert_eq!(
            evidence.operation_cid,
            entries.last().expect("entries")["cid"]
                .as_str()
                .expect("cid")
        );
        let identity = active(&evidence);
        assert!(identity.signing_key.is_some());
        assert!(
            identity
                .pds
                .as_deref()
                .is_some_and(|pds| pds.starts_with("https://"))
        );
    }

    #[test]
    fn legacy_genesis_alone_is_normalised() {
        let log = fixture("legacy-create-and-rotation.json");
        let genesis_only = tamper(&log, |entries| entries.truncate(1));
        let evidence = verify(&genesis_only).expect("valid");
        let identity = active(&evidence);
        assert_eq!(identity.handle.as_deref(), Some("paul.bsky.social"));
        assert_eq!(identity.pds.as_deref(), Some("https://bsky.social"));
        assert_eq!(
            identity
                .signing_key
                .as_ref()
                .map(AtprotoKey::did_key)
                .as_deref(),
            Some("did:key:zQ3shP5TBe1sQfSttXty15FAEHV1DZgcxRZNxvEWnPfLFwLxJ")
        );
    }

    #[test]
    fn nullified_operations_are_skipped() {
        for name in [
            "nullified.json",
            "nullified-nontrivial.json",
            "nullified-tombstone.json",
        ] {
            let log = fixture(name);
            let evidence = verify(&log).expect(name);
            assert!(matches!(evidence.state, PlcState::Active(_)), "{name}");
        }
    }

    #[test]
    fn nullified_entry_cannot_be_revived() {
        // Clearing the directory's `nullified` flag yields two operations with the same prev.
        let log = fixture("nullified.json");
        let revived = tamper(&log, |entries| {
            for entry in entries.iter_mut() {
                entry["nullified"] = Value::Bool(false);
            }
        });
        assert!(matches!(verify(&revived), Err(PlcError::BrokenChain(_))));
    }

    #[test]
    fn tombstone() {
        let evidence = verify(&fixture("tombstone.json")).expect("valid");
        assert_eq!(evidence.state, PlcState::Tombstoned);
    }

    #[test]
    fn tampered_logs_fail() {
        let log = fixture("legacy-create-and-rotation.json");
        let last = |entries: &mut Vec<Value>| entries.len() - 1;

        // A changed field in the last operation changes its CID.
        let edited = tamper(&log, |entries| {
            let i = last(entries);
            entries[i]["operation"]["alsoKnownAs"] = serde_json::json!(["at://evil.example"]);
        });
        assert!(matches!(verify(&edited), Err(PlcError::CidMismatch(_))));

        // The same change with the CID recomputed: the signature no longer verifies.
        let recomputed = tamper(&log, |entries| {
            let i = last(entries);
            entries[i]["operation"]["alsoKnownAs"] = serde_json::json!(["at://evil.example"]);
            let cbor = json_to_dag_cbor(&entries[i]["operation"]).expect("cbor");
            entries[i]["cid"] = Value::String(cid_for_dag_cbor(&cbor).to_string());
        });
        assert!(matches!(
            verify(&recomputed),
            Err(PlcError::BadSignature(_))
        ));

        // An operation dropped from the middle breaks the chain.
        let dropped = tamper(&log, |entries| {
            entries.remove(2);
        });
        assert!(matches!(verify(&dropped), Err(PlcError::BrokenChain(_))));

        // A genesis operation that is not this DID's.
        let other = fixture("atproto-com.json");
        let swapped = tamper(&other, |entries| {
            for entry in entries.iter_mut() {
                entry["did"] = Value::String("did:plc:ragtjsm2j2vknwkz3zp4oxrd".into());
            }
        });
        let did = Did::parse("did:plc:ragtjsm2j2vknwkz3zp4oxrd", Mode::Production).expect("did");
        assert_eq!(
            verify_audit_log(&did, &swapped, Mode::Production),
            Err(PlcError::GenesisHash)
        );

        // An entry for another DID.
        let mixed = tamper(&log, |entries| {
            entries[1]["did"] = Value::String("did:plc:ewvi7nxzyoun6zhxrhs64oiz".into());
        });
        assert_eq!(verify(&mixed), Err(PlcError::WrongDid));

        // The genesis operation removed.
        let headless = tamper(&log, |entries| {
            entries.remove(0);
        });
        assert_eq!(verify(&headless), Err(PlcError::NotGenesis));

        // Everything nullified.
        let empty = tamper(&log, |entries| {
            for entry in entries.iter_mut() {
                entry["nullified"] = Value::Bool(true);
            }
        });
        assert_eq!(verify(&empty), Err(PlcError::Empty));
    }

    #[test]
    fn signature_must_be_canonical_and_low_s() {
        let log = fixture("atproto-com.json");
        let entries: Vec<Value> = serde_json::from_slice(&log).expect("json");
        let i = entries.len() - 1;
        let sig = entries[i]["operation"]["sig"]
            .as_str()
            .expect("sig")
            .to_owned();
        let bytes = URL_SAFE_NO_PAD.decode(&sig).expect("base64url");

        // Padding characters, and a spelling with non-zero trailing bits.
        let padded = format!("{sig}==");
        let mut trailing = sig.clone();
        let last_char = trailing.pop().expect("char");
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let position = alphabet
            .iter()
            .position(|&c| c == last_char as u8)
            .expect("alphabet");
        trailing.push(alphabet[position | 1] as char);
        assert_ne!(trailing, sig);

        // The high-S twin of the signature.
        let low = k256::ecdsa::Signature::from_slice(&bytes).expect("signature");
        let (r, s) = low.split_scalars();
        let high = k256::ecdsa::Signature::from_scalars(r, -*s).expect("high-S");
        let high = URL_SAFE_NO_PAD.encode(high.to_bytes());

        for (replacement, expected) in [
            (padded, PlcError::SignatureEncoding(i)),
            (trailing, PlcError::SignatureEncoding(i)),
            (high, PlcError::BadSignature(i)),
        ] {
            let changed = tamper(&log, |entries| {
                entries[i]["operation"]["sig"] = Value::String(replacement.clone());
                let cbor = json_to_dag_cbor(&entries[i]["operation"]).expect("cbor");
                entries[i]["cid"] = Value::String(cid_for_dag_cbor(&cbor).to_string());
            });
            assert_eq!(verify(&changed), Err(expected));
        }
    }

    #[test]
    fn size_caps() {
        let did = Did::parse("did:plc:ewvi7nxzyoun6zhxrhs64oiz", Mode::Production).expect("did");
        let big = vec![b' '; MAX_LOG_BYTES + 1];
        assert_eq!(
            verify_audit_log(&did, &big, Mode::Production),
            Err(PlcError::TooLarge)
        );
        let entry = serde_json::json!({"did": did.as_str(), "operation": {}, "cid": "x"});
        let many = serde_json::to_vec(&vec![entry; MAX_LOG_ENTRIES + 1]).expect("json");
        assert_eq!(
            verify_audit_log(&did, &many, Mode::Production),
            Err(PlcError::TooLarge)
        );
        let web = Did::parse("did:web:rawkode.dev", Mode::Production).expect("did");
        assert_eq!(
            verify_audit_log(&web, b"[]", Mode::Production),
            Err(PlcError::NotPlc)
        );
    }
}
