//! ES256 JWTs: DPoP proofs (RFC 9449) and `private_key_jwt` client assertions (RFC 7523).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jose_jwa::{Algorithm, Signing};
use jose_jwk::crypto::Kind;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// A decoded, not yet verified, compact JWS.
pub(crate) struct Jwt {
    pub header: Map<String, Value>,
    pub claims: Map<String, Value>,
    signing_input: String,
    signature: Vec<u8>,
}

/// Splits and decodes `token`.
pub(crate) fn decode(token: &str) -> Option<Jwt> {
    let mut parts = token.split('.');
    let (header, claims, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let object = |part: &str| -> Option<Map<String, Value>> {
        match serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).ok()?).ok()? {
            Value::Object(map) => Some(map),
            _ => None,
        }
    };
    Some(Jwt {
        header: object(header)?,
        claims: object(claims)?,
        signing_input: format!("{header}.{claims}"),
        signature: URL_SAFE_NO_PAD.decode(signature).ok()?,
    })
}

impl Jwt {
    /// True when the header's `alg` is ES256.
    pub(crate) fn is_es256(&self) -> bool {
        self.header
            .get("alg")
            .cloned()
            .and_then(|alg| serde_json::from_value::<Algorithm>(alg).ok())
            == Some(Algorithm::Signing(Signing::Es256))
    }

    /// Verifies the signature with `key`.
    pub(crate) fn verify(&self, key: &p256::PublicKey) -> bool {
        let Ok(signature) = Signature::from_slice(&self.signature) else {
            return false;
        };
        VerifyingKey::from(key)
            .verify(self.signing_input.as_bytes(), &signature)
            .is_ok()
    }

    pub(crate) fn claim_str(&self, name: &str) -> Option<&str> {
        self.claims.get(name).and_then(Value::as_str)
    }

    pub(crate) fn claim_i64(&self, name: &str) -> Option<i64> {
        self.claims.get(name).and_then(Value::as_i64)
    }
}

/// The P-256 public key of a JWK, refusing private keys and other key types.
pub(crate) fn public_key(jwk: &Value) -> Option<p256::PublicKey> {
    if jwk.get("d").is_some() {
        return None;
    }
    let jwk: jose_jwk::Jwk = serde_json::from_value(jwk.clone()).ok()?;
    let jose_jwk::Key::Ec(ec) = &jwk.key else {
        return None;
    };
    match Kind::<p256::PublicKey, p256::SecretKey>::try_from(ec).ok()? {
        Kind::Public(key) => Some(key),
        Kind::Secret(_) => None,
    }
}

/// The RFC 7638 thumbprint of an EC JWK.
pub(crate) fn thumbprint(jwk: &Value) -> Option<String> {
    let canonical = json!({
        "crv": jwk.get("crv")?.as_str()?,
        "kty": jwk.get("kty")?.as_str()?,
        "x": jwk.get("x")?.as_str()?,
        "y": jwk.get("y")?.as_str()?,
    });
    // serde_json's map is ordered by key, as RFC 7638 requires.
    Some(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.to_string())))
}

/// A verified DPoP proof.
#[derive(Debug, Clone)]
pub(crate) struct DpopProof {
    /// The thumbprint of the proof's key.
    pub jkt: String,
    pub jti: String,
    pub nonce: Option<String>,
    pub ath: Option<String>,
}

/// How far a proof's `iat` may be from now.
const MAX_IAT_SKEW_SECS: i64 = 300;

/// Verifies `proof` for a request of `method` to `url` (compared without query or fragment).
pub(crate) fn verify_dpop(
    proof: &str,
    method: &str,
    url: &str,
    now_secs: i64,
) -> Result<DpopProof, String> {
    let jwt = decode(proof).ok_or("the DPoP proof is not a JWT")?;
    if jwt.header.get("typ").and_then(Value::as_str) != Some("dpop+jwt") {
        return Err("the DPoP proof's typ is not dpop+jwt".into());
    }
    if !jwt.is_es256() {
        return Err("the DPoP proof is not ES256".into());
    }
    let jwk = jwt.header.get("jwk").ok_or("the DPoP proof has no jwk")?;
    let key = public_key(jwk).ok_or("the DPoP proof's jwk is not a P-256 public key")?;
    if !jwt.verify(&key) {
        return Err("the DPoP proof's signature does not verify".into());
    }
    if jwt.claim_str("htm") != Some(method) {
        return Err("the DPoP proof's htm does not match".into());
    }
    let strip = |u: &str| u.split(['?', '#']).next().unwrap_or_default().to_owned();
    if jwt.claim_str("htu").map(strip) != Some(strip(url)) {
        return Err("the DPoP proof's htu does not match".into());
    }
    let iat = jwt.claim_i64("iat").ok_or("the DPoP proof has no iat")?;
    if (now_secs - iat).abs() > MAX_IAT_SKEW_SECS {
        return Err("the DPoP proof's iat is too far from now".into());
    }
    let jti = jwt
        .claim_str("jti")
        .filter(|jti| !jti.is_empty())
        .ok_or("the DPoP proof has no jti")?;
    Ok(DpopProof {
        jkt: thumbprint(jwk).ok_or("the DPoP proof's jwk has no thumbprint")?,
        jti: jti.to_owned(),
        nonce: jwt
            .claim_str("nonce")
            .filter(|nonce| !nonce.is_empty())
            .map(str::to_owned),
        ath: jwt.claim_str("ath").map(str::to_owned),
    })
}

/// `ath`: the unpadded base64url SHA-256 of an access token.
pub(crate) fn ath(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
pub(crate) mod testing {
    //! Builds ES256 JWTs for the devnet's own tests.

    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer as _;

    pub(crate) fn public_jwk(key: &SigningKey) -> Value {
        let point = key.verifying_key().to_encoded_point(false);
        json!({
            "kty": "EC",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(point.x().expect("x")),
            "y": URL_SAFE_NO_PAD.encode(point.y().expect("y")),
        })
    }

    pub(crate) fn sign(key: &SigningKey, header: &Value, claims: &Value) -> String {
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature: Signature = key.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use p256::ecdsa::SigningKey;

    fn key() -> SigningKey {
        SigningKey::from_slice(&[7u8; 32]).expect("key")
    }

    fn proof(claims: Value) -> String {
        let key = key();
        sign(
            &key,
            &json!({"typ": "dpop+jwt", "alg": "ES256", "jwk": public_jwk(&key)}),
            &claims,
        )
    }

    #[test]
    fn accepts_a_valid_proof() {
        let proof = proof(json!({
            "htm": "POST", "htu": "http://127.0.0.1:1/oauth/token", "iat": 1000,
            "jti": "a", "nonce": "n"
        }));
        let verified =
            verify_dpop(&proof, "POST", "http://127.0.0.1:1/oauth/token", 1010).expect("valid");
        assert_eq!(verified.nonce.as_deref(), Some("n"));
        assert_eq!(verified.jkt, thumbprint(&public_jwk(&key())).expect("jkt"));
    }

    #[test]
    fn refuses_wrong_method_url_and_time() {
        let proof = proof(json!({"htm": "POST", "htu": "http://h/x", "iat": 1000, "jti": "a"}));
        assert!(verify_dpop(&proof, "GET", "http://h/x", 1000).is_err());
        assert!(verify_dpop(&proof, "POST", "http://h/y", 1000).is_err());
        assert!(verify_dpop(&proof, "POST", "http://h/x", 5000).is_err());
        assert!(verify_dpop(&proof, "POST", "http://h/x?q=1", 1000).is_ok());
    }

    #[test]
    fn refuses_a_tampered_proof() {
        let proof = proof(json!({"htm": "POST", "htu": "http://h/x", "iat": 1000, "jti": "a"}));
        let mut parts: Vec<&str> = proof.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(
            json!({"htm": "GET", "htu": "http://h/x", "iat": 1000, "jti": "a"}).to_string(),
        );
        parts[1] = &forged;
        assert!(verify_dpop(&parts.join("."), "GET", "http://h/x", 1000).is_err());
    }
}
