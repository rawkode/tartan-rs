//! The signature Git stores (design D7, *The signature Git stores*).
//!
//! `tt` writes exactly six lines, each ending in LF:
//!
//! ```text
//! -----BEGIN SIGNED MESSAGE-----
//! Tartan-Signature: 1
//! Record: at://<did>/academy.rawkode.tartan.signature/<tid>
//! CID: <CIDv1, dag-cbor, sha2-256, lowercase base32>
//! Salt: <32 bytes, unpadded base64url, 43 characters>
//! -----END SIGNED MESSAGE-----
//! ```
//!
//! The parser is strict, and every field must equal its canonical re-encoding byte for byte, so
//! each accepted block has exactly one spelling and nobody can mint a second object id with the
//! same payload and a still-valid signature.

use cid::Cid;

use crate::Mode;
use crate::did::Did;
use crate::ipld::{DAG_CBOR, SHA2_256};
use crate::signature::COLLECTION;
use crate::signature::record::Salt;
use crate::signature::tid::Tid;

/// The first line of every X.509 signature Git recognises.
pub const BEGIN: &str = "-----BEGIN SIGNED MESSAGE-----";
/// The last line.
pub const END: &str = "-----END SIGNED MESSAGE-----";
/// The prefix of the second line that makes an X.509 block a Tartan signature.
pub const VERSION_PREFIX: &str = "Tartan-Signature: ";
/// The largest block accepted: the longest legal block, whose `Record:` line names a `did:web`
/// DID with a 253-character host.
pub const MAX_BLOCK_BYTES: usize = 517;

/// A parsed Tartan signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureBlock {
    /// The signer, always a DID.
    pub did: Did,
    /// The record key.
    pub rkey: Tid,
    /// The record's CID.
    pub cid: Cid,
    /// The HMAC key of the record's digest.
    pub salt: Salt,
}

/// Which rule a block that claims `Tartan-Signature` breaks (a `bad_signature`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    /// Longer than [`MAX_BLOCK_BYTES`].
    #[error("the signature is longer than 517 bytes")]
    TooLong,
    /// Not UTF-8, or a carriage return.
    #[error("the signature must be UTF-8 with LF line endings")]
    Encoding,
    /// Not exactly six LF-terminated lines.
    #[error("the signature must be exactly six lines")]
    LineCount,
    /// The armour lines are wrong.
    #[error("the signature's armour lines are wrong")]
    Armour,
    /// The version is not `1`.
    #[error("unsupported Tartan-Signature version")]
    Version,
    /// The `Record:` line is missing or malformed.
    #[error("the Record line is malformed")]
    Record,
    /// The record URI's authority is not a `did:plc` or hostname-only `did:web` DID.
    #[error("the record URI must name a did:plc or did:web DID")]
    Authority,
    /// The record URI names another collection.
    #[error("the record URI must name the academy.rawkode.tartan.signature collection")]
    Collection,
    /// The record key is not a TID.
    #[error("the record key must be a TID")]
    RecordKey,
    /// The `CID:` line is missing, not a dag-cbor sha2-256 CIDv1, or not canonical.
    #[error("the CID line must be a canonical dag-cbor sha2-256 CIDv1")]
    Cid,
    /// The `Salt:` line is missing, not 43 base64url characters decoding to 32 bytes, or not
    /// canonical.
    #[error("the Salt line must be 43 canonical base64url characters")]
    Salt,
}

impl SignatureBlock {
    /// The record's `at://` URI.
    pub fn uri(&self) -> String {
        format!("at://{}/{COLLECTION}/{}", self.did, self.rkey)
    }

    /// Writes the six-line block.
    pub fn write(&self) -> String {
        format!(
            "{BEGIN}\n{VERSION_PREFIX}1\nRecord: {}\nCID: {}\nSalt: {}\n{END}\n",
            self.uri(),
            self.cid,
            self.salt.to_base64url()
        )
    }

    /// Parses a block strictly under the DID rules of `mode`.
    pub fn parse(block: &[u8], mode: Mode) -> Result<Self, BlockError> {
        if block.len() > MAX_BLOCK_BYTES {
            return Err(BlockError::TooLong);
        }
        let text = std::str::from_utf8(block).map_err(|_| BlockError::Encoding)?;
        if text.contains('\r') {
            return Err(BlockError::Encoding);
        }
        let body = text.strip_suffix('\n').ok_or(BlockError::LineCount)?;
        let lines: Vec<&str> = body.split('\n').collect();
        let [begin, version, record, cid, salt, end] = lines[..] else {
            return Err(BlockError::LineCount);
        };
        if begin != BEGIN || end != END {
            return Err(BlockError::Armour);
        }
        if version != format!("{VERSION_PREFIX}1") {
            return Err(BlockError::Version);
        }

        let uri = record.strip_prefix("Record: ").ok_or(BlockError::Record)?;
        let path = uri.strip_prefix("at://").ok_or(BlockError::Record)?;
        let mut parts = path.split('/');
        let (Some(authority), Some(collection), Some(rkey), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(BlockError::Record);
        };
        let did = Did::parse(authority, mode).map_err(|_| BlockError::Authority)?;
        if collection != COLLECTION {
            return Err(BlockError::Collection);
        }
        let rkey = Tid::parse(rkey).map_err(|_| BlockError::RecordKey)?;

        let cid_text = cid.strip_prefix("CID: ").ok_or(BlockError::Cid)?;
        let cid = Cid::try_from(cid_text).map_err(|_| BlockError::Cid)?;
        let canonical_cid = cid.version() == cid::Version::V1
            && cid.codec() == DAG_CBOR
            && cid.hash().code() == SHA2_256
            && cid.hash().size() == 32
            && cid.to_string() == cid_text;
        if !canonical_cid {
            return Err(BlockError::Cid);
        }

        let salt_text = salt.strip_prefix("Salt: ").ok_or(BlockError::Salt)?;
        let salt = Salt::from_base64url(salt_text).ok_or(BlockError::Salt)?;

        let parsed = SignatureBlock {
            did,
            rkey,
            cid,
            salt,
        };
        // Every field has been checked against its canonical form; the whole block must be what
        // the writer writes.
        if parsed.write() != text {
            return Err(BlockError::Record);
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::record::SignatureRecord;
    use base64::Engine as _;
    use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
    use cid::multibase;

    fn sample(did: &str) -> SignatureBlock {
        let salt = Salt::from_bytes([42; 32]);
        let record = SignatureRecord::for_git(b"payload", &salt, "2026-11-09T12:00:00Z".into());
        SignatureBlock {
            did: Did::parse(did, Mode::Production).expect("did"),
            rkey: Tid::parse("3lbmxyz2abc2k").expect("tid"),
            cid: record.cid(),
            salt,
        }
    }

    const DID: &str = "did:plc:abcdefghijklmnopqrstuvwx";

    #[test]
    fn writer_output_parses() {
        let block = sample(DID);
        let text = block.write();
        assert_eq!(text.lines().count(), 6);
        assert!(text.starts_with("-----BEGIN SIGNED MESSAGE-----\nTartan-Signature: 1\nRecord: at://did:plc:abcdefghijklmnopqrstuvwx/academy.rawkode.tartan.signature/3lbmxyz2abc2k\nCID: bafyrei"));
        assert_eq!(
            SignatureBlock::parse(text.as_bytes(), Mode::Production),
            Ok(block)
        );
        let web = sample("did:web:rawkode.dev");
        assert_eq!(
            SignatureBlock::parse(web.write().as_bytes(), Mode::Production),
            Ok(web)
        );
    }

    #[test]
    fn longest_legal_block_fits() {
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}.com", "b".repeat(57));
        assert_eq!(host.len(), 253);
        let block = sample(&format!("did:web:{host}"));
        let text = block.write();
        assert_eq!(text.len(), MAX_BLOCK_BYTES);
        assert!(SignatureBlock::parse(text.as_bytes(), Mode::Production).is_ok());
    }

    fn replace_line(text: &str, index: usize, line: &str) -> String {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines[index] = line.to_owned();
        lines.join("\n") + "\n"
    }

    #[test]
    fn each_rule_refuses() {
        let block = sample(DID);
        let good = block.write();
        let uri_with = |authority: &str, collection: &str, rkey: &str| {
            format!("Record: at://{authority}/{collection}/{rkey}")
        };
        let cid_v0 = {
            // A CIDv0 (dag-pb, base58btc `Qm…`) of some bytes.
            let hash = cid::multihash::Multihash::<64>::wrap(SHA2_256, &[1u8; 32]).expect("hash");
            Cid::new_v0(hash).expect("v0").to_string()
        };
        let cid_base58 = block
            .cid
            .to_string_of_base(multibase::Base::Base58Btc)
            .expect("base58");
        let cid_upper = block
            .cid
            .to_string_of_base(multibase::Base::Base32Upper)
            .expect("base32upper");
        let raw_cid = crate::ipld::cid_for(crate::ipld::RAW, b"x").to_string();
        let padded_salt = format!("Salt: {}", URL_SAFE.encode([42u8; 32]));
        let short_salt = format!("Salt: {}", URL_SAFE_NO_PAD.encode([42u8; 31]));
        let trailing_bits = {
            let mut text = block.salt.to_base64url();
            // 43 characters carry 258 bits; the last character's two spare bits must be zero.
            let last = text.pop().expect("char");
            let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let index = alphabet
                .iter()
                .position(|&c| c == last as u8)
                .expect("alphabet");
            text.push(alphabet[index | 1] as char);
            format!("Salt: {text}")
        };

        let cases: Vec<(String, BlockError)> = vec![
            (
                replace_line(
                    &good,
                    2,
                    &uri_with("alice.example.com", COLLECTION, "3lbmxyz2abc2k"),
                ),
                BlockError::Authority,
            ),
            (
                replace_line(
                    &good,
                    2,
                    &uri_with(
                        "did:key:zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ",
                        COLLECTION,
                        "3lbmxyz2abc2k",
                    ),
                ),
                BlockError::Authority,
            ),
            (
                replace_line(
                    &good,
                    2,
                    &uri_with(DID, "app.bsky.feed.post", "3lbmxyz2abc2k"),
                ),
                BlockError::Collection,
            ),
            (
                replace_line(&good, 2, &uri_with(DID, COLLECTION, "self")),
                BlockError::RecordKey,
            ),
            (
                replace_line(&good, 2, &format!("Record: at://{DID}/{COLLECTION}")),
                BlockError::Record,
            ),
            (
                replace_line(&good, 3, &format!("CID: {cid_v0}")),
                BlockError::Cid,
            ),
            (
                replace_line(&good, 3, &format!("CID: {cid_base58}")),
                BlockError::Cid,
            ),
            (
                replace_line(&good, 3, &format!("CID: {cid_upper}")),
                BlockError::Cid,
            ),
            (
                replace_line(&good, 3, &format!("CID: {raw_cid}")),
                BlockError::Cid,
            ),
            (replace_line(&good, 4, &padded_salt), BlockError::Salt),
            (replace_line(&good, 4, &short_salt), BlockError::Salt),
            (replace_line(&good, 4, &trailing_bits), BlockError::Salt),
            (
                replace_line(&good, 1, "Tartan-Signature: 2"),
                BlockError::Version,
            ),
            (
                replace_line(&good, 0, "-----BEGIN PGP SIGNATURE-----"),
                BlockError::Armour,
            ),
            (good.replace('\n', "\r\n"), BlockError::Encoding),
            (format!("{good}extra\n"), BlockError::LineCount),
            (
                good.trim_end_matches('\n').to_owned(),
                BlockError::LineCount,
            ),
            (
                good.lines()
                    .filter(|line| !line.starts_with("Salt: "))
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
                BlockError::LineCount,
            ),
            (
                replace_line(&good, 4, &format!("Salt:  {}", block.salt.to_base64url())),
                BlockError::Salt,
            ),
            (format!("{good}{}", "x".repeat(600)), BlockError::TooLong),
        ];
        for (text, error) in cases {
            assert_eq!(
                SignatureBlock::parse(text.as_bytes(), Mode::Production),
                Err(error.clone()),
                "{error:?}\n{text}"
            );
        }
        assert_eq!(
            SignatureBlock::parse(&[0xff, 0xfe], Mode::Production),
            Err(BlockError::Encoding)
        );
    }

    #[test]
    fn localhost_did_web_only_in_development() {
        let mut block = sample(DID);
        let text = block.write().replace(DID, "did:web:localhost%3A9400");
        assert_eq!(
            SignatureBlock::parse(text.as_bytes(), Mode::Production),
            Err(BlockError::Authority)
        );
        #[cfg(feature = "dev-identity")]
        {
            block.did = Did::parse("did:web:localhost%3A9400", Mode::Development).expect("did");
            assert_eq!(
                SignatureBlock::parse(text.as_bytes(), Mode::Development),
                Ok(block)
            );
        }
        #[cfg(not(feature = "dev-identity"))]
        let _ = &mut block;
    }
}
