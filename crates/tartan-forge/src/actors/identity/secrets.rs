//! Secrets of sessions, tokens and CLI codes: 32 bytes from the OS random number generator,
//! written as unpadded base64url, of which only the SHA-256 is ever stored.

use anyhow::{Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// 32 random bytes as 43 unpadded base64url characters.
pub(crate) fn random_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("the OS random source failed: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The SHA-256 of `secret`.
pub(crate) fn sha256(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

/// A hex-encoded SHA-256 as bytes, or `None` when it is not 64 hex characters.
pub(crate) fn digest_from_hex(hex_digest: &str) -> Option<Vec<u8>> {
    let bytes = hex::decode(hex_digest).ok()?;
    (bytes.len() == 32).then_some(bytes)
}

/// The PKCE S256 challenge of `verifier`: base64url of its SHA-256 (RFC 7636).
pub(crate) fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Compares two byte strings in time that depends only on their lengths.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Whether `value` is made only of base64url characters.
pub(crate) fn is_base64url(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_43_base64url_characters_and_differ() {
        let a = random_secret().expect("random");
        let b = random_secret().expect("random");
        assert_eq!(a.len(), 43);
        assert!(is_base64url(&a));
        assert_ne!(a, b);
    }

    #[test]
    fn s256_matches_rfc_7636_appendix_b() {
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn digests_must_be_32_bytes_of_hex() {
        assert!(digest_from_hex(&hex::encode(sha256("x"))).is_some());
        assert!(digest_from_hex("abcd").is_none());
        assert!(digest_from_hex("zz").is_none());
    }

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
