//! DAG-CBOR and CIDs.
//!
//! ATProto hashes records and PLC operations as DAG-CBOR. The JSON form of the data model
//! writes bytes as `{"$bytes": "<base64>"}` and links as `{"$link": "<cid>"}`; a PDS converts
//! that JSON to the data model before it hashes a record, so a record's CID is computed over
//! the converted form ([`json_to_dag_cbor`]).

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use cid::Cid;
use cid::multihash::Multihash;
use serde::ser::{Error as _, SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

/// The multicodec of DAG-CBOR.
pub const DAG_CBOR: u64 = 0x71;
/// The multicodec of raw bytes.
pub const RAW: u64 = 0x55;
/// The multihash code of SHA2-256.
pub const SHA2_256: u64 = 0x12;

/// Why JSON could not be converted to DAG-CBOR.
#[derive(Debug, thiserror::Error)]
pub enum IpldError {
    /// The JSON uses a number that is not an integer (the data model has no floats).
    #[error("the data model has no floating-point numbers")]
    Float,
    /// A `$bytes` or `$link` object is malformed.
    #[error("malformed {0} object")]
    Malformed(&'static str),
    /// Encoding failed.
    #[error("DAG-CBOR encoding failed: {0}")]
    Encode(String),
}

/// The CIDv1 (dag-cbor, sha2-256) of a DAG-CBOR block.
pub fn cid_for_dag_cbor(block: &[u8]) -> Cid {
    cid_for(DAG_CBOR, block)
}

/// The CIDv1 with `codec` and a sha2-256 multihash of `block`.
pub fn cid_for(codec: u64, block: &[u8]) -> Cid {
    let digest = Sha256::digest(block);
    // A 32-byte digest always fits the 64-byte multihash.
    let hash = Multihash::<64>::wrap(SHA2_256, &digest).unwrap_or_default();
    Cid::new_v1(codec, hash)
}

/// True when `cid` is the sha2-256 CIDv1 of `block` and its codec is dag-cbor or raw.
pub fn block_matches(cid: &Cid, block: &[u8]) -> bool {
    cid.version() == cid::Version::V1
        && (cid.codec() == DAG_CBOR || cid.codec() == RAW)
        && cid.hash().code() == SHA2_256
        && cid.hash().size() == 32
        && cid.hash().digest() == Sha256::digest(block).as_slice()
}

/// Encodes the ATProto JSON form of a value as DAG-CBOR: `{"$bytes"}` objects become byte
/// strings, `{"$link"}` objects become CID links, and map keys are sorted as DAG-CBOR requires.
pub fn json_to_dag_cbor(value: &serde_json::Value) -> Result<Vec<u8>, IpldError> {
    check_json(value)?;
    serde_ipld_dagcbor::to_vec(&JsonAsIpld(value))
        .map_err(|error| IpldError::Encode(error.to_string()))
}

/// Finds what [`JsonAsIpld`] cannot encode, so the error says why.
fn check_json(value: &serde_json::Value) -> Result<(), IpldError> {
    use serde_json::Value;
    match value {
        Value::Number(n) if n.as_i64().is_none() && n.as_u64().is_none() => Err(IpldError::Float),
        Value::Array(items) => items.iter().try_for_each(check_json),
        Value::Object(map) => {
            if let Some(bytes) = map.get("$bytes") {
                let ok = map.len() == 1
                    && bytes
                        .as_str()
                        .is_some_and(|text| decode_bytes(text).is_some());
                return if ok {
                    Ok(())
                } else {
                    Err(IpldError::Malformed("$bytes"))
                };
            }
            if let Some(link) = map.get("$link") {
                let ok = map.len() == 1
                    && link
                        .as_str()
                        .is_some_and(|text| Cid::try_from(text).is_ok());
                return if ok {
                    Ok(())
                } else {
                    Err(IpldError::Malformed("$link"))
                };
            }
            map.values().try_for_each(check_json)
        }
        _ => Ok(()),
    }
}

/// Decodes a `$bytes` value: standard base64, unpadded as ATProto writes it (padded accepted).
fn decode_bytes(text: &str) -> Option<Vec<u8>> {
    STANDARD_NO_PAD
        .decode(text)
        .or_else(|_| STANDARD.decode(text))
        .ok()
}

/// Encodes any serializable value as DAG-CBOR.
pub fn to_dag_cbor<T: Serialize>(value: &T) -> Result<Vec<u8>, IpldError> {
    serde_ipld_dagcbor::to_vec(value).map_err(|error| IpldError::Encode(error.to_string()))
}

struct JsonAsIpld<'a>(&'a serde_json::Value);

impl Serialize for JsonAsIpld<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde_json::Value;
        match self.0 {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    serializer.serialize_i64(i)
                } else if let Some(u) = n.as_u64() {
                    serializer.serialize_u64(u)
                } else {
                    Err(S::Error::custom(
                        "the data model has no floating-point numbers",
                    ))
                }
            }
            Value::String(s) => serializer.serialize_str(s),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&JsonAsIpld(item))?;
                }
                seq.end()
            }
            Value::Object(map) => {
                if let Some(bytes) = map.get("$bytes") {
                    let text = bytes
                        .as_str()
                        .filter(|_| map.len() == 1)
                        .ok_or_else(|| S::Error::custom("malformed $bytes object"))?;
                    let decoded = decode_bytes(text)
                        .ok_or_else(|| S::Error::custom("malformed $bytes object"))?;
                    return serializer.serialize_bytes(&decoded);
                }
                if let Some(link) = map.get("$link") {
                    let cid = link
                        .as_str()
                        .filter(|_| map.len() == 1)
                        .and_then(|text| Cid::try_from(text).ok())
                        .ok_or_else(|| S::Error::custom("malformed $link object"))?;
                    return cid.serialize(serializer);
                }
                let mut out = serializer.serialize_map(Some(map.len()))?;
                for (key, value) in map {
                    out.serialize_entry(key, &JsonAsIpld(value))?;
                }
                out.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(name: &str) -> serde_json::Value {
        let path = format!("{}/testdata/pds/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).expect(&path);
        serde_json::from_slice(&bytes).expect("json")
    }

    /// A record holding `$bytes` values, as `com.atproto.repo.getRecord` on a reference PDS
    /// (bsky.network) returned it: the CID computed locally from the JSON equals the CID the PDS
    /// reported.
    #[test]
    fn bytes_record_cid_matches_reference_pds() {
        let answer = captured("germ-declaration.getRecord.json");
        let cbor = json_to_dag_cbor(&answer["value"]).expect("cbor");
        assert_eq!(
            cid_for_dag_cbor(&cbor).to_string(),
            answer["cid"].as_str().expect("cid")
        );
    }

    /// The PDS behind did:web:rawkode.dev (not the reference implementation) stored a blob's
    /// `ref` as the literal map `{"$link": "…"}` rather than as a CID link: the CID it reports is
    /// that of the unconverted JSON, not of the data model the reference PDS would have stored.
    /// Kept as a record of that difference, which matters for `tt`'s locally computed CID.
    #[test]
    fn owner_pds_stores_link_objects_unconverted() {
        let answer = captured("rawkode-profile.getRecord.json");
        let converted = json_to_dag_cbor(&answer["value"]).expect("cbor");
        let literal = to_dag_cbor(&answer["value"]).expect("cbor");
        let reported = answer["cid"].as_str().expect("cid");
        assert_ne!(cid_for_dag_cbor(&converted).to_string(), reported);
        assert_eq!(cid_for_dag_cbor(&literal).to_string(), reported);
    }

    #[test]
    fn refuses_floats_and_malformed_objects() {
        assert!(matches!(
            json_to_dag_cbor(&serde_json::json!({"a": 1.5})),
            Err(IpldError::Float)
        ));
        assert!(json_to_dag_cbor(&serde_json::json!({"$bytes": 3})).is_err());
        assert!(json_to_dag_cbor(&serde_json::json!({"$bytes": "AA", "x": 1})).is_err());
        assert!(json_to_dag_cbor(&serde_json::json!({"$link": "nope"})).is_err());
    }

    #[test]
    fn block_matching() {
        let block = to_dag_cbor(&"hello").expect("cbor");
        let cid = cid_for_dag_cbor(&block);
        assert!(block_matches(&cid, &block));
        assert!(!block_matches(&cid, b"other"));
        let raw = cid_for(RAW, b"x");
        assert!(block_matches(&raw, b"x"));
        let other_codec = cid_for(0x70, b"x");
        assert!(!block_matches(&other_codec, b"x"));
    }
}
