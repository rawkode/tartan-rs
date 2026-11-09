//! The `academy.rawkode.tartan.signature` record (design D7, *The lexicon*).
//!
//! A record says "this account signs the payload whose keyed digest is `digest`":
//!
//! ```json
//! {"$type": "academy.rawkode.tartan.signature", "namespace": "git",
//!  "algorithm": "hmac-sha256", "digest": {"$bytes": "…"}, "createdAt": "2026-11-09T12:00:00Z"}
//! ```
//!
//! `digest` is HMAC-SHA256 keyed with a fresh 32-byte salt from the operating system's CSPRNG,
//! over the exact payload bytes. The salt travels only in the signature Git stores, so the public
//! record cannot be used to confirm a guess at the payload.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use cid::Cid;
use hmac::{Hmac, Mac};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::ipld::cid_for_dag_cbor;
use crate::signature::COLLECTION;

/// The only namespace verifiers accept.
pub const NAMESPACE_GIT: &str = "git";
/// The only algorithm verifiers accept.
pub const ALGORITHM_HMAC_SHA256: &str = "hmac-sha256";

/// A signature's 32-byte salt: the HMAC key of the record's digest.
#[derive(Clone, PartialEq, Eq)]
pub struct Salt([u8; 32]);

impl std::fmt::Debug for Salt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Salt(…)")
    }
}

/// The OS random number generator failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the operating system's random number generator failed")]
pub struct RandomError;

impl Salt {
    /// Draws a fresh salt from the operating system's CSPRNG.
    pub fn generate() -> Result<Self, RandomError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| RandomError)?;
        Ok(Salt(bytes))
    }

    /// A salt from known bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Salt(bytes)
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The `Salt:` line's form: unpadded base64url, exactly 43 characters.
    pub fn to_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Reads the `Salt:` line's form, requiring its one canonical spelling.
    pub fn from_base64url(text: &str) -> Option<Self> {
        if text.len() != 43 {
            return None;
        }
        let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(text).ok()?.try_into().ok()?;
        let salt = Salt(bytes);
        (salt.to_base64url() == text).then_some(salt)
    }
}

/// HMAC-SHA256 of `payload` keyed with `salt`.
pub fn keyed_digest(salt: &Salt, payload: &[u8]) -> [u8; 32] {
    let mut mac = hmac_for(salt);
    mac.update(payload);
    mac.finalize().into_bytes().into()
}

fn hmac_for(salt: &Salt) -> Hmac<Sha256> {
    // HMAC accepts keys of any length, so this cannot fail.
    <Hmac<Sha256> as Mac>::new_from_slice(&salt.0).unwrap_or_else(|_| unreachable!())
}

/// A signature record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureRecord {
    /// What kind of payload is signed (`git`).
    pub namespace: String,
    /// How `digest` is computed (`hmac-sha256`).
    pub algorithm: String,
    /// The keyed digest.
    pub digest: [u8; 32],
    /// When the signer signed, by the signer's clock.
    pub created_at: String,
}

/// Why a record block is not a valid signature record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The block is not a DAG-CBOR map with the required fields of the right types.
    #[error("the record is malformed: {0}")]
    Malformed(String),
    /// `$type` is not `academy.rawkode.tartan.signature`.
    #[error("the record's $type is `{0}`")]
    WrongType(String),
}

/// Why a decoded record does not sign a payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MismatchError {
    /// The namespace is not `git`.
    #[error("the record's namespace is `{0}`, not git")]
    Namespace(String),
    /// The algorithm is not `hmac-sha256`.
    #[error("the record's algorithm is `{0}`, not hmac-sha256")]
    Algorithm(String),
    /// The digest is not HMAC-SHA256 of the payload under the salt.
    #[error("the record's digest does not match the payload")]
    Digest,
}

impl SignatureRecord {
    /// The record for a Git payload: `namespace` `git`, `algorithm` `hmac-sha256`, the keyed
    /// digest of `payload` under `salt`, and `created_at` (RFC 3339, whole seconds, `Z`).
    pub fn for_git(payload: &[u8], salt: &Salt, created_at: String) -> Self {
        SignatureRecord {
            namespace: NAMESPACE_GIT.to_owned(),
            algorithm: ALGORITHM_HMAC_SHA256.to_owned(),
            digest: keyed_digest(salt, payload),
            created_at,
        }
    }

    /// The record's DAG-CBOR encoding, `$type` included.
    pub fn to_dag_cbor(&self) -> Vec<u8> {
        // Encoding a map of strings and bytes into a Vec cannot fail.
        serde_ipld_dagcbor::to_vec(&Encoded(self)).unwrap_or_default()
    }

    /// The record's CID: CIDv1, dag-cbor, sha2-256 of [`Self::to_dag_cbor`].
    pub fn cid(&self) -> Cid {
        cid_for_dag_cbor(&self.to_dag_cbor())
    }

    /// The record as `com.atproto.repo.applyWrites` sends it: `$type` included, `digest` as
    /// `{"$bytes": "<unpadded standard base64>"}`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "$type": COLLECTION,
            "namespace": self.namespace,
            "algorithm": self.algorithm,
            "digest": {"$bytes": STANDARD_NO_PAD.encode(self.digest)},
            "createdAt": self.created_at,
        })
    }

    /// Decodes a record block strictly: `$type`, `namespace`, `algorithm`, a 32-byte `digest`
    /// and an RFC 3339 `createdAt` must be present with these types; unknown fields are ignored,
    /// as Lexicon evolution allows.
    pub fn decode(block: &[u8]) -> Result<Self, RecordError> {
        let raw: Raw = serde_ipld_dagcbor::from_slice(block)
            .map_err(|error| RecordError::Malformed(error.to_string()))?;
        if raw.kind != COLLECTION {
            return Err(RecordError::WrongType(raw.kind));
        }
        if raw.namespace.len() > 64 || raw.algorithm.len() > 32 {
            return Err(RecordError::Malformed(
                "namespace or algorithm too long".into(),
            ));
        }
        if jacquard_common::deps::chrono::DateTime::parse_from_rfc3339(&raw.created_at).is_err() {
            return Err(RecordError::Malformed("createdAt is not a datetime".into()));
        }
        Ok(SignatureRecord {
            namespace: raw.namespace,
            algorithm: raw.algorithm,
            digest: raw.digest.0,
            created_at: raw.created_at,
        })
    }

    /// Checks that the record signs `payload` under `salt`: namespace `git`, algorithm
    /// `hmac-sha256`, and the digest compared in constant time.
    pub fn check(&self, payload: &[u8], salt: &Salt) -> Result<(), MismatchError> {
        if self.namespace != NAMESPACE_GIT {
            return Err(MismatchError::Namespace(self.namespace.clone()));
        }
        if self.algorithm != ALGORITHM_HMAC_SHA256 {
            return Err(MismatchError::Algorithm(self.algorithm.clone()));
        }
        let mut mac = hmac_for(salt);
        mac.update(payload);
        mac.verify_slice(&self.digest)
            .map_err(|_| MismatchError::Digest)
    }
}

/// The current time as `createdAt` wants it: RFC 3339, whole seconds, UTC with `Z`.
pub fn created_at_now() -> String {
    jacquard_common::deps::chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

struct Encoded<'a>(&'a SignatureRecord);

impl Serialize for Encoded<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Bytes<'a>(&'a [u8]);
        impl Serialize for Bytes<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_bytes(self.0)
            }
        }
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("$type", COLLECTION)?;
        map.serialize_entry("namespace", &self.0.namespace)?;
        map.serialize_entry("algorithm", &self.0.algorithm)?;
        map.serialize_entry("digest", &Bytes(&self.0.digest))?;
        map.serialize_entry("createdAt", &self.0.created_at)?;
        map.end()
    }
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "$type")]
    kind: String,
    namespace: String,
    algorithm: String,
    digest: Digest32,
    #[serde(rename = "createdAt")]
    created_at: String,
}

struct Digest32([u8; 32]);

impl<'de> Deserialize<'de> for Digest32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor;
        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = Digest32;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("32 bytes")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Digest32, E> {
                <[u8; 32]>::try_from(v)
                    .map(Digest32)
                    .map_err(|_| E::invalid_length(v.len(), &self))
            }
            fn visit_borrowed_bytes<E: de::Error>(self, v: &'de [u8]) -> Result<Digest32, E> {
                self.visit_bytes(v)
            }
            fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Digest32, E> {
                self.visit_bytes(&v)
            }
            fn visit_map<A: MapAccess<'de>>(self, _map: A) -> Result<Digest32, A::Error> {
                Err(de::Error::invalid_type(de::Unexpected::Map, &self))
            }
        }
        deserializer.deserialize_bytes(BytesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipld::{json_to_dag_cbor, to_dag_cbor};

    fn record() -> SignatureRecord {
        SignatureRecord::for_git(
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
            &Salt::from_bytes([7; 32]),
            "2026-11-09T12:00:00Z".into(),
        )
    }

    #[test]
    fn json_and_cbor_agree() {
        let record = record();
        let from_json = json_to_dag_cbor(&record.to_json()).expect("cbor");
        assert_eq!(from_json, record.to_dag_cbor());
        assert_eq!(cid_for_dag_cbor(&from_json), record.cid());
        let cid = record.cid();
        assert_eq!(cid.codec(), crate::ipld::DAG_CBOR);
        assert!(cid.to_string().starts_with("bafyrei"));
    }

    #[test]
    fn decode_round_trip_and_unknown_fields() {
        let record = record();
        assert_eq!(
            SignatureRecord::decode(&record.to_dag_cbor()),
            Ok(record.clone())
        );
        let mut json = record.to_json();
        json["note"] = serde_json::json!("ignored");
        let extended = json_to_dag_cbor(&json).expect("cbor");
        assert_eq!(SignatureRecord::decode(&extended), Ok(record));
    }

    #[test]
    fn strict_decode() {
        let base = record().to_json();
        let cases: Vec<(&str, serde_json::Value)> = vec![
            ("$type", serde_json::json!("app.bsky.feed.post")),
            ("namespace", serde_json::json!(3)),
            ("algorithm", serde_json::Value::Null),
            (
                "digest",
                serde_json::json!({"$bytes": STANDARD_NO_PAD.encode([0u8; 31])}),
            ),
            ("digest", serde_json::json!("not bytes")),
            ("createdAt", serde_json::json!("yesterday")),
        ];
        for (field, value) in cases {
            let mut json = base.clone();
            json[field] = value.clone();
            let block = json_to_dag_cbor(&json).expect("cbor");
            assert!(SignatureRecord::decode(&block).is_err(), "{field}: {value}");
        }
        for field in ["$type", "namespace", "algorithm", "digest", "createdAt"] {
            let mut json = base.clone();
            json.as_object_mut().expect("object").remove(field);
            let block = json_to_dag_cbor(&json).expect("cbor");
            assert!(SignatureRecord::decode(&block).is_err(), "missing {field}");
        }
        // A digest stored as the literal JSON map rather than bytes.
        let literal = to_dag_cbor(&base).expect("cbor");
        assert!(SignatureRecord::decode(&literal).is_err());
    }

    #[test]
    fn check() {
        let salt = Salt::from_bytes([7; 32]);
        let payload = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n";
        let record = record();
        assert_eq!(record.check(payload, &salt), Ok(()));
        assert_eq!(
            record.check(b"tree other\n", &salt),
            Err(MismatchError::Digest)
        );
        assert_eq!(
            record.check(payload, &Salt::from_bytes([8; 32])),
            Err(MismatchError::Digest)
        );
        let mut other = record.clone();
        other.namespace = "release".into();
        assert!(matches!(
            other.check(payload, &salt),
            Err(MismatchError::Namespace(_))
        ));
        let mut other = record;
        other.algorithm = "sha256".into();
        assert!(matches!(
            other.check(payload, &salt),
            Err(MismatchError::Algorithm(_))
        ));
    }

    #[test]
    fn salts() {
        let a = Salt::generate().expect("rng");
        let b = Salt::generate().expect("rng");
        assert_ne!(a, b);
        let text = a.to_base64url();
        assert_eq!(text.len(), 43);
        assert_eq!(Salt::from_base64url(&text), Some(a));
        assert_eq!(Salt::from_base64url(&format!("{text}=")), None);
        assert_eq!(
            Salt::from_base64url(&URL_SAFE_NO_PAD.encode([1u8; 31])),
            None
        );
    }

    #[test]
    fn created_at_format() {
        let now = created_at_now();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z'));
    }

    /// The published lexicon describes the record this module writes.
    #[test]
    fn lexicon_matches() {
        let path = format!(
            "{}/../../lexicons/academy/rawkode/tartan/signature.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let lexicon: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect("json");
        assert_eq!(lexicon["id"], COLLECTION);
        let main = &lexicon["defs"]["main"];
        assert_eq!(main["key"], "tid");
        let properties = &main["record"]["properties"];
        assert_eq!(properties["namespace"]["knownValues"][0], NAMESPACE_GIT);
        assert_eq!(
            properties["algorithm"]["knownValues"][0],
            ALGORITHM_HMAC_SHA256
        );
        assert_eq!(properties["digest"]["minLength"], 32);
        assert_eq!(properties["digest"]["maxLength"], 32);
        let json = record().to_json();
        let mut written: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "$type")
            .collect();
        written.sort_unstable();
        let mut required: Vec<&str> = main["record"]["required"]
            .as_array()
            .expect("required")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        required.sort_unstable();
        assert_eq!(written, required);
    }
}
