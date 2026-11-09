//! Verifying a Tartan signature for Git (design D7, *Verification by `tt`*).
//!
//! `tt` proves the record back to its DID with the forge's own code: the DID evidence, the
//! signed repository commit, the MST path, the record's CID, its namespace and algorithm, and
//! the keyed digest of the payload under the signature's salt. On the status descriptor it
//! writes `NEWSIG` first, then `GOODSIG`, `VALIDSIG` and a trust line (exit 0), or `BADSIG` or
//! `ERRSIG` (exit 1). Trust comes from `git config --get-all tt.trustedSigner`, never from the
//! cache.
//!
//! A proved result is cached for 24 hours under `$XDG_CACHE_HOME/tt/verified`, keyed by the
//! record URI, the CID and HMAC-SHA256(salt, payload), so a changed `Salt:` line misses the
//! cache. Fetches use a 3-second connect timeout, and a host that failed to connect is skipped
//! for two minutes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tartan_atproto::did::{Did, DidMethod};
use tartan_atproto::handle::HandleVerdict;
use tartan_atproto::proof::{
    RecordAnswer, RecordRef, SignatureJudgement, fetch_record, judge_signature, prove_record,
};
use tartan_atproto::resolver::ResolveError;
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::block::SignatureBlock;
use tartan_atproto::signature::git::{SignatureClass, classify};
use tartan_atproto::signature::record::keyed_digest;
use tartan_atproto::signature::tid::Tid;

use crate::ctx::Ctx;
use crate::gpg::{StatusWriter, status_safe};
use crate::paths;
use crate::queue::Queue;
use crate::sign::{FORMAT_MESSAGE, read_payload};
use crate::state;

/// How long a proved result is reused.
const CACHE_MS: i64 = 24 * 60 * 60 * 1000;
/// How long a host that failed to connect is skipped.
const HOST_BACKOFF_MS: i64 = 2 * 60 * 1000;

/// A proved verification, as cached.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Proved {
    did: String,
    /// `@handle` when verified both ways, else the DID.
    name: String,
    cid: String,
    /// The proving repository revision's time (Unix seconds).
    rev_time: i64,
    cached_at: i64,
}

/// What verification concluded.
enum Verdict {
    Good(Proved),
    Bad { did: Option<String>, reason: String },
    Error { did: Option<String>, reason: String },
    Unsupported,
}

/// Git's verify call: returns the exit code.
pub async fn git_verify(ctx: &Ctx, status_fd: i32, file: &str) -> i32 {
    let status = StatusWriter::new(status_fd);
    let payload = read_payload().unwrap_or_default();
    let signature = match std::fs::read(file) {
        Ok(signature) => signature,
        Err(error) => {
            status.line("NEWSIG");
            status.line(&errsig("tartan"));
            eprintln!("tt: cannot read the signature file {file}: {error}");
            return 1;
        }
    };
    let verdict = verify(ctx, &signature, &payload).await;
    status.line("NEWSIG");
    match verdict {
        Verdict::Good(proved) => {
            let trusted = trusted_signers(ctx).contains(&proved.did);
            if !(status_safe(&proved.did) && status_safe(&proved.name) && status_safe(&proved.cid))
            {
                status.line(&errsig("tartan"));
                eprintln!("tt: the signature names values that cannot be shown safely");
                return 1;
            }
            status.line(&format!(
                "GOODSIG {} {} ({})",
                proved.did, proved.name, proved.did
            ));
            let date = jacquard_common::deps::chrono::DateTime::from_timestamp(proved.rev_time, 0)
                .map(|time| time.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "1970-01-01".to_owned());
            status.line(&format!(
                "VALIDSIG {} {date} {}",
                proved.cid, proved.rev_time
            ));
            status.line(if trusted {
                "TRUST_FULLY 0 tartan"
            } else {
                "TRUST_UNDEFINED 0 tartan"
            });
            let since = jacquard_common::deps::chrono::DateTime::from_timestamp(proved.rev_time, 0)
                .map(|time| time.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            let shown = if proved.name == proved.did {
                proved.did.clone()
            } else {
                format!("{} ({})", proved.name, proved.did)
            };
            eprintln!(
                "tt: Good signature by {shown}, record {}, in the account's repository since \
                 {since}{}",
                record_uri(ctx, &proved, &signature),
                if trusted {
                    ""
                } else {
                    " (not in tt.trustedSigner)"
                }
            );
            0
        }
        Verdict::Bad { did, reason } => {
            let did = did
                .filter(|did| status_safe(did))
                .unwrap_or_else(|| "tartan-invalid".to_owned());
            status.line(&format!("BADSIG {did} {did}"));
            eprintln!("tt: BAD signature: {reason}");
            1
        }
        Verdict::Error { did, reason } => {
            let did = did
                .filter(|did| status_safe(did))
                .unwrap_or_else(|| "tartan".to_owned());
            status.line(&errsig(&did));
            eprintln!("tt: cannot verify the signature: {reason}");
            1
        }
        Verdict::Unsupported => {
            status.line(&errsig("unsupported"));
            eprintln!(
                "tt: this is not a Tartan signature; verify it with its own program, for example \
                 `git -c gpg.x509.program=gitsign verify-commit` (or gpgsm)"
            );
            1
        }
    }
}

fn record_uri(ctx: &Ctx, proved: &Proved, signature: &[u8]) -> String {
    match classify(signature, ctx.mode) {
        SignatureClass::Tartan(block) => block.uri(),
        _ => format!("at://{}/{COLLECTION}/…", proved.did),
    }
}

/// Git verifying with `tt` as its OpenPGP program.
pub fn openpgp_verify(status_fd: Option<i32>) -> i32 {
    let _ = read_payload();
    if let Some(fd) = status_fd {
        let status = StatusWriter::new(fd);
        status.line("NEWSIG");
        status.line(&errsig("tartan"));
    }
    eprintln!("{FORMAT_MESSAGE}");
    1
}

/// An `ERRSIG` line in gpg's field layout (key id, public-key and hash algorithms, class,
/// time, reason 9 "missing key", fingerprint). Git reads the key id up to the next space, so
/// the fields after it keep the line's end out of `%GK`.
fn errsig(id: &str) -> String {
    format!("ERRSIG {id} 0 8 00 0 9 -")
}

fn trusted_signers(ctx: &Ctx) -> Vec<String> {
    crate::git::config_get_all("tt.trustedSigner")
        .unwrap_or_default()
        .into_iter()
        .filter(|value| {
            let valid = Did::parse(value.trim(), ctx.mode).is_ok();
            if !valid {
                eprintln!("tt: ignoring tt.trustedSigner {value:?}, which is not a DID");
            }
            valid
        })
        .map(|value| value.trim().to_owned())
        .collect()
}

async fn verify(ctx: &Ctx, signature: &[u8], payload: &[u8]) -> Verdict {
    let block = match classify(signature, ctx.mode) {
        SignatureClass::Tartan(block) => block,
        SignatureClass::BadTartan(error) => {
            return Verdict::Bad {
                did: None,
                reason: format!("the Tartan signature is malformed: {error}"),
            };
        }
        _ => return Verdict::Unsupported,
    };
    let did = block.did.as_str().to_owned();
    let key = cache_key(&block, payload);
    let use_cache = std::env::var("TT_NO_CACHE").map_or(true, |value| value != "1");
    if use_cache && let Some(proved) = cached(ctx, &key) {
        return Verdict::Good(proved);
    }
    if std::env::var("TT_VERIFY_OFFLINE").is_ok_and(|value| value == "1") {
        return Verdict::Error {
            did: Some(did),
            reason: "TT_VERIFY_OFFLINE=1 and no cached verification".to_owned(),
        };
    }
    if queued(ctx, &block) {
        return Verdict::Error {
            did: Some(did),
            reason: format!(
                "the record {} has not been published yet; it is in tt's queue (run `tt sign \
                 --flush`)",
                block.uri()
            ),
        };
    }
    let verdict = prove(ctx, &block, payload).await;
    if let Verdict::Good(proved) = &verdict
        && use_cache
    {
        store(ctx, &key, proved);
    }
    verdict
}

async fn prove(ctx: &Ctx, block: &SignatureBlock, payload: &[u8]) -> Verdict {
    let did = block.did.as_str().to_owned();
    let error = |reason: String| Verdict::Error {
        did: Some(did.clone()),
        reason,
    };
    let fetch = match ctx.fetch(Duration::from_secs(10), Some(Duration::from_secs(3))) {
        Ok(fetch) => fetch,
        Err(failure) => return error(failure.to_string()),
    };
    let resolver = match ctx.resolver_with(fetch) {
        Ok(resolver) => resolver,
        Err(failure) => return error(failure.to_string()),
    };
    let did_host = did_host(ctx, &block.did);
    if let Some(until) = backed_off(ctx, &did_host) {
        return error(format!(
            "{did_host} could not be reached recently; tt retries it after {}",
            crate::sign::format_time(until)
        ));
    }
    let resolved = match resolver.resolve_did(&block.did).await {
        Ok(resolved) => resolved,
        Err(ResolveError::Unavailable(reason)) => {
            back_off(ctx, &did_host);
            return error(format!("resolving {did}: {reason}"));
        }
        Err(failure) => return error(format!("resolving {did}: {failure}")),
    };
    let Some(identity) = resolved.identity else {
        return error(format!("{did} is tombstoned"));
    };
    let (Some(pds), Some(key)) = (identity.pds.clone(), identity.signing_key.clone()) else {
        return error(format!("{did} names no PDS or no #atproto key"));
    };
    let pds_host = crate::pds::host_of(&pds);
    if let Some(until) = backed_off(ctx, &pds_host) {
        return error(format!(
            "{pds_host} could not be reached recently; tt retries it after {}",
            crate::sign::format_time(until)
        ));
    }
    let reference = RecordRef {
        did: block.did.clone(),
        collection: COLLECTION.to_owned(),
        rkey: block.rkey.as_str().to_owned(),
    };
    let car = match fetch_record(resolver.fetch(), &pds, &reference).await {
        RecordAnswer::Car(car) => car,
        RecordAnswer::RecordNotFound => {
            return error(format!(
                "the record {} is not in the repository",
                block.uri()
            ));
        }
        RecordAnswer::RepoUnavailable(reason) => {
            return error(format!("the repository of {did} is unavailable ({reason})"));
        }
        RecordAnswer::Unavailable(reason) => {
            back_off(ctx, &pds_host);
            return error(reason);
        }
        RecordAnswer::Failed(reason) => return error(reason),
    };
    let now = u64::try_from(state::now_ms()).unwrap_or_default();
    let outcome = prove_record(car, reference, key, now).await;
    match judge_signature(outcome, block, payload) {
        SignatureJudgement::Verified { commit, .. } => {
            let rev_time = Tid::parse(&commit.rev)
                .map(|tid| i64::try_from(tid.micros() / 1_000_000).unwrap_or_default())
                .unwrap_or_default();
            let name = match resolver.verify_handle(&identity).await {
                HandleVerdict::Verified(handle) if status_safe(handle.as_str()) => {
                    format!("@{}", handle.as_str())
                }
                _ => did.clone(),
            };
            Verdict::Good(Proved {
                did: did.clone(),
                name,
                cid: block.cid.to_string(),
                rev_time,
                cached_at: state::now_ms(),
            })
        }
        SignatureJudgement::Replaced { found, .. } => Verdict::Bad {
            did: Some(did.clone()),
            reason: format!(
                "the repository holds another record under {} (CID {found}); it was replaced \
                 after signing",
                block.uri()
            ),
        },
        SignatureJudgement::Mismatch(reason) => Verdict::Bad {
            did: Some(did.clone()),
            reason: format!("the record does not sign this object: {reason}"),
        },
        SignatureJudgement::Absent { .. } => error(format!(
            "the record {} is not in the repository",
            block.uri()
        )),
        SignatureJudgement::Incomplete(reason) => {
            error(format!("the proof is incomplete: {reason}"))
        }
        SignatureJudgement::Invalid(failure) => {
            error(format!("the proof does not verify: {failure}"))
        }
    }
}

fn did_host(ctx: &Ctx, did: &Did) -> String {
    match did.method() {
        DidMethod::Plc => match &ctx.dev {
            Some(dev) => crate::pds::host_of(&dev.plc_url),
            None => "plc.directory".to_owned(),
        },
        DidMethod::Web => did
            .web_host()
            .map(|(host, port)| match port {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            })
            .unwrap_or_default(),
    }
}

fn cache_key(block: &SignatureBlock, payload: &[u8]) -> String {
    let digest = keyed_digest(&block.salt, payload);
    let key = format!("{}\n{}\n{}", block.uri(), block.cid, hex::encode(digest));
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn cache_file(ctx: &Ctx, key: &str) -> PathBuf {
    ctx.dirs.cache.join("verified").join(format!("{key}.json"))
}

fn cached(ctx: &Ctx, key: &str) -> Option<Proved> {
    let bytes = std::fs::read(cache_file(ctx, key)).ok()?;
    let proved: Proved = serde_json::from_slice(&bytes).ok()?;
    (state::now_ms() - proved.cached_at < CACHE_MS).then_some(proved)
}

fn store(ctx: &Ctx, key: &str, proved: &Proved) {
    if let Ok(bytes) = serde_json::to_vec(proved) {
        let _ = paths::write_private(&cache_file(ctx, key), &bytes);
    }
}

fn queued(ctx: &Ctx, block: &SignatureBlock) -> bool {
    Queue::new(ctx, block.did.as_str())
        .read_unlocked()
        .is_ok_and(|entries| {
            entries.iter().any(|entry| {
                entry.rkey == block.rkey.as_str() && entry.cid == block.cid.to_string()
            })
        })
}

fn hosts_file(ctx: &Ctx) -> PathBuf {
    ctx.dirs.cache.join("hosts.json")
}

fn backed_off(ctx: &Ctx, host: &str) -> Option<i64> {
    let bytes = std::fs::read(hosts_file(ctx)).ok()?;
    let hosts: BTreeMap<String, i64> = serde_json::from_slice(&bytes).ok()?;
    hosts
        .get(host)
        .copied()
        .filter(|until| *until > state::now_ms())
}

fn back_off(ctx: &Ctx, host: &str) {
    let path = hosts_file(ctx);
    let now = state::now_ms();
    let mut hosts: BTreeMap<String, i64> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    hosts.retain(|_, until| *until > now);
    hosts.insert(host.to_owned(), now + HOST_BACKOFF_MS);
    if let Ok(bytes) = serde_json::to_vec(&hosts) {
        let _ = paths::write_private(&path, &bytes);
    }
}

/// The cache result: whether verifying `signature` over `payload` would be answered from the
/// cache (for tests of the cache key).
#[cfg(test)]
fn key_for(signature: &[u8], payload: &[u8]) -> Option<String> {
    match classify(signature, tartan_atproto::Mode::default()) {
        SignatureClass::Tartan(block) => Some(cache_key(&block, payload)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use tartan_atproto::signature::record::Salt;

    use super::*;

    fn block(salt: [u8; 32]) -> String {
        let block = SignatureBlock {
            did: Did::parse(
                "did:plc:abcdefghijklmnopqrstuvwx",
                tartan_atproto::Mode::default(),
            )
            .expect("did"),
            rkey: Tid::parse("3lbmxyz2abc2k").expect("tid"),
            cid: tartan_atproto::ipld::cid_for_dag_cbor(b"record"),
            salt: Salt::from_bytes(salt),
        };
        block.write()
    }

    #[test]
    fn the_cache_key_covers_the_salt() {
        let payload = b"tree 0000\n";
        let first = key_for(block([1; 32]).as_bytes(), payload).expect("key");
        let other_salt = key_for(block([2; 32]).as_bytes(), payload).expect("key");
        let other_payload = key_for(block([1; 32]).as_bytes(), b"tree 1111\n").expect("key");
        assert_ne!(first, other_salt);
        assert_ne!(first, other_payload);
        assert_eq!(
            first,
            key_for(block([1; 32]).as_bytes(), payload).expect("key")
        );
    }
}
