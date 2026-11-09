//! Verifying one pushed object (design D8, *Per object*).
//!
//! [`classify`] does the local steps (extract, parse) without the network. [`Verifier`] does the
//! rest for a Tartan signature: resolving the signer, proving the record from its PDS, and
//! judging the record against the object. One [`Verifier`] serves one `verify` run, so its
//! [`FetchMemo`] resolves each DID and refreshes each identity at most once per run, and a host
//! that failed fails fast for the rest of the run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rivetkit::client::Client;
use tartan_atproto::Mode;
use tartan_atproto::did::Did;
use tartan_atproto::keys::AtprotoKey;
use tartan_atproto::proof::{
    BundleInput, ProofBundle, ProofFailure, RecordAnswer, RecordRef, SignatureJudgement,
    fetch_record, judge_signature, prove_record,
};
use tartan_atproto::resolver::{ResolveError, Resolved, VerifyingResolver};
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::block::SignatureBlock;
use tartan_atproto::signature::git::{
    HashAlgorithm, SignatureClass, classify as classify_signature, extract_commit, extract_tag,
};
use tartan_identity_api::{
    People, RefreshIdentity, ReproveSignature, SignatureRecordQuery, status_is_inactive,
};
use tokio::sync::{OnceCell, Semaphore};

use crate::clients::identity_handle;
use crate::git_objects::{ObjectKind, RawObject};
use crate::proof_cache::{CachedProof, proof_cache};
use crate::sql::now_ms;

/// A final result (design D8, *Statuses are final*).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Status {
    Verified,
    Unsigned,
    BadSignature,
    SignerInactive,
    UnsupportedFormat,
    NotChecked,
    Unverifiable,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Verified => "verified",
            Status::Unsigned => "unsigned",
            Status::BadSignature => "bad_signature",
            Status::SignerInactive => "signer_inactive",
            Status::UnsupportedFormat => "unsupported_format",
            Status::NotChecked => "not_checked",
            Status::Unverifiable => "unverifiable",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "verified" => Status::Verified,
            "unsigned" => Status::Unsigned,
            "bad_signature" => Status::BadSignature,
            "signer_inactive" => Status::SignerInactive,
            "unsupported_format" => Status::UnsupportedFormat,
            "not_checked" => Status::NotChecked,
            "unverifiable" => Status::Unverifiable,
            _ => return None,
        })
    }
}

/// A result to store in `object_signatures`.
#[derive(Clone, Debug)]
pub(crate) struct Stored {
    pub(crate) oid: String,
    pub(crate) kind: ObjectKind,
    pub(crate) status: Status,
    pub(crate) did: Option<String>,
    pub(crate) rkey: Option<String>,
    pub(crate) record_cid: Option<String>,
    pub(crate) record_rev: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) committed_at: Option<i64>,
    /// The claimed author and committer (a tag's tagger) as JSON `{author, committer}`, each
    /// `{name, email}` or `null`.
    pub(crate) idents: Option<String>,
    /// The proof bundle of a `verified` result: its store key, digest and bytes.
    pub(crate) bundle: Option<(String, String, Vec<u8>)>,
}

/// A check that could not finish now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// A short reason, without secrets.
    pub(crate) reason: String,
    /// The record is missing, or the signer's PDS, DID host or PLC directory could not be
    /// reached, timed out or answered 5xx or 429: the cases an update on a covered ref may wait
    /// up to seven days for.
    pub(crate) waiting: bool,
    /// The signer the check waits on.
    pub(crate) did: Option<String>,
}

/// What one object's check came to in one attempt.
#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    Final(Box<Stored>),
    Failed(Failure),
}

/// The local reading of an object: either a final result, or a Tartan signature to prove.
pub(crate) enum Local {
    Final(Stored),
    Tartan {
        block: SignatureBlock,
        payload: Vec<u8>,
        committed_at: Option<i64>,
    },
}

fn stored(
    raw: &RawObject,
    status: Status,
    detail: Option<String>,
    committed_at: Option<i64>,
) -> Stored {
    Stored {
        oid: raw.oid.clone(),
        kind: raw.kind,
        status,
        did: None,
        rkey: None,
        record_cid: None,
        record_rev: None,
        detail,
        committed_at,
        idents: idents(raw),
        bundle: None,
    }
}

/// The `{name, email}` of a Git identity line's value (`Name <email> time zone`).
fn ident(value: &str) -> Option<serde_json::Value> {
    let (name, rest) = value.split_once(" <")?;
    let (email, _) = rest.split_once('>')?;
    Some(serde_json::json!({ "name": name.trim(), "email": email.trim() }))
}

/// The claimed author and committer of a raw commit, or the tagger of a tag (as `committer`),
/// as stored in `object_signatures.idents`.
pub(crate) fn idents(raw: &RawObject) -> Option<String> {
    let text = std::str::from_utf8(&raw.bytes).ok()?;
    let header = |name: &str| {
        text.split('\n')
            .take_while(|line| !line.is_empty())
            .find_map(|line| line.strip_prefix(name))
            .and_then(ident)
    };
    let (author, committer) = match raw.kind {
        ObjectKind::Commit => (header("author "), header("committer ")),
        ObjectKind::Tag => (None, header("tagger ")),
    };
    Some(serde_json::json!({ "author": author, "committer": committer }).to_string())
}

/// The committer's (or tagger's) time of a raw object, in Unix milliseconds.
fn committed_at(raw: &RawObject) -> Option<i64> {
    let header = match raw.kind {
        ObjectKind::Commit => "committer ",
        ObjectKind::Tag => "tagger ",
    };
    let text = std::str::from_utf8(&raw.bytes).ok()?;
    let line = text
        .split('\n')
        .take_while(|line| !line.is_empty())
        .find(|line| line.starts_with(header))?;
    let mut parts = line.rsplitn(3, ' ');
    let _zone = parts.next()?;
    let seconds: i64 = parts.next()?.parse().ok()?;
    seconds.checked_mul(1000)
}

/// Design D8 steps 1 and 2: extracts the signature and parses it.
pub(crate) fn classify(raw: &RawObject, mode: Mode) -> Local {
    let at = committed_at(raw);
    let extracted = match raw.kind {
        ObjectKind::Commit => match extract_commit(&raw.bytes, HashAlgorithm::Sha1) {
            Ok(extracted) => extracted,
            Err(error) => {
                return Local::Final(stored(
                    raw,
                    Status::BadSignature,
                    Some(error.to_string()),
                    at,
                ));
            }
        },
        ObjectKind::Tag => extract_tag(&raw.bytes),
    };
    let Some(signature) = extracted.signature else {
        return Local::Final(stored(raw, Status::Unsigned, None, at));
    };
    match classify_signature(&signature, mode) {
        SignatureClass::Tartan(block) => Local::Tartan {
            block,
            payload: extracted.payload,
            committed_at: at,
        },
        SignatureClass::BadTartan(error) => Local::Final(stored(
            raw,
            Status::BadSignature,
            Some(error.to_string()),
            at,
        )),
        SignatureClass::OtherX509 => Local::Final(stored(
            raw,
            Status::UnsupportedFormat,
            Some("signed with an X.509 tool other than tt".into()),
            at,
        )),
        SignatureClass::Pgp => Local::Final(stored(
            raw,
            Status::UnsupportedFormat,
            Some("signed with OpenPGP".into()),
            at,
        )),
        SignatureClass::Ssh => Local::Final(stored(
            raw,
            Status::UnsupportedFormat,
            Some("signed with SSH".into()),
            at,
        )),
        SignatureClass::Unknown => Local::Final(stored(
            raw,
            Status::UnsupportedFormat,
            Some("an unrecognised signature".into()),
            at,
        )),
    }
}

/// The host part of a URL, for reasons and per-host limits.
fn host_of(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => match (parsed.host_str(), parsed.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (Some(host), None) => host.to_owned(),
            _ => url.to_owned(),
        },
        Err(_) => url.to_owned(),
    }
}

/// What a run has learned, shared by the objects it checks.
#[derive(Default)]
pub(crate) struct FetchMemo {
    /// Each DID's resolution in this run.
    resolved: HashMap<String, Arc<OnceCell<Result<Resolved, ResolveError>>>>,
    /// The DIDs `RefreshIdentity` ran for in this run, with the status it stored.
    refreshed: HashMap<String, Arc<OnceCell<Option<String>>>>,
    /// Hosts that failed in this run, with the reason they failed.
    failed_hosts: HashMap<String, String>,
    /// At most two requests to one host at a time.
    hosts: HashMap<String, Arc<Semaphore>>,
}

/// Checks objects for one `verify` run.
pub(crate) struct Verifier {
    client: Client,
    resolver: VerifyingResolver,
    memo: Mutex<FetchMemo>,
}

impl Verifier {
    pub(crate) fn new(client: Client, resolver: VerifyingResolver) -> Self {
        Self {
            client,
            resolver,
            memo: Mutex::new(FetchMemo::default()),
        }
    }

    fn memo(&self) -> std::sync::MutexGuard<'_, FetchMemo> {
        self.memo.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this run has resolved `did` already.
    pub(crate) fn has_resolved(&self, did: &str) -> bool {
        self.memo().resolved.contains_key(did)
    }

    fn failed_host(&self, host: &str) -> Option<String> {
        self.memo().failed_hosts.get(host).cloned()
    }

    fn fail_host(&self, host: &str, reason: &str) {
        self.memo()
            .failed_hosts
            .entry(host.to_owned())
            .or_insert_with(|| reason.to_owned());
    }

    fn host_permits(&self, host: &str) -> Arc<Semaphore> {
        self.memo()
            .hosts
            .entry(host.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(2)))
            .clone()
    }

    /// Resolves `did` once per run (or afresh when `fresh`), failing fast on a host that failed.
    async fn resolve(&self, did: &Did, fresh: bool) -> Result<Resolved, ResolveError> {
        let cell = {
            let mut memo = self.memo();
            if fresh {
                memo.resolved.remove(did.as_str());
            }
            memo.resolved
                .entry(did.to_string())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        cell.get_or_init(|| async {
            let host = match did.web_host() {
                Some((host, _)) => host.to_owned(),
                None => "plc".to_owned(),
            };
            if let Some(reason) = self.failed_host(&host) {
                return Err(ResolveError::Unavailable(reason));
            }
            let permits = self.host_permits(&host);
            let _permit = permits.acquire().await;
            let resolved = self.resolver.resolve_did(did).await;
            if let Err(ResolveError::Unavailable(reason)) = &resolved {
                self.fail_host(&host, reason);
            }
            resolved
        })
        .await
        .clone()
    }

    /// Runs `RefreshIdentity` for `did` at most once per run and answers the status it stored.
    async fn refresh(&self, did: &str) -> Option<String> {
        let cell = self
            .memo()
            .refreshed
            .entry(did.to_owned())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone();
        cell.get_or_init(|| async {
            let handle = identity_handle(&self.client).ok()?;
            match handle
                .send(RefreshIdentity {
                    did: did.to_owned(),
                })
                .await
            {
                Ok(refreshed) => Some(refreshed.status),
                Err(error) => {
                    tracing::warn!(%did, %error, "could not refresh a signer's identity");
                    None
                }
            }
        })
        .await
        .clone()
    }

    /// The DID's stored account status, refreshing it when the forge has none yet.
    async fn status(&self, did: &str) -> Option<String> {
        let handle = identity_handle(&self.client).ok()?;
        let people = handle
            .send(People {
                dids: vec![did.to_owned()],
            })
            .await
            .ok()?;
        match people.into_iter().next().flatten() {
            Some(person) if person.status_checked_at.is_some() => Some(person.status),
            _ => self.refresh(did).await,
        }
    }

    /// Asks the identity actor to re-prove a tracked record, which marks it revoked only after
    /// its own confirmation with fresh evidence (design D7, *Confirming before revoking*).
    async fn reprove_if_tracked(&self, did: &str, rkey: &str) {
        let Ok(handle) = identity_handle(&self.client) else {
            return;
        };
        let tracked = handle
            .send(SignatureRecordQuery {
                did: did.to_owned(),
                rkey: rkey.to_owned(),
            })
            .await;
        if let Ok(Some(row)) = tracked
            && row.revoked_at.is_none()
        {
            let reproved = handle
                .send(ReproveSignature {
                    did: did.to_owned(),
                    rkey: rkey.to_owned(),
                })
                .await;
            if let Err(error) = reproved {
                tracing::warn!(%did, %rkey, %error, "could not re-prove a tracked signature record");
            }
        }
    }

    /// Design D8 steps 4 to 6 for a Tartan signature.
    pub(crate) async fn verify(
        &self,
        raw: &RawObject,
        block: SignatureBlock,
        payload: Vec<u8>,
        committed_at: Option<i64>,
    ) -> Outcome {
        let did = block.did.to_string();
        let rkey = block.rkey.to_string();
        let uri = block.uri();
        let failed = |reason: String, waiting: bool| {
            Outcome::Failed(Failure {
                reason,
                waiting,
                did: Some(did.clone()),
            })
        };
        let final_with = |status: Status, detail: Option<String>| {
            let mut result = stored(raw, status, detail, committed_at);
            result.did = Some(did.clone());
            result.rkey = Some(rkey.clone());
            result.record_cid = Some(block.cid.to_string());
            Outcome::Final(Box::new(result))
        };

        // Step 4: the signer's identity and status.
        let mut resolved = match self.resolve(&block.did, false).await {
            Ok(resolved) => resolved,
            Err(error) => {
                // An unreachable DID host (a did:web signer's own host, often its PDS too) or
                // PLC directory is waited on like an unreachable PDS.
                let waiting = error.is_transient();
                return failed(format!("could not resolve {did}: {error}"), waiting);
            }
        };
        if resolved.identity.is_none() {
            return final_with(Status::SignerInactive, Some("deleted".into()));
        }
        if let Some(status) = self.status(&did).await
            && status_is_inactive(&status)
        {
            return final_with(Status::SignerInactive, Some(status));
        }

        // Step 5: prove, refreshing the identity and retrying once when the proof does not
        // verify against the evidence in hand.
        let mut retried = false;
        loop {
            let Some(identity) = resolved.identity.clone() else {
                return final_with(Status::SignerInactive, Some("deleted".into()));
            };
            let (Some(pds), Some(key)) = (identity.pds.clone(), identity.signing_key.clone())
            else {
                return failed(format!("{did} names no PDS or #atproto key"), false);
            };
            let host = host_of(&pds);
            let car = match self.fetch(&did, &rkey, &pds, &key, &host).await {
                Ok(car) => car,
                Err(Fetched::Unavailable(reason)) => return failed(reason, true),
                Err(Fetched::NotFound) => {
                    return failed(format!("signature record {uri} not found at {host}"), true);
                }
                Err(Fetched::Account(error)) => {
                    let status = self.refresh(&did).await;
                    if let Some(status) = status
                        && status_is_inactive(&status)
                    {
                        return final_with(Status::SignerInactive, Some(status));
                    }
                    return failed(format!("{host} answered {error} for {did}"), false);
                }
                Err(Fetched::Other(reason)) => return failed(reason, false),
            };
            let record = RecordRef {
                did: block.did.clone(),
                collection: COLLECTION.to_owned(),
                rkey: rkey.clone(),
            };
            let now = u64::try_from(now_ms()).unwrap_or_default();
            let outcome = prove_record(car.as_ref().clone(), record, key.clone(), now).await;
            let judged = judge_signature(outcome, &block, &payload);
            let refresh_and_retry = |failure: &ProofFailure| failure.needs_identity_refresh();
            match judged {
                SignatureJudgement::Verified { commit, .. } => {
                    proof_cache().put(
                        &did,
                        &rkey,
                        CachedProof {
                            pds: pds.clone(),
                            atproto_key: key.did_key(),
                            car: car.clone(),
                            fetched_at_ms: now_ms(),
                        },
                    );
                    let fetched_at = rfc3339(now_ms());
                    let bundle = ProofBundle::new(BundleInput {
                        oid: &raw.oid,
                        kind: raw.kind.as_str(),
                        payload: &payload,
                        block: &block,
                        resolved: &resolved,
                        key: &key,
                        commit: &commit,
                        car: &car,
                        fetched_at: &fetched_at,
                    });
                    let mut result = stored(raw, Status::Verified, None, committed_at);
                    result.did = Some(did.clone());
                    result.rkey = Some(rkey.clone());
                    result.record_cid = Some(block.cid.to_string());
                    result.record_rev = Some(commit.rev.clone());
                    result.bundle = Some((bundle.store_key(), bundle.sha256(), bundle.to_json()));
                    return Outcome::Final(Box::new(result));
                }
                SignatureJudgement::Mismatch(reason) => {
                    return final_with(
                        Status::BadSignature,
                        Some(format!(
                            "record does not sign this {}: {reason}",
                            raw.kind.as_str()
                        )),
                    );
                }
                SignatureJudgement::Replaced { found, .. } => {
                    // Confirm with fresh evidence before it counts (design D7).
                    proof_cache().drop_did(&did);
                    if retried {
                        // The fresh proof agrees with the first.
                        self.reprove_if_tracked(&did, &rkey).await;
                        return final_with(
                            Status::BadSignature,
                            Some(format!("record {uri} was replaced (now {found})")),
                        );
                    }
                    self.refresh(&did).await;
                    match self.resolve(&block.did, true).await {
                        Ok(fresh) => resolved = fresh,
                        Err(error) => {
                            let waiting = error.is_transient();
                            return failed(
                                format!("could not re-resolve {did} to confirm: {error}"),
                                waiting,
                            );
                        }
                    }
                    retried = true;
                }
                SignatureJudgement::Absent { .. } => {
                    // A missing record is not final (design D8); a tracked row is revoked only
                    // after the identity actor confirms it.
                    self.reprove_if_tracked(&did, &rkey).await;
                    return failed(format!("signature record {uri} not found at {host}"), true);
                }
                SignatureJudgement::Incomplete(reason) => {
                    return failed(format!("incomplete proof from {host}: {reason}"), false);
                }
                SignatureJudgement::Invalid(failure) => {
                    if retried || !refresh_and_retry(&failure) {
                        return failed(
                            format!(
                                "proof from {host} did not verify against {did}'s #atproto key: {failure}"
                            ),
                            false,
                        );
                    }
                    proof_cache().drop_did(&did);
                    self.refresh(&did).await;
                    match self.resolve(&block.did, true).await {
                        Ok(fresh) => resolved = fresh,
                        Err(error) => {
                            let waiting = error.is_transient();
                            return failed(format!("could not re-resolve {did}: {error}"), waiting);
                        }
                    }
                    retried = true;
                }
            }
        }
    }

    /// The `getRecord` answer for `(did, rkey)`: from the reuse cache when it was fetched less
    /// than ten minutes ago from the same PDS against the same key, else fetched.
    async fn fetch(
        &self,
        did: &str,
        rkey: &str,
        pds: &str,
        key: &AtprotoKey,
        host: &str,
    ) -> Result<Arc<Vec<u8>>, Fetched> {
        if let Some(cached) = proof_cache().get(did, rkey, now_ms())
            && cached.pds == pds
            && cached.atproto_key == key.did_key()
        {
            return Ok(cached.car);
        }
        if let Some(reason) = self.failed_host(host) {
            return Err(Fetched::Unavailable(reason));
        }
        let Ok(parsed) = Did::parse(did, self.resolver.mode()) else {
            return Err(Fetched::Other(format!("{did} is not a DID")));
        };
        let record = RecordRef {
            did: parsed,
            collection: COLLECTION.to_owned(),
            rkey: rkey.to_owned(),
        };
        let permits = self.host_permits(host);
        let _permit = permits.acquire().await;
        match fetch_record(self.resolver.fetch(), pds, &record).await {
            RecordAnswer::Car(car) => Ok(Arc::new(car)),
            RecordAnswer::RecordNotFound => Err(Fetched::NotFound),
            RecordAnswer::RepoUnavailable(error) => Err(Fetched::Account(error)),
            RecordAnswer::Unavailable(reason) => {
                let reason = format!("PDS {host} unreachable: {reason}");
                self.fail_host(host, &reason);
                Err(Fetched::Unavailable(reason))
            }
            RecordAnswer::Failed(reason) => Err(Fetched::Other(reason)),
        }
    }
}

/// Why a `getRecord` gave no CAR.
enum Fetched {
    Unavailable(String),
    NotFound,
    Account(String),
    Other(String),
}

/// `ms` (Unix milliseconds) as an RFC 3339 UTC timestamp.
pub(crate) fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_762_689_600_000), "2025-11-09T12:00:00Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn reads_the_committer_time() {
        let raw = RawObject {
            oid: "x".into(),
            kind: ObjectKind::Commit,
            bytes: b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@b> 1 +0000\ncommitter C <c@d> 1700000000 +0100\n\nmessage\n".to_vec(),
        };
        assert_eq!(committed_at(&raw), Some(1_700_000_000_000));
        let parsed: serde_json::Value =
            serde_json::from_str(&idents(&raw).expect("idents")).expect("json");
        assert_eq!(
            parsed,
            serde_json::json!({
                "author": {"name": "A", "email": "a@b"},
                "committer": {"name": "C", "email": "c@d"},
            })
        );
        assert!(matches!(
            classify(&raw, Mode::Production),
            Local::Final(Stored {
                status: Status::Unsigned,
                ..
            })
        ));
    }
}
