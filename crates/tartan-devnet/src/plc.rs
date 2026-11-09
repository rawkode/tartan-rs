//! The PLC directory: genuinely signed operations, audit logs and DID documents.
//!
//! A person's genesis operation names their handle, `#atproto` key and rotation key but no
//! services, so the DID depends only on the seed and the handle, never on the port the devnet
//! listens on. The operation after it adds the PDS endpoint. Every later change (a handle, a
//! key rotation, a migration, a tombstone) is another operation signed by the rotation key,
//! so `tartan-atproto`'s audit-log verifier replays the log exactly as it would plc.directory's.

use cid::multibase;
use k256::ecdsa::SigningKey;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tartan_atproto::ipld::{cid_for_dag_cbor, json_to_dag_cbor};

use crate::keys;

/// One signed operation as the directory stores it.
#[derive(Debug, Clone)]
pub(crate) struct Operation {
    /// The signed operation.
    pub json: Value,
    /// Its CID.
    pub cid: String,
    /// When the directory accepted it (RFC 3339).
    pub created_at: String,
}

/// The facts a `plc_operation` states.
#[derive(Debug, Clone)]
pub(crate) struct Facts {
    pub handle: String,
    pub atproto_key: String,
    pub rotation_key: String,
    pub pds: Option<String>,
}

fn unsigned_operation(facts: &Facts, prev: Option<&str>) -> Map<String, Value> {
    let services = match &facts.pds {
        Some(endpoint) => json!({
            "atproto_pds": {"type": "AtprotoPersonalDataServer", "endpoint": endpoint}
        }),
        None => json!({}),
    };
    let value = json!({
        "type": "plc_operation",
        "rotationKeys": [facts.rotation_key],
        "verificationMethods": {"atproto": facts.atproto_key},
        "alsoKnownAs": [format!("at://{}", facts.handle)],
        "services": services,
        "prev": prev,
    });
    match value {
        Value::Object(map) => map,
        _ => unreachable!("json! of an object is an object"),
    }
}

fn sign(mut unsigned: Map<String, Value>, rotation: &SigningKey, now_ms: u64) -> Operation {
    let bytes = json_to_dag_cbor(&Value::Object(unsigned.clone()))
        .expect("a PLC operation is valid DAG-CBOR");
    unsigned.insert(
        "sig".into(),
        Value::String(keys::sign_base64url(rotation, &bytes)),
    );
    let json = Value::Object(unsigned);
    let signed = json_to_dag_cbor(&json).expect("a PLC operation is valid DAG-CBOR");
    Operation {
        cid: cid_for_dag_cbor(&signed).to_string(),
        json,
        created_at: crate::util::rfc3339(now_ms),
    }
}

/// The genesis operation for `facts` (its `pds` is ignored) and the DID it creates.
pub(crate) fn genesis(facts: &Facts, rotation: &SigningKey, now_ms: u64) -> (String, Operation) {
    let facts = Facts {
        pds: None,
        ..facts.clone()
    };
    let operation = sign(unsigned_operation(&facts, None), rotation, now_ms);
    let signed = json_to_dag_cbor(&operation.json).expect("a PLC operation is valid DAG-CBOR");
    let encoded = multibase::Base::Base32Lower.encode(Sha256::digest(&signed));
    (format!("did:plc:{}", &encoded[..24]), operation)
}

/// A `plc_operation` after `prev` stating `facts`.
pub(crate) fn update(
    prev: &Operation,
    facts: &Facts,
    rotation: &SigningKey,
    now_ms: u64,
) -> Operation {
    sign(unsigned_operation(facts, Some(&prev.cid)), rotation, now_ms)
}

/// A `plc_tombstone` after `prev`.
pub(crate) fn tombstone(prev: &Operation, rotation: &SigningKey, now_ms: u64) -> Operation {
    let unsigned = match json!({"type": "plc_tombstone", "prev": prev.cid}) {
        Value::Object(map) => map,
        _ => unreachable!("json! of an object is an object"),
    };
    sign(unsigned, rotation, now_ms)
}

/// `GET /{did}/log/audit`.
pub(crate) fn audit_log(did: &str, operations: &[Operation]) -> Value {
    Value::Array(
        operations
            .iter()
            .map(|operation| {
                json!({
                    "did": did,
                    "operation": operation.json,
                    "cid": operation.cid,
                    "nullified": false,
                    "createdAt": operation.created_at,
                })
            })
            .collect(),
    )
}

/// `GET /{did}`: the DID document of the last operation, or `None` after a tombstone.
pub(crate) fn document(did: &str, operations: &[Operation]) -> Option<Value> {
    let last = &operations.last()?.json;
    if last.get("type").and_then(Value::as_str) != Some("plc_operation") {
        return None;
    }
    let mut document = json!({
        "@context": [
            "https://www.w3.org/ns/did/v1",
            "https://w3id.org/security/multikey/v1",
            "https://w3id.org/security/suites/secp256k1-2019/v1"
        ],
        "id": did,
        "alsoKnownAs": last.get("alsoKnownAs").cloned().unwrap_or_else(|| json!([])),
        "verificationMethod": [],
        "service": [],
    });
    if let Some(key) = last
        .pointer("/verificationMethods/atproto")
        .and_then(Value::as_str)
    {
        document["verificationMethod"] = json!([{
            "id": format!("{did}#atproto"),
            "type": "Multikey",
            "controller": did,
            "publicKeyMultibase": key.trim_start_matches("did:key:"),
        }]);
    }
    if let Some(service) = last.pointer("/services/atproto_pds") {
        document["service"] = json!([{
            "id": "#atproto_pds",
            "type": service.get("type"),
            "serviceEndpoint": service.get("endpoint"),
        }]);
    }
    Some(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tartan_atproto::Mode;
    use tartan_atproto::did::Did;
    use tartan_atproto::plc::{PlcState, verify_audit_log};

    fn facts(handle: &str, pds: Option<&str>) -> (Facts, SigningKey) {
        let rotation = keys::derive("t", "rotation");
        let atproto = keys::derive("t", "atproto");
        (
            Facts {
                handle: handle.into(),
                atproto_key: keys::did_key(&atproto),
                rotation_key: keys::did_key(&rotation),
                pds: pds.map(str::to_owned),
            },
            rotation,
        )
    }

    #[test]
    fn the_verifier_accepts_the_log() {
        let (facts, rotation) = facts("alice.test", Some("http://127.0.0.1:9400"));
        let (did, genesis) = genesis(&facts, &rotation, 1);
        let second = update(&genesis, &facts, &rotation, 2);
        let log =
            serde_json::to_vec(&audit_log(&did, &[genesis.clone(), second.clone()])).expect("json");
        let parsed = Did::parse(&did, Mode::Development).expect("did");
        let evidence = verify_audit_log(&parsed, &log, Mode::Development).expect("verifies");
        let PlcState::Active(identity) = evidence.state else {
            panic!("tombstoned");
        };
        assert_eq!(identity.handle.as_deref(), Some("alice.test"));
        assert_eq!(identity.pds.as_deref(), Some("http://127.0.0.1:9400"));

        let third = tombstone(&second, &rotation, 3);
        let log = serde_json::to_vec(&audit_log(&did, &[genesis, second, third])).expect("json");
        let evidence = verify_audit_log(&parsed, &log, Mode::Development).expect("verifies");
        assert_eq!(evidence.state, PlcState::Tombstoned);
    }

    #[test]
    fn the_did_ignores_the_endpoint() {
        let (a, rotation) = facts("alice.test", Some("http://127.0.0.1:1"));
        let (b, _) = facts("alice.test", Some("http://127.0.0.1:2"));
        assert_eq!(genesis(&a, &rotation, 1).0, genesis(&b, &rotation, 99).0);
    }

    #[test]
    fn documents_name_the_key_and_service() {
        let (facts, rotation) = facts("alice.test", Some("http://127.0.0.1:9400"));
        let (did, genesis) = genesis(&facts, &rotation, 1);
        let second = update(&genesis, &facts, &rotation, 2);
        let document = document(&did, &[genesis, second]).expect("document");
        assert_eq!(document["service"][0]["id"], "#atproto_pds");
        assert_eq!(
            document["verificationMethod"][0]["publicKeyMultibase"]
                .as_str()
                .map(|key| format!("did:key:{key}")),
            Some(facts.atproto_key)
        );
    }
}
