//! Deterministic secp256k1 keys derived from the devnet's seed.
//!
//! Every key is `SHA-256("tartan-devnet" ‖ seed ‖ label ‖ counter)`, taking the first counter
//! whose hash is a valid scalar, so the same seed always gives the same people and DIDs.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use k256::ecdsa::signature::Signer as _;
use k256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};
use tartan_atproto::keys::{AtprotoKey, Curve};

/// Derives the k256 key named `label` from `seed`.
pub(crate) fn derive(seed: &str, label: &str) -> SigningKey {
    (0u32..)
        .find_map(|counter| {
            let mut hash = Sha256::new();
            for part in ["tartan-devnet", seed, label] {
                hash.update((part.len() as u64).to_be_bytes());
                hash.update(part.as_bytes());
            }
            hash.update(counter.to_be_bytes());
            SigningKey::from_slice(&hash.finalize()).ok()
        })
        .expect("a valid scalar is found within a few attempts")
}

/// The public half of `key` as an ATProto key.
pub(crate) fn public(key: &SigningKey) -> AtprotoKey {
    AtprotoKey::from_sec1(
        Curve::K256,
        key.verifying_key().to_encoded_point(true).as_bytes(),
    )
    .expect("a k256 verifying key is a valid point")
}

/// `did:key:` of `key`.
pub(crate) fn did_key(key: &SigningKey) -> String {
    public(key).did_key()
}

/// A compact low-S signature of `message` (hashed with SHA-256), as unpadded base64url.
pub(crate) fn sign_base64url(key: &SigningKey, message: &[u8]) -> String {
    let signature: Signature = key.sign(message);
    let signature = signature.normalize_s().unwrap_or(signature);
    URL_SAFE_NO_PAD.encode(signature.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let a = derive("dev", "alice.test/atproto/0");
        let b = derive("dev", "alice.test/atproto/0");
        let c = derive("other", "alice.test/atproto/0");
        assert_eq!(a.to_bytes(), b.to_bytes());
        assert_ne!(a.to_bytes(), c.to_bytes());
    }

    #[test]
    fn signatures_verify_and_are_low_s() {
        let key = derive("dev", "test");
        let signature = URL_SAFE_NO_PAD
            .decode(sign_base64url(&key, b"hello"))
            .expect("base64");
        public(&key).verify(b"hello", &signature).expect("verifies");
    }
}
