//! Signatures in Git objects (design D8, *Per object*, step 1).
//!
//! The rules follow Git itself, so the forge judges exactly the bytes Git handed `tt` to sign:
//!
//! - **Commits** (`parse_buffer_signed_by_header` in Git's `commit.c`): the signature is the
//!   `gpgsig` header for the repository's hash algorithm (`gpgsig` for SHA-1, `gpgsig-sha256`
//!   for SHA-256), continuation lines included; every header starting with `gpgsig` (the other
//!   algorithm's signature of a repository with `compatObjectFormat`, too) is removed from the
//!   signed data. Git concatenates repeated headers; Tartan refuses a commit with more than one
//!   header for its algorithm as `bad_signature`.
//! - **Tags** (`parse_signature` in Git's `gpg-interface.c`): the signature starts at the last
//!   line that begins with any signature marker Git knows (PGP, X.509 or SSH), so a message that
//!   quotes a marker earlier is still parsed correctly. The signed data is everything before it,
//!   with any `gpgsig`/`gpgsig-sha256` header removed (`remove_signature`). Such a header in a
//!   tag is the signature for the *other* hash algorithm, written by Git's `compatObjectFormat`
//!   support (`do_sign` in `builtin/tag.c`); as in Git, it is never the tag's own signature, so a
//!   tag that carries only a header is unsigned.
//!
//! gix-object's `CommitRefIter::signature` returns only the first `gpgsig` header and knows
//! neither the algorithm rule nor tag armour other than PGP, so the parsing is done here.

use crate::Mode;
use crate::signature::block::{BEGIN, BlockError, SignatureBlock, VERSION_PREFIX};

/// The repository's object hash algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    /// SHA-1 repositories: the signature header is `gpgsig`.
    Sha1,
    /// SHA-256 repositories: the signature header is `gpgsig-sha256`.
    Sha256,
}

impl HashAlgorithm {
    fn header(self) -> &'static [u8] {
        match self {
            HashAlgorithm::Sha1 => b"gpgsig",
            HashAlgorithm::Sha256 => b"gpgsig-sha256",
        }
    }
}

/// The signature markers Git knows (`gpg-interface.c`).
pub const SIGNATURE_MARKERS: &[&str] = &[
    "-----BEGIN PGP SIGNATURE-----",
    "-----BEGIN PGP MESSAGE-----",
    "-----BEGIN SIGNED MESSAGE-----",
    "-----BEGIN SSH SIGNATURE-----",
];

/// An object split into what was signed and its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// The signed data: the object without its signature, as Git gave it to the signing
    /// program.
    pub payload: Vec<u8>,
    /// The signature, if any.
    pub signature: Option<Vec<u8>>,
}

/// A commit with more than one signature header for its algorithm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the commit has more than one signature header")]
pub struct MultipleSignatures;

/// Splits a raw commit (the object's content, without the `commit <size>\0` prefix).
pub fn extract_commit(
    raw: &[u8],
    algorithm: HashAlgorithm,
) -> Result<Extracted, MultipleSignatures> {
    let own = algorithm.header();
    let mut payload = Vec::with_capacity(raw.len());
    let mut signature = Vec::new();
    let mut headers = 0usize;
    let (mut in_signature, mut other_signature) = (false, false);
    let mut rest = raw;
    while !rest.is_empty() {
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .map_or(rest.len(), |i| i + 1);
        let line = &rest[..end];
        let mut sig_part: Option<&[u8]> = None;
        if in_signature && line.first() == Some(&b' ') {
            sig_part = Some(&line[1..]);
        } else if line.starts_with(own) && line.get(own.len()) == Some(&b' ') {
            sig_part = Some(&line[own.len() + 1..]);
            headers += 1;
            other_signature = false;
        } else if line.starts_with(b"gpgsig") {
            other_signature = true;
        } else if other_signature && line.first() != Some(&b' ') {
            other_signature = false;
        }
        match sig_part {
            Some(part) => {
                signature.extend_from_slice(part);
                in_signature = true;
            }
            None => {
                if line.first() == Some(&b'\n') {
                    // The blank line ends the headers: the rest is the message, all signed.
                    payload.extend_from_slice(rest);
                    break;
                }
                if !other_signature {
                    payload.extend_from_slice(line);
                }
                in_signature = false;
            }
        }
        rest = &rest[end..];
    }
    if headers > 1 {
        return Err(MultipleSignatures);
    }
    Ok(Extracted {
        payload,
        signature: (headers == 1).then_some(signature),
    })
}

/// Splits a raw annotated tag (the object's content, without the `tag <size>\0` prefix).
pub fn extract_tag(raw: &[u8]) -> Extracted {
    let mut start = 0usize;
    let mut found: Option<usize> = None;
    while start < raw.len() {
        if SIGNATURE_MARKERS
            .iter()
            .any(|marker| raw[start..].starts_with(marker.as_bytes()))
        {
            found = Some(start);
        }
        start += raw[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(raw.len() - start, |i| i + 1);
    }
    match found {
        Some(at) => Extracted {
            payload: remove_signature_headers(&raw[..at]),
            signature: Some(raw[at..].to_vec()),
        },
        None => Extracted {
            payload: raw.to_vec(),
            signature: None,
        },
    }
}

/// Git's `remove_signature`: drops `gpgsig` and `gpgsig-sha256` headers (with their
/// continuation lines) from the header block.
fn remove_signature_headers(buffer: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(buffer.len());
    let mut in_signature = false;
    let mut rest = buffer;
    while !rest.is_empty() {
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .map_or(rest.len(), |i| i + 1);
        let line = &rest[..end];
        let header = [&b"gpgsig "[..], &b"gpgsig-sha256 "[..]]
            .iter()
            .any(|prefix| line.starts_with(prefix));
        if in_signature && line.first() == Some(&b' ') {
            // Continuation of a removed header.
        } else if header {
            in_signature = true;
        } else {
            if line.first() == Some(&b'\n') {
                out.extend_from_slice(rest);
                break;
            }
            in_signature = false;
            out.extend_from_slice(line);
        }
        rest = &rest[end..];
    }
    out
}

/// What kind of signature an object carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureClass {
    /// A Tartan signature that parses.
    Tartan(SignatureBlock),
    /// A block that claims `Tartan-Signature` but breaks a rule (`bad_signature`).
    BadTartan(BlockError),
    /// Another X.509 signature, such as gitsign's CMS (`unsupported_format`).
    OtherX509,
    /// An OpenPGP signature (`unsupported_format`).
    Pgp,
    /// An SSH signature (`unsupported_format`).
    Ssh,
    /// Something else (`unsupported_format`).
    Unknown,
}

/// Classifies a signature by its armour. An X.509 block whose second line starts with
/// `Tartan-Signature: ` is Tartan's and is parsed strictly.
pub fn classify(signature: &[u8], mode: Mode) -> SignatureClass {
    let first_line_end = signature
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(signature.len());
    let first = &signature[..first_line_end];
    let starts = |marker: &str| first.starts_with(marker.as_bytes());
    if starts(BEGIN) {
        let second = signature.get(first_line_end + 1..).unwrap_or_default();
        if second.starts_with(VERSION_PREFIX.as_bytes()) {
            match SignatureBlock::parse(signature, mode) {
                Ok(block) => SignatureClass::Tartan(block),
                Err(error) => SignatureClass::BadTartan(error),
            }
        } else {
            SignatureClass::OtherX509
        }
    } else if starts("-----BEGIN PGP SIGNATURE-----") || starts("-----BEGIN PGP MESSAGE-----") {
        SignatureClass::Pgp
    } else if starts("-----BEGIN SSH SIGNATURE-----") {
        SignatureClass::Ssh
    } else {
        SignatureClass::Unknown
    }
}

#[cfg(test)]
mod tests;
