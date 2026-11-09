//! `#atproto` keys and signature verification.
//!
//! Keys are P-256 or secp256k1, as ATProto requires. Signatures are verified with
//! atrium-crypto's default [`Verifier`], which accepts only compact 64-byte low-S signatures,
//! so a high-S signature (the malleable twin of a valid one) is refused (design D2, D8 step 5.3,
//! D9 step 3).
//!
//! atrium-crypto 0.1.3's `parse_multikey` indexes the decoded bytes without a length check, so
//! every multibase key is decoded and length-checked here before it is handed over (design D2,
//! *Panic paths on untrusted input*). The legacy `EcdsaSecp256k1VerificationKey2019` and
//! `EcdsaSecp256r1VerificationKey2019` forms carry no multicodec prefix at all and are decoded
//! here (design D9).

use std::borrow::Cow;

use atrium_crypto::Algorithm;
use atrium_crypto::verify::Verifier;
use cid::multibase;
use jacquard_common::types::crypto::{KeyCodec, PublicKey};

/// The multicodec prefix of a compressed P-256 public key.
const PREFIX_P256: [u8; 2] = [0x80, 0x24];
/// The multicodec prefix of a compressed secp256k1 public key.
const PREFIX_K256: [u8; 2] = [0xe7, 0x01];

/// A curve ATProto signs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Curve {
    /// NIST P-256 (`secp256r1`).
    P256,
    /// `secp256k1`.
    K256,
}

/// A public `#atproto` (or PLC rotation) key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AtprotoKey {
    curve: Curve,
    /// The SEC1 compressed point (33 bytes).
    compressed: Vec<u8>,
}

impl std::fmt::Debug for AtprotoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AtprotoKey").field(&self.did_key()).finish()
    }
}

/// Why a key could not be read or a signature did not verify.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The text is not a `did:key:` value.
    #[error("not a did:key")]
    NotDidKey,
    /// The multibase text does not decode.
    #[error("the key is not valid multibase")]
    Multibase,
    /// The key is a multibase encoding other than base58btc (`z…`).
    #[error("the key is not base58btc multibase")]
    NotBase58,
    /// The decoded key has the wrong length or an unknown multicodec prefix.
    #[error("the key is not a P-256 or secp256k1 multikey")]
    UnsupportedKey,
    /// The bytes are not a point on the curve.
    #[error("the key is not a valid curve point")]
    BadPoint,
    /// The verification method's type is not one Tartan reads.
    #[error("unsupported verification method type `{0}`")]
    UnsupportedType(String),
    /// The signature does not verify (wrong key or message, high-S, or not compact).
    #[error("the signature does not verify")]
    BadSignature,
}

impl AtprotoKey {
    /// Reads a multikey (`z…`, base58btc of a multicodec-prefixed compressed point), as found in
    /// a `Multikey` verification method's `publicKeyMultibase`.
    pub fn from_multikey(multikey: &str) -> Result<Self, KeyError> {
        let decoded = decode_base58btc(multikey)?;
        // Check the length before atrium-crypto sees the bytes: its `parse_multikey` panics on
        // fewer than two.
        if decoded.len() != 35 || !matches!(decoded[2], 0x02 | 0x03) {
            return Err(KeyError::UnsupportedKey);
        }
        let curve = match [decoded[0], decoded[1]] {
            PREFIX_P256 => Curve::P256,
            PREFIX_K256 => Curve::K256,
            _ => return Err(KeyError::UnsupportedKey),
        };
        let (algorithm, _uncompressed) =
            atrium_crypto::did::parse_multikey(multikey).map_err(|_| KeyError::BadPoint)?;
        debug_assert_eq!(algorithm, curve.algorithm());
        Self::from_sec1(curve, &decoded[2..])
    }

    /// Reads a `did:key:z…` value, as found in PLC operations.
    pub fn from_did_key(did_key: &str) -> Result<Self, KeyError> {
        let multikey = did_key
            .strip_prefix("did:key:")
            .ok_or(KeyError::NotDidKey)?;
        Self::from_multikey(multikey)
    }

    /// Reads a legacy verification method: `EcdsaSecp256k1VerificationKey2019` or
    /// `EcdsaSecp256r1VerificationKey2019`, whose `publicKeyMultibase` is base58btc of the raw
    /// (normally uncompressed) point with no multicodec prefix.
    pub fn from_legacy(method_type: &str, multibase_key: &str) -> Result<Self, KeyError> {
        let curve = match method_type {
            "EcdsaSecp256r1VerificationKey2019" => Curve::P256,
            "EcdsaSecp256k1VerificationKey2019" => Curve::K256,
            other => return Err(KeyError::UnsupportedType(other.to_owned())),
        };
        let decoded = decode_base58btc(multibase_key)?;
        let sec1_form = match decoded.first() {
            Some(0x04) => decoded.len() == 65,
            Some(0x02 | 0x03) => decoded.len() == 33,
            _ => false,
        };
        if !sec1_form {
            return Err(KeyError::UnsupportedKey);
        }
        Self::from_sec1(curve, &decoded)
    }

    /// Builds a key from a SEC1 point (compressed or uncompressed).
    pub fn from_sec1(curve: Curve, point: &[u8]) -> Result<Self, KeyError> {
        let compressed = match curve {
            Curve::P256 => p256::PublicKey::from_sec1_bytes(point)
                .map(|key| p256::EncodedPoint::from(key).compress().as_bytes().to_vec()),
            Curve::K256 => k256::PublicKey::from_sec1_bytes(point)
                .map(|key| k256::EncodedPoint::from(key).compress().as_bytes().to_vec()),
        }
        .map_err(|_| KeyError::BadPoint)?;
        Ok(AtprotoKey { curve, compressed })
    }

    /// The curve.
    pub fn curve(&self) -> Curve {
        self.curve
    }

    /// The SEC1 compressed point.
    pub fn compressed(&self) -> &[u8] {
        &self.compressed
    }

    /// The multikey form (`z…`).
    pub fn multikey(&self) -> String {
        let prefix = match self.curve {
            Curve::P256 => PREFIX_P256,
            Curve::K256 => PREFIX_K256,
        };
        let mut bytes = Vec::with_capacity(35);
        bytes.extend_from_slice(&prefix);
        bytes.extend_from_slice(&self.compressed);
        multibase::encode(multibase::Base::Base58Btc, bytes)
    }

    /// The `did:key:` form.
    pub fn did_key(&self) -> String {
        format!("did:key:{}", self.multikey())
    }

    /// Verifies a compact, low-S ECDSA signature over `message` (hashed with SHA-256).
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), KeyError> {
        Verifier::default()
            .verify(self.curve.algorithm(), &self.compressed, message, signature)
            .map_err(|_| KeyError::BadSignature)
    }

    /// The key as jacquard's [`PublicKey`], for `jacquard_repo::commit::proof::verify_proofs`.
    pub fn to_jacquard(&self) -> PublicKey<'static> {
        PublicKey {
            codec: match self.curve {
                Curve::P256 => KeyCodec::P256,
                Curve::K256 => KeyCodec::Secp256k1,
            },
            bytes: Cow::Owned(self.compressed.clone()),
        }
    }
}

impl Curve {
    fn algorithm(self) -> Algorithm {
        match self {
            Curve::P256 => Algorithm::P256,
            Curve::K256 => Algorithm::Secp256k1,
        }
    }
}

fn decode_base58btc(text: &str) -> Result<Vec<u8>, KeyError> {
    if !text.starts_with('z') {
        return Err(KeyError::NotBase58);
    }
    let (base, bytes) = multibase::decode(text).map_err(|_| KeyError::Multibase)?;
    if base != multibase::Base::Base58Btc {
        return Err(KeyError::NotBase58);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::signature::Signer;
    use k256::elliptic_curve::sec1::ToEncodedPoint;

    #[test]
    fn reads_known_multikeys() {
        // The #atproto key of did:web:rawkode.dev and a PLC rotation key from plc.directory.
        for text in [
            "zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ",
            "zQ3shhCGUqDKjStzuDxPkTxN6ujddP4RkEKJJouJGRRkaLGbg",
        ] {
            let key = AtprotoKey::from_multikey(text).expect(text);
            assert_eq!(key.curve(), Curve::K256);
            assert_eq!(key.multikey(), text);
        }
        let did_key = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo";
        let key = AtprotoKey::from_did_key(did_key).expect("p256 did:key");
        assert_eq!(key.curve(), Curve::P256);
        assert_eq!(key.did_key(), did_key);
    }

    #[test]
    fn short_and_malformed_keys_fail_without_panicking() {
        for text in ["z", "z1", "z2", "zQ3s", "", "Q3shpG8", "mAAAA", "z0OIl"] {
            assert!(AtprotoKey::from_multikey(text).is_err(), "{text:?}");
        }
        // A valid multicodec prefix followed by something that is not a compressed point.
        let bytes = PREFIX_K256.to_vec();
        for tag in [5u8, 7] {
            let mut bytes = bytes.clone();
            bytes.push(tag);
            bytes.extend_from_slice(&[2u8; 32]);
            let bogus = multibase::encode(multibase::Base::Base58Btc, bytes);
            assert!(AtprotoKey::from_multikey(&bogus).is_err(), "{tag}");
        }
        assert_eq!(
            AtprotoKey::from_did_key("did:web:x"),
            Err(KeyError::NotDidKey)
        );
    }

    #[test]
    fn legacy_keys() {
        let secret = k256::SecretKey::from_slice(&[7u8; 32]).expect("secret");
        let point = k256::EncodedPoint::from(secret.public_key());
        let uncompressed = secret
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        assert_eq!(uncompressed.len(), 65);
        let text = multibase::encode(multibase::Base::Base58Btc, &uncompressed);
        let key = AtprotoKey::from_legacy("EcdsaSecp256k1VerificationKey2019", &text)
            .expect("legacy k256");
        assert_eq!(key.compressed(), point.compress().as_bytes());

        let p_secret = p256::SecretKey::from_slice(&[9u8; 32]).expect("secret");
        let p_text = multibase::encode(
            multibase::Base::Base58Btc,
            p_secret.public_key().to_encoded_point(false).as_bytes(),
        );
        let p_key = AtprotoKey::from_legacy("EcdsaSecp256r1VerificationKey2019", &p_text)
            .expect("legacy p256");
        assert_eq!(p_key.curve(), Curve::P256);

        assert!(AtprotoKey::from_legacy("EcdsaSecp256k1VerificationKey2019", "z").is_err());
        assert!(AtprotoKey::from_legacy("Ed25519VerificationKey2020", &text).is_err());
    }

    #[test]
    fn verification_refuses_high_s() {
        let signing = k256::ecdsa::SigningKey::from_slice(&[3u8; 32]).expect("key");
        let key = AtprotoKey::from_sec1(
            Curve::K256,
            signing.verifying_key().to_encoded_point(true).as_bytes(),
        )
        .expect("key");
        let message = b"tartan";
        let signature: k256::ecdsa::Signature = signing.sign(message);
        let low = signature.normalize_s().unwrap_or(signature);
        assert!(key.verify(message, &low.to_bytes()).is_ok());
        let (r, s) = low.split_scalars();
        let high = k256::ecdsa::Signature::from_scalars(r, -*s).expect("high-S");
        assert!(key.verify(message, &high.to_bytes()).is_err());
        assert!(key.verify(b"other", &low.to_bytes()).is_err());
    }
}
