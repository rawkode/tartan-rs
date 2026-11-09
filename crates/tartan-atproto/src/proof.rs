//! Proving a record against the signer's repository (design D8, *Per object*, step 5).
//!
//! [`fetch_record`] asks the signer's PDS for `com.atproto.sync.getRecord`, a CAR holding the
//! signed repository commit, the MST path to the record and the record block. [`prove_record`]
//! checks it without trusting the PDS:
//!
//! 1. The CAR is at most [`MAX_CAR_BYTES`] and [`MAX_CAR_BLOCKS`] blocks. Every block is
//!    re-hashed (jacquard's CAR reader does not), and only sha2-256 CIDv1 blocks with the
//!    dag-cbor or raw codec are allowed.
//! 2. The root is a commit with `version` 3, the expected DID and a `rev` at most five minutes in
//!    the future, signed by the DID's `#atproto` key. The signature is checked with atrium-crypto,
//!    which refuses high-S (jacquard's `Commit::verify` with p256 0.13 accepts it).
//! 3. The record's CID comes from the signed MST (`Mst::get`), and jacquard-repo's
//!    `verify_proofs` must verify the same claim, for presence or, with `cid: None`, absence.
//!    `verify_proofs` reads a missing MST block as "absent", so absence is accepted only when
//!    `Mst::get` itself walked the whole path; a missing block makes the proof incomplete.
//!
//! The four outcomes stay distinct ([`ProofOutcome`]). The work runs in a spawned task, so a
//! panic inside the CAR, MST or crypto code becomes an error rather than taking the caller down.

use std::collections::BTreeMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cid::Cid;
use jacquard_common::CowStr;
use jacquard_common::deps::bytes::Bytes;
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_common::types::did::Did as JDid;
use jacquard_repo::commit::proof::{RecordClaim, verify_proofs};
use jacquard_repo::commit::{Commit, UnsignedCommit};
use jacquard_repo::storage::MemoryBlockStore;
use jacquard_repo::{Mst, RepoErrorKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::did::Did;
use crate::fetch::FetchClient;
use crate::ipld::block_matches;
use crate::keys::AtprotoKey;
use crate::resolver::{Evidence, Resolved};
use crate::signature::COLLECTION;
use crate::signature::block::SignatureBlock;
use crate::signature::record::SignatureRecord;
use crate::signature::tid::Tid;

/// The largest `getRecord` answer accepted.
pub const MAX_CAR_BYTES: usize = 1024 * 1024;
/// The most blocks a proof CAR may hold.
pub const MAX_CAR_BLOCKS: usize = 2000;
/// How far in the future a commit's `rev` may be.
pub const MAX_REV_SKEW_MS: u64 = 5 * 60 * 1000;

/// The repository commit a proof verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvedCommit {
    /// The commit's CID.
    pub cid: Cid,
    /// The commit's revision (a TID).
    pub rev: String,
}

/// Why a proof did not verify, by the step that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProofFailure {
    /// Step 5.2: the CAR is too large, a block does not hash to its CID, or a codec is not
    /// allowed. The PDS answered inconsistently; no identity refresh helps.
    #[error("the CAR is invalid: {0}")]
    Car(String),
    /// Step 5.3: the commit's version, DID, rev or signature is wrong. Stale identity evidence
    /// can cause this, so the caller refreshes the DID and retries once.
    #[error("the repository commit does not verify: {0}")]
    Commit(String),
    /// Step 5.4: the MST does not prove the claim. Also retried after an identity refresh.
    #[error("the record proof does not verify: {0}")]
    Mst(String),
}

impl ProofFailure {
    /// True when design D8 asks for an identity refresh and one retry.
    pub fn needs_identity_refresh(&self) -> bool {
        !matches!(self, ProofFailure::Car(_))
    }
}

/// What a proof established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofOutcome {
    /// The repository holds the record, with this CID and block (re-hashed).
    Present {
        /// The record's CID, from the signed MST.
        record_cid: Cid,
        /// The record block.
        record: Vec<u8>,
        /// The commit that proved it.
        commit: ProvedCommit,
    },
    /// The repository, as of `commit`, provably does not hold the record.
    Absent {
        /// The commit that proved it.
        commit: ProvedCommit,
    },
    /// A block needed to decide is missing (a transient failure, without identity refresh).
    Incomplete(String),
    /// The proof does not verify.
    Invalid(ProofFailure),
}

/// What to prove.
#[derive(Debug, Clone)]
pub struct RecordRef {
    /// The repository's DID.
    pub did: Did,
    /// The collection.
    pub collection: String,
    /// The record key.
    pub rkey: String,
}

/// Proves `record` from `car` against `key` (the DID's `#atproto` key) at time `now_ms`.
pub async fn prove_record(
    car: Vec<u8>,
    record: RecordRef,
    key: AtprotoKey,
    now_ms: u64,
) -> ProofOutcome {
    let task = tokio::spawn(async move { prove(&car, &record, &key, now_ms).await });
    match task.await {
        Ok(outcome) => outcome,
        Err(error) => ProofOutcome::Invalid(ProofFailure::Car(format!(
            "proof verification failed unexpectedly: {error}"
        ))),
    }
}

async fn prove(car: &[u8], record: &RecordRef, key: &AtprotoKey, now_ms: u64) -> ProofOutcome {
    use ProofFailure::{Car, Commit as CommitFailure, Mst as MstFailure};
    let invalid = ProofOutcome::Invalid;

    // 5.2: caps, then re-hash every block.
    if car.len() > MAX_CAR_BYTES {
        return invalid(Car(format!("larger than {MAX_CAR_BYTES} bytes")));
    }
    let parsed = match jacquard_repo::car::parse_car_bytes(car).await {
        Ok(parsed) => parsed,
        Err(error) => return invalid(Car(error.to_string())),
    };
    if parsed.blocks.len() > MAX_CAR_BLOCKS {
        return invalid(Car(format!("more than {MAX_CAR_BLOCKS} blocks")));
    }
    if let Some((cid, _)) = parsed
        .blocks
        .iter()
        .find(|(cid, block)| !block_matches(cid, block))
    {
        return invalid(Car(format!("block {cid} does not match its CID")));
    }

    // 5.3: the commit.
    let Some(commit_bytes) = parsed.blocks.get(&parsed.root) else {
        return ProofOutcome::Incomplete("the CAR does not hold its root commit".into());
    };
    let commit = match Commit::<SmolStr>::from_cbor(commit_bytes) {
        Ok(commit) => commit,
        Err(error) => return invalid(CommitFailure(format!("undecodable commit: {error}"))),
    };
    if commit.version != 3 {
        return invalid(CommitFailure(format!("version {}", commit.version)));
    }
    if commit.did.as_str() != record.did.as_str() {
        return invalid(CommitFailure(format!(
            "the commit is for {}",
            commit.did.as_str()
        )));
    }
    let rev_micros = match Tid::parse(commit.rev.as_str()) {
        Ok(rev) => rev.micros(),
        Err(_) => return invalid(CommitFailure("rev is not a TID".into())),
    };
    if rev_micros / 1000 > now_ms.saturating_add(MAX_REV_SKEW_MS) {
        return invalid(CommitFailure("rev is in the future".into()));
    }
    let unsigned = UnsignedCommit {
        did: commit.did.clone(),
        version: commit.version,
        data: commit.data,
        rev: commit.rev.clone(),
        prev: commit.prev,
    };
    let Ok(unsigned) = serde_ipld_dagcbor::to_vec(&unsigned) else {
        return invalid(CommitFailure("unencodable commit".into()));
    };
    if key.verify(&unsigned, &commit.sig).is_err() {
        return invalid(CommitFailure(format!(
            "signature does not verify against {}'s #atproto key",
            record.did
        )));
    }
    let proved = ProvedCommit {
        cid: parsed.root,
        rev: commit.rev.as_str().to_owned(),
    };

    // 5.4: the record's CID from the signed MST, then verify_proofs with that claim.
    let blocks: BTreeMap<Cid, Bytes> = parsed.blocks.clone();
    let store = Arc::new(MemoryBlockStore::new_from_blocks(blocks));
    let path = format!("{}/{}", record.collection, record.rkey);
    let found = match Mst::load(store, commit.data, None).get(&path).await {
        Ok(found) => found,
        Err(error) if matches!(error.kind(), RepoErrorKind::NotFound) => {
            return ProofOutcome::Incomplete(format!(
                "a block on the MST path is missing: {error}"
            ));
        }
        Err(error) => return invalid(MstFailure(error.to_string())),
    };
    let claim = RecordClaim {
        collection: CowStr::Owned(SmolStr::from(record.collection.as_str())),
        rkey: CowStr::Owned(SmolStr::from(record.rkey.as_str())),
        cid: found,
    };
    let Ok(did) = JDid::<SmolStr>::new_owned(record.did.as_str()) else {
        return invalid(CommitFailure("the DID is not valid for jacquard".into()));
    };
    match verify_proofs(car, vec![claim.clone()], &did, &key.to_jacquard()).await {
        Ok(output) if output.verified.contains(&claim) => {}
        Ok(_) => return invalid(MstFailure("the claim is not verified".into())),
        Err(error) => return invalid(MstFailure(error.to_string())),
    }

    match found {
        None => ProofOutcome::Absent { commit: proved },
        Some(record_cid) => match parsed.blocks.get(&record_cid) {
            Some(block) => ProofOutcome::Present {
                record_cid,
                record: block.to_vec(),
                commit: proved,
            },
            None => ProofOutcome::Incomplete("the CAR does not hold the record block".into()),
        },
    }
}

/// What a PDS answered to `com.atproto.sync.getRecord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordAnswer {
    /// A CAR to prove.
    Car(Vec<u8>),
    /// `RecordNotFound`, which some PDSes send instead of a non-existence proof (a transient
    /// failure, like a proved absence).
    RecordNotFound,
    /// `RepoDeactivated`, `RepoTakendown`, `RepoSuspended` or `RepoNotFound`: the caller
    /// refreshes the DID's account status.
    RepoUnavailable(String),
    /// The PDS could not be reached, timed out, or answered 5xx or 429.
    Unavailable(String),
    /// Any other answer.
    Failed(String),
}

/// Fetches the proof of `record` from `pds` (at most [`MAX_CAR_BYTES`]).
pub async fn fetch_record(fetch: &FetchClient, pds: &str, record: &RecordRef) -> RecordAnswer {
    let Ok(mut url) = reqwest::Url::parse(&format!("{pds}/xrpc/com.atproto.sync.getRecord")) else {
        return RecordAnswer::Failed(format!("invalid PDS endpoint {pds}"));
    };
    url.query_pairs_mut()
        .append_pair("did", record.did.as_str())
        .append_pair("collection", &record.collection)
        .append_pair("rkey", &record.rkey);
    let host = url.host_str().unwrap_or_default().to_owned();
    let fetched = match fetch.get(url.as_str(), MAX_CAR_BYTES).await {
        Ok(fetched) => fetched,
        Err(error) if error.is_transient() => return RecordAnswer::Unavailable(error.to_string()),
        Err(error) => return RecordAnswer::Failed(error.to_string()),
    };
    let status = fetched.status.as_u16();
    if fetched.status.is_success() {
        return RecordAnswer::Car(fetched.body);
    }
    if fetched.status.is_server_error() || status == 429 {
        return RecordAnswer::Unavailable(format!("PDS {host} answered {status}"));
    }
    let error = serde_json::from_slice::<serde_json::Value>(&fetched.body)
        .ok()
        .and_then(|body| body.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_default();
    match error.as_str() {
        "RecordNotFound" => RecordAnswer::RecordNotFound,
        "RepoDeactivated" | "RepoTakendown" | "RepoSuspended" | "RepoNotFound" => {
            RecordAnswer::RepoUnavailable(error)
        }
        _ => RecordAnswer::Failed(format!("PDS {host} answered {status} {error}")),
    }
}

/// What a proof says about a Tartan signature (design D8 steps 5.5 and 5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureJudgement {
    /// The proved record signs the payload.
    Verified {
        /// The decoded record.
        record: SignatureRecord,
        /// The commit that proved it.
        commit: ProvedCommit,
    },
    /// The repository holds a record under the key with another CID: it was replaced after
    /// signing. Confirm with fresh evidence before acting on it.
    Replaced {
        /// The CID the repository holds.
        found: Cid,
        /// The commit that proved it.
        commit: ProvedCommit,
    },
    /// The proved record does not sign the payload (conclusive: `bad_signature`).
    Mismatch(String),
    /// The repository provably does not hold the record now.
    Absent {
        /// The commit that proved it.
        commit: ProvedCommit,
    },
    /// A block is missing (transient, no identity refresh).
    Incomplete(String),
    /// The proof does not verify (transient; refresh the identity as the failure says).
    Invalid(ProofFailure),
}

/// Judges a proof outcome against the signature `block` and the signed `payload`.
pub fn judge_signature(
    outcome: ProofOutcome,
    block: &SignatureBlock,
    payload: &[u8],
) -> SignatureJudgement {
    match outcome {
        ProofOutcome::Present {
            record_cid,
            record,
            commit,
        } => {
            if record_cid != block.cid {
                return SignatureJudgement::Replaced {
                    found: record_cid,
                    commit,
                };
            }
            let decoded = match SignatureRecord::decode(&record) {
                Ok(decoded) => decoded,
                Err(error) => return SignatureJudgement::Mismatch(error.to_string()),
            };
            match decoded.check(payload, &block.salt) {
                Ok(()) => SignatureJudgement::Verified {
                    record: decoded,
                    commit,
                },
                Err(error) => SignatureJudgement::Mismatch(error.to_string()),
            }
        }
        ProofOutcome::Absent { commit } => SignatureJudgement::Absent { commit },
        ProofOutcome::Incomplete(reason) => SignatureJudgement::Incomplete(reason),
        ProofOutcome::Invalid(failure) => SignatureJudgement::Invalid(failure),
    }
}

/// The format name of [`ProofBundle`].
pub const BUNDLE_FORMAT: &str = "tartan.signature-proof/2";

/// The evidence behind a `verified` result (design D8, *Proof bundle*), stored content-addressed
/// at `proofs/sha256/<hex>.json`. Bytes are standard base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBundle {
    /// Always [`BUNDLE_FORMAT`].
    pub format: String,
    /// The signed object.
    pub object: BundleObject,
    /// The signer.
    pub did: String,
    /// The DID evidence.
    pub identity: BundleIdentity,
    /// The `#atproto` key as a `did:key`.
    pub atproto_key: String,
    /// The record.
    pub record: BundleRecord,
    /// The repository commit that proved it.
    pub repo_commit: BundleCommit,
    /// The `getRecord` answer.
    pub car: String,
    /// When the evidence was fetched (RFC 3339).
    pub fetched_at: String,
}

/// The signed object in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleObject {
    /// The Git object id.
    pub oid: String,
    /// `commit` or `tag`.
    pub kind: String,
    /// Hex SHA-256 of the signed data.
    pub payload_sha256: String,
    /// The salt (base64).
    pub salt: String,
}

/// The DID evidence in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum BundleIdentity {
    /// A did:plc audit log.
    Plc {
        /// The log as fetched (base64).
        audit_log: String,
        /// The CID of the operation the document came from.
        operation_cid: String,
    },
    /// A did:web document.
    Web {
        /// The URL fetched.
        url: String,
        /// The document as fetched (base64).
        document: String,
    },
}

/// The record in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRecord {
    /// The collection.
    pub collection: String,
    /// The record key.
    pub rkey: String,
    /// The record CID.
    pub cid: String,
}

/// The repository commit in a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleCommit {
    /// The commit CID.
    pub cid: String,
    /// The revision.
    pub rev: String,
}

/// The facts a bundle is built from.
#[derive(Debug, Clone)]
pub struct BundleInput<'a> {
    /// The Git object id.
    pub oid: &'a str,
    /// `commit` or `tag`.
    pub kind: &'a str,
    /// The signed data.
    pub payload: &'a [u8],
    /// The parsed signature.
    pub block: &'a SignatureBlock,
    /// The DID resolution used.
    pub resolved: &'a Resolved,
    /// The key the proof verified against.
    pub key: &'a AtprotoKey,
    /// The commit that proved the record.
    pub commit: &'a ProvedCommit,
    /// The `getRecord` answer.
    pub car: &'a [u8],
    /// When it was fetched (RFC 3339).
    pub fetched_at: &'a str,
}

impl ProofBundle {
    /// Builds a bundle.
    pub fn new(input: BundleInput<'_>) -> Self {
        let identity = match &input.resolved.evidence {
            Evidence::Plc {
                audit_log,
                operation_cid,
            } => BundleIdentity::Plc {
                audit_log: STANDARD.encode(audit_log),
                operation_cid: operation_cid.clone(),
            },
            Evidence::Web { url, document } => BundleIdentity::Web {
                url: url.clone(),
                document: STANDARD.encode(document),
            },
        };
        ProofBundle {
            format: BUNDLE_FORMAT.to_owned(),
            object: BundleObject {
                oid: input.oid.to_owned(),
                kind: input.kind.to_owned(),
                payload_sha256: hex::encode(Sha256::digest(input.payload)),
                salt: STANDARD.encode(input.block.salt.as_bytes()),
            },
            did: input.block.did.to_string(),
            identity,
            atproto_key: input.key.did_key(),
            record: BundleRecord {
                collection: COLLECTION.to_owned(),
                rkey: input.block.rkey.to_string(),
                cid: input.block.cid.to_string(),
            },
            repo_commit: BundleCommit {
                cid: input.commit.cid.to_string(),
                rev: input.commit.rev.clone(),
            },
            car: STANDARD.encode(input.car),
            fetched_at: input.fetched_at.to_owned(),
        }
    }

    /// The bundle's JSON bytes, as stored.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The hex SHA-256 of [`Self::to_json`], which addresses the stored bundle.
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(self.to_json()))
    }

    /// The object-store key: `proofs/sha256/<hex>.json`.
    pub fn store_key(&self) -> String {
        format!("proofs/sha256/{}.json", self.sha256())
    }
}

#[cfg(test)]
mod tests;
