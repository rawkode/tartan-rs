//! What a DID document says, and reading a `did:web` document (design D9, *did:web*).
//!
//! - The document's `id` must be the DID.
//! - The PDS is the service with id `#atproto_pds` and type `AtprotoPersonalDataServer`; a
//!   document whose `#atproto_pds` has another type has no PDS.
//! - The signing key is the verification method with id `#atproto` and type `Multikey`, or one
//!   of the legacy `EcdsaSecp256k1VerificationKey2019` / `EcdsaSecp256r1VerificationKey2019`
//!   types, which ATProto still asks resolvers to accept.
//! - The handle claim is the first `at://` entry of `alsoKnownAs`, lowercased.

use serde_json::Value;

use crate::Mode;
use crate::did::Did;
use crate::keys::AtprotoKey;
use crate::plc::handle_claim;

/// The largest did:web document accepted.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

/// The ATProto facts of a DID document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtprotoIdentity {
    /// The DID.
    pub did: Did,
    /// The handle the document claims (lowercased, without `at://`). Only a claim: it is
    /// verified in the other direction by [`crate::handle`].
    pub handle: Option<String>,
    /// The PDS endpoint (scheme and authority, no trailing slash).
    pub pds: Option<String>,
    /// The `#atproto` key.
    pub signing_key: Option<AtprotoKey>,
}

/// Why a did:web document was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// Larger than [`MAX_DOCUMENT_BYTES`].
    #[error("the DID document is larger than 64 KiB")]
    TooLarge,
    /// Not a JSON object.
    #[error("the DID document is not a JSON object")]
    Malformed,
    /// Its `id` is not the DID.
    #[error("the DID document's id is `{0}`, not the DID")]
    IdMismatch(String),
}

/// Reads the ATProto facts of a did:web document, keeping nothing but what it can verify.
pub fn read_document(
    did: &Did,
    bytes: &[u8],
    mode: Mode,
) -> Result<AtprotoIdentity, DocumentError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(DocumentError::TooLarge);
    }
    let document: Value = serde_json::from_slice(bytes).map_err(|_| DocumentError::Malformed)?;
    let object = document.as_object().ok_or(DocumentError::Malformed)?;
    let id = object.get("id").and_then(Value::as_str).unwrap_or_default();
    if id != did.as_str() {
        return Err(DocumentError::IdMismatch(id.to_owned()));
    }
    let names = |fragment: &str, value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|id| id == fragment || id == format!("{did}{fragment}"))
    };
    let handle = object
        .get("alsoKnownAs")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .find(|entry| entry.starts_with("at://"))
        })
        .and_then(handle_claim);
    let signing_key = object
        .get("verificationMethod")
        .and_then(Value::as_array)
        .and_then(|methods| methods.iter().find(|m| names("#atproto", m.get("id"))))
        .and_then(|method| {
            let kind = method.get("type")?.as_str()?;
            let key = method.get("publicKeyMultibase")?.as_str()?;
            match kind {
                "Multikey" => AtprotoKey::from_multikey(key).ok(),
                _ => AtprotoKey::from_legacy(kind, key).ok(),
            }
        });
    let pds = object
        .get("service")
        .and_then(Value::as_array)
        .and_then(|services| services.iter().find(|s| names("#atproto_pds", s.get("id"))))
        .filter(|service| {
            service.get("type").and_then(Value::as_str) == Some("AtprotoPersonalDataServer")
        })
        .and_then(|service| service.get("serviceEndpoint")?.as_str())
        .and_then(|endpoint| normalise_pds_endpoint(endpoint, mode));
    Ok(AtprotoIdentity {
        did: did.clone(),
        handle,
        pds,
        signing_key,
    })
}

/// Normalises a PDS endpoint to `scheme://authority`, or `None` when it is not an absolute
/// `https` URL (`http` too in development mode) without credentials, query or fragment.
pub fn normalise_pds_endpoint(endpoint: &str, mode: Mode) -> Option<String> {
    let url = reqwest::Url::parse(endpoint).ok()?;
    let scheme_ok = url.scheme() == "https" || (mode.is_development() && url.scheme() == "http");
    if !scheme_ok
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cid::multibase;
    use k256::elliptic_curve::sec1::ToEncodedPoint;
    use serde_json::json;

    fn did() -> Did {
        Did::parse("did:web:example.com", Mode::Production).expect("did")
    }

    const KEY: &str = "zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ";

    fn document() -> Value {
        json!({
            "@context": ["https://www.w3.org/ns/did/v1"],
            "id": "did:web:example.com",
            "alsoKnownAs": ["https://example.com", "at://Alice.Example.com"],
            "verificationMethod": [{
                "id": "did:web:example.com#atproto",
                "type": "Multikey",
                "controller": "did:web:example.com",
                "publicKeyMultibase": KEY
            }],
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "https://pds.example.com/"
            }]
        })
    }

    fn read(value: &Value) -> Result<AtprotoIdentity, DocumentError> {
        read_document(
            &did(),
            &serde_json::to_vec(value).expect("json"),
            Mode::Production,
        )
    }

    #[test]
    fn captured_owner_document() {
        let path = format!(
            "{}/testdata/web/rawkode.dev.did.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).expect(&path);
        let did = Did::parse("did:web:rawkode.dev", Mode::Production).expect("did");
        let identity = read_document(&did, &bytes, Mode::Production).expect("valid");
        assert_eq!(identity.handle.as_deref(), Some("rawkode.dev"));
        assert_eq!(identity.pds.as_deref(), Some("https://rawkode.dev"));
        assert!(identity.signing_key.is_some());
    }

    #[test]
    fn complete_document() {
        let identity = read(&document()).expect("valid");
        assert_eq!(identity.handle.as_deref(), Some("alice.example.com"));
        assert_eq!(identity.pds.as_deref(), Some("https://pds.example.com"));
        assert_eq!(
            identity.signing_key.map(|key| key.multikey()).as_deref(),
            Some(KEY)
        );
    }

    #[test]
    fn mismatched_id() {
        let mut doc = document();
        doc["id"] = json!("did:web:other.example");
        assert_eq!(
            read(&doc),
            Err(DocumentError::IdMismatch("did:web:other.example".into()))
        );
    }

    #[test]
    fn missing_pds() {
        let mut doc = document();
        doc["service"] = json!([]);
        assert_eq!(read(&doc).expect("valid").pds, None);
    }

    #[test]
    fn pds_service_of_another_type() {
        let mut doc = document();
        doc["service"][0]["type"] = json!("BskyFeedGenerator");
        assert_eq!(read(&doc).expect("valid").pds, None);
    }

    #[test]
    fn pds_endpoint_must_be_https_origin() {
        for endpoint in [
            "http://pds.example.com",
            "https://pds.example.com/xrpc",
            "https://user:pass@pds.example.com",
            "pds.example.com",
        ] {
            let mut doc = document();
            doc["service"][0]["serviceEndpoint"] = json!(endpoint);
            assert_eq!(read(&doc).expect("valid").pds, None, "{endpoint}");
        }
    }

    #[test]
    fn atproto_key_must_be_multikey_or_legacy() {
        let mut doc = document();
        doc["verificationMethod"][0]["type"] = json!("JsonWebKey2020");
        assert_eq!(read(&doc).expect("valid").signing_key, None);
    }

    #[test]
    fn legacy_key_types() {
        let secret = k256::SecretKey::from_slice(&[5u8; 32]).expect("secret");
        let raw = multibase::encode(
            multibase::Base::Base58Btc,
            secret.public_key().to_encoded_point(false).as_bytes(),
        );
        let mut doc = document();
        doc["verificationMethod"][0]["type"] = json!("EcdsaSecp256k1VerificationKey2019");
        doc["verificationMethod"][0]["publicKeyMultibase"] = json!(raw);
        assert!(read(&doc).expect("valid").signing_key.is_some());

        let p_secret = p256::SecretKey::from_slice(&[6u8; 32]).expect("secret");
        let p_raw = multibase::encode(
            multibase::Base::Base58Btc,
            p_secret.public_key().to_encoded_point(false).as_bytes(),
        );
        doc["verificationMethod"][0]["type"] = json!("EcdsaSecp256r1VerificationKey2019");
        doc["verificationMethod"][0]["publicKeyMultibase"] = json!(p_raw);
        assert!(read(&doc).expect("valid").signing_key.is_some());
    }

    #[test]
    fn short_multibase_key_fails_without_panic() {
        for key in ["z", "z1", "zQ", ""] {
            let mut doc = document();
            doc["verificationMethod"][0]["publicKeyMultibase"] = json!(key);
            assert_eq!(read(&doc).expect("valid").signing_key, None, "{key:?}");
            doc["verificationMethod"][0]["type"] = json!("EcdsaSecp256k1VerificationKey2019");
            assert_eq!(read(&doc).expect("valid").signing_key, None, "{key:?}");
        }
    }

    #[test]
    fn other_fragments_are_ignored() {
        let mut doc = document();
        doc["verificationMethod"][0]["id"] = json!("did:web:other.example#atproto");
        doc["service"][0]["id"] = json!("#atproto_labeler");
        let identity = read(&doc).expect("valid");
        assert_eq!(identity.signing_key, None);
        assert_eq!(identity.pds, None);
    }

    #[test]
    fn size_cap_and_malformed() {
        let big = vec![b' '; MAX_DOCUMENT_BYTES + 1];
        assert_eq!(
            read_document(&did(), &big, Mode::Production),
            Err(DocumentError::TooLarge)
        );
        assert_eq!(
            read_document(&did(), b"[]", Mode::Production),
            Err(DocumentError::Malformed)
        );
    }
}
