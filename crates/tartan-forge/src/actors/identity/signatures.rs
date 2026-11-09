//! The signature records the forge relies on (design D7, *Watching records*): tracking, re-proofs
//! confirmed with fresh evidence, withdrawals on the signer's word, and the standing that
//! verification reads. Also the Jetstream cursor (design D10).
//!
//! A revoked row is never un-revoked: nothing in this module clears `revoked_at`, and nothing
//! deletes a `signature_records` row.

use std::sync::Arc;

use anyhow::Result;
use rivetkit::SqliteTransaction;
use rivetkit::prelude::*;
use serde_json::json;
use tartan_atproto::did::Did;
use tartan_atproto::fetch::FetchClient;
use tartan_atproto::keys::AtprotoKey;
use tartan_atproto::proof::{ProofOutcome, RecordAnswer, RecordRef, fetch_record, prove_record};
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::tid::Tid;
use tartan_identity_api::{
    GetJetstreamCursor, KnownAs, MAX_STANDING_PAIRS, RecordSignature, ReproveSignature, Reproved,
    RevokedReason, SetJetstreamCursor, SignatureRecordInfo, SignatureRecordQuery, SignerStanding,
    Standing, WithdrawSignature, Withdrawn, status_is_inactive,
};

use super::{Identity, SOURCE, parse_did};
use crate::event_log::append;
use crate::proof_cache::proof_cache;
use crate::services::services;
use crate::sql::{
    integer, now_ms, optional_integer, optional_text, param_integer, param_text, text,
};

/// The longest record CID accepted from a withdrawal (a CIDv1 in base32 is 59 characters).
const MAX_CID_CHARS: usize = 128;

/// The single integer a `SELECT COUNT(*)` answers.
async fn count(
    tx: &SqliteTransaction,
    sql: &str,
    params: Option<Vec<rivetkit::BindParam>>,
) -> Result<i64> {
    let result = tx.execute(sql, params).await?;
    result.rows.first().map_or(Ok(0), |row| integer(row, 0))
}

/// A tracked row as the re-proof needs it.
struct Tracked {
    cid: String,
    rev: String,
    revoked_at: Option<i64>,
    revoked_reason: Option<String>,
}

async fn tracked(ctx: &Ctx<Identity>, did: &str, rkey: &str) -> Result<Option<Tracked>> {
    let result = ctx
        .sql()
        .query(
            "SELECT cid, rev, revoked_at, revoked_reason FROM signature_records
             WHERE did = ? AND rkey = ?",
            Some(vec![param_text(did), param_text(rkey)]),
        )
        .await?;
    result
        .rows
        .first()
        .map(|row| {
            Ok(Tracked {
                cid: text(row, 0)?,
                rev: text(row, 1)?,
                revoked_at: optional_integer(row, 2)?,
                revoked_reason: optional_text(row, 3)?,
            })
        })
        .transpose()
}

/// What one fetch and proof of a record established.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Finding {
    /// The repository holds a record with this CID, as of this revision.
    Present { cid: String, rev: String },
    /// The repository provably does not hold the record, as of this revision.
    Absent { rev: String },
    /// The PDS answered `RepoDeactivated`, `RepoTakendown`, `RepoSuspended` or `RepoNotFound`.
    AccountError(String),
    /// The PDS could not be reached, timed out or answered 5xx or 429.
    Unavailable(String),
    /// The answer proves nothing: a proof that does not verify, a missing block, or a
    /// `RecordNotFound` error, which is not signed.
    Inconclusive(String),
}

impl Finding {
    /// The revocation this finding would make against the tracked CID, with its revision.
    fn revokes(&self, tracked_cid: &str) -> Option<(RevokedReason, &str, Option<&str>)> {
        match self {
            Finding::Absent { rev } => Some((RevokedReason::Deleted, rev, None)),
            Finding::Present { cid, rev } if cid != tracked_cid => {
                Some((RevokedReason::Replaced, rev, Some(cid)))
            }
            _ => None,
        }
    }

    fn reason(&self) -> String {
        match self {
            Finding::Present { cid, .. } => format!("the record is present as {cid}"),
            Finding::Absent { .. } => "the record is absent".to_owned(),
            Finding::AccountError(error) => format!("the PDS answered {error}"),
            Finding::Unavailable(reason) | Finding::Inconclusive(reason) => reason.clone(),
        }
    }
}

/// Fetches `getRecord` for `(did, rkey)` from `pds` afresh and proves it against `key`.
async fn prove_once(
    fetch: &FetchClient,
    pds: &str,
    key: &str,
    did: &Did,
    rkey: &str,
    now: i64,
) -> Finding {
    let Ok(key) = AtprotoKey::from_did_key(key) else {
        return Finding::Inconclusive(format!("{did}'s stored #atproto key is not usable"));
    };
    let record = RecordRef {
        did: did.clone(),
        collection: COLLECTION.to_owned(),
        rkey: rkey.to_owned(),
    };
    let host = reqwest::Url::parse(pds)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| pds.to_owned());
    match fetch_record(fetch, pds, &record).await {
        RecordAnswer::Car(car) => {
            let now = u64::try_from(now).unwrap_or_default();
            match prove_record(car, record, key, now).await {
                ProofOutcome::Present {
                    record_cid, commit, ..
                } => Finding::Present {
                    cid: record_cid.to_string(),
                    rev: commit.rev,
                },
                ProofOutcome::Absent { commit } => Finding::Absent { rev: commit.rev },
                ProofOutcome::Incomplete(reason) => {
                    Finding::Inconclusive(format!("incomplete proof from {host}: {reason}"))
                }
                ProofOutcome::Invalid(failure) => Finding::Inconclusive(format!(
                    "proof from {host} did not verify against {did}'s #atproto key: {failure}"
                )),
            }
        }
        RecordAnswer::RecordNotFound => Finding::Inconclusive(format!(
            "{host} answered RecordNotFound, which proves nothing"
        )),
        RecordAnswer::RepoUnavailable(error) => Finding::AccountError(error),
        RecordAnswer::Unavailable(reason) => Finding::Unavailable(reason),
        RecordAnswer::Failed(reason) => Finding::Inconclusive(reason),
    }
}

fn transient(reason: impl Into<String>) -> Reproved {
    Reproved::Transient {
        reason: reason.into(),
    }
}

impl Identity {
    /// Re-proves a tracked record (design D7, *Watching records* and *Confirming before
    /// revoking*).
    async fn reprove(&self, ctx: &Ctx<Self>, did: &str, rkey: &str) -> Result<Reproved> {
        let Some(row) = tracked(ctx, did, rkey).await? else {
            return Ok(Reproved::Untracked);
        };
        if row.revoked_at.is_some() {
            return Ok(Reproved::AlreadyRevoked {
                reason: row.revoked_reason.as_deref().and_then(RevokedReason::parse),
            });
        }
        let parsed = parse_did(did)?;
        let fetch = services()?.resolver.fetch().clone();

        // The first proof uses the stored evidence, resolving the DID first if there is none.
        let stored = ctx
            .sql()
            .query(
                "SELECT pds, atproto_key FROM people WHERE did = ?",
                Some(vec![param_text(did)]),
            )
            .await?;
        let mut evidence = match stored.rows.first() {
            Some(row) => (optional_text(row, 0)?, optional_text(row, 1)?),
            None => (None, None),
        };
        if evidence.0.is_none() || evidence.1.is_none() {
            let refreshed = self.refresh(ctx, did).await?;
            evidence = (refreshed.pds, refreshed.atproto_key);
        }
        let (Some(pds), Some(key)) = evidence else {
            return Ok(transient(format!(
                "{did} has no verified PDS or #atproto key"
            )));
        };
        let first = prove_once(&fetch, &pds, &key, &parsed, rkey, now_ms()).await;
        match &first {
            Finding::Present { cid, .. } if *cid == row.cid => return Ok(Reproved::Present),
            Finding::Unavailable(reason) => return Ok(transient(reason.clone())),
            Finding::AccountError(error) => {
                // The account may have moved or been suspended: learn its status, change
                // nothing else.
                self.refresh(ctx, did).await?;
                return Ok(transient(format!("{pds} answered {error}")));
            }
            _ => {}
        }

        // Confirm with fresh evidence: drop the reuse cache, re-resolve the DID, and prove
        // again from the PDS and against the key that resolution names.
        proof_cache().drop_did(did);
        let refreshed = self.refresh(ctx, did).await?;
        let (Some(pds), Some(key)) = (refreshed.pds, refreshed.atproto_key) else {
            return Ok(transient(format!(
                "{did} could not be re-resolved to confirm the proof"
            )));
        };
        let second = prove_once(&fetch, &pds, &key, &parsed, rkey, now_ms()).await;
        let Some((reason, rev, found)) = second.revokes(&row.cid) else {
            return Ok(match second {
                Finding::Present { cid, .. } if cid == row.cid => Reproved::Present,
                other => transient(other.reason()),
            });
        };
        // Two proofs must agree. When the first proof was not conclusive (stale evidence), a
        // third proof against the fresh evidence confirms the second.
        let agrees = |finding: &Finding| {
            finding
                .revokes(&row.cid)
                .is_some_and(|(r, _, f)| r == reason && f == found)
        };
        let confirming = if agrees(&first) {
            first
        } else {
            prove_once(&fetch, &pds, &key, &parsed, rkey, now_ms()).await
        };
        if !agrees(&confirming) {
            return Ok(transient(format!(
                "the confirming proof disagrees: {}",
                confirming.reason()
            )));
        }
        // A proof from an older repository state (a caching proxy, a lagging replica) is
        // stale; revisions are TIDs, which sort as text.
        let oldest = match confirming.revokes(&row.cid) {
            Some((_, confirming_rev, _)) => confirming_rev.min(rev).to_owned(),
            None => rev.to_owned(),
        };
        if !row.rev.is_empty() && oldest.as_str() < row.rev.as_str() {
            return Ok(transient(format!(
                "the proof's revision {oldest} is older than the tracked {}",
                row.rev
            )));
        }
        self.revoke_record(ctx, did, rkey, &row.cid, reason).await?;
        Ok(Reproved::Revoked { reason })
    }

    /// Marks a tracked row revoked and records `signature.revoked`, unless it already is.
    async fn revoke_record(
        &self,
        ctx: &Ctx<Self>,
        did: &str,
        rkey: &str,
        cid: &str,
        reason: RevokedReason,
    ) -> Result<bool> {
        let now = now_ms();
        let (did_owned, rkey_owned, cid_owned) = (did.to_owned(), rkey.to_owned(), cid.to_owned());
        let revoked = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    let updated = tx
                        .execute(
                            "UPDATE signature_records SET revoked_at = ?, revoked_reason = ?
                             WHERE did = ? AND rkey = ? AND revoked_at IS NULL",
                            Some(vec![
                                param_integer(now),
                                param_text(reason.as_str()),
                                param_text(did_owned.as_str()),
                                param_text(rkey_owned.as_str()),
                            ]),
                        )
                        .await?;
                    if updated.changes == 0 {
                        return Ok(false);
                    }
                    let data = json!({
                        "did": did_owned, "rkey": rkey_owned, "cid": cid_owned,
                        "reason": reason.as_str(),
                    });
                    append(&tx, SOURCE, "signature.revoked", data).await?;
                    Ok(true)
                },
                SqliteTransactionOptions {
                    name: Some("revoke_signature"),
                    timeout: None,
                },
            )
            .await?;
        proof_cache().drop_record(did, rkey);
        if revoked {
            tracing::info!(%did, %rkey, reason = reason.as_str(), "signature record revoked");
            self.flush_later(ctx);
        }
        Ok(revoked)
    }

    /// Withdraws a record on its DID's word (design D7, *Watching records*).
    async fn withdraw(&self, ctx: &Ctx<Self>, action: WithdrawSignature) -> Result<Withdrawn> {
        parse_did(&action.did)?;
        if Tid::parse(&action.rkey).is_err() {
            return Ok(Withdrawn::Invalid {
                message: "the record key is not a TID".to_owned(),
            });
        }
        if action.cid.len() > MAX_CID_CHARS || cid::Cid::try_from(action.cid.as_str()).is_err() {
            return Ok(Withdrawn::Invalid {
                message: "the record CID is not a CID".to_owned(),
            });
        }
        let services = services()?;
        let limits = services.identity_limits.clone();
        let can_read = services.can_read(&action.did);
        let now = now_ms();
        let WithdrawSignature { did, rkey, cid } = action;
        let (did_tx, rkey_tx) = (did.clone(), rkey.clone());
        let (outcome, recorded) = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    let (did, rkey) = (did_tx, rkey_tx);
                    let existing = tx
                        .execute(
                            "SELECT revoked_at, pre_withdrawn FROM signature_records
                             WHERE did = ? AND rkey = ?",
                            Some(vec![param_text(did.as_str()), param_text(rkey.as_str())]),
                        )
                        .await?;
                    let event = json!({
                        "did": did, "rkey": rkey, "cid": cid,
                        "reason": RevokedReason::Withdrawn.as_str(),
                    });
                    if let Some(row) = existing.rows.first() {
                        let outcome = if integer(row, 1)? == 1 {
                            Withdrawn::PreWithdrawn
                        } else {
                            Withdrawn::Tracked
                        };
                        if optional_integer(row, 0)?.is_some() {
                            return Ok((outcome, false));
                        }
                        // A tracked record is always revoked: no cap, no rate limit.
                        tx.execute(
                            "UPDATE signature_records SET revoked_at = ?, revoked_reason = ?
                             WHERE did = ? AND rkey = ?",
                            Some(vec![
                                param_integer(now),
                                param_text(RevokedReason::Withdrawn.as_str()),
                                param_text(did.as_str()),
                                param_text(rkey.as_str()),
                            ]),
                        )
                        .await?;
                        append(&tx, SOURCE, "signature.revoked", event).await?;
                        return Ok((outcome, true));
                    }

                    let mine = count(
                        &tx,
                        "SELECT COUNT(*) FROM signature_records WHERE pre_withdrawn = 1 AND did = ?",
                        Some(vec![param_text(did.as_str())]),
                    )
                    .await?;
                    let total = count(
                        &tx,
                        "SELECT COUNT(*) FROM signature_records WHERE pre_withdrawn = 1",
                        None,
                    )
                    .await?;
                    let known = count(
                        &tx,
                        "SELECT COUNT(*) FROM people WHERE did = ? AND known_as <> '[]'",
                        Some(vec![param_text(did.as_str())]),
                    )
                    .await?
                        > 0;
                    let as_u64 = |n: i64| u64::try_from(n).unwrap_or(u64::MAX);
                    let capped = if as_u64(mine) >= limits.pre_withdrawn_per_did {
                        Some(format!(
                            "this account has withdrawn {} records the forge does not track, the most it may",
                            limits.pre_withdrawn_per_did
                        ))
                    } else if as_u64(total) >= limits.pre_withdrawn {
                        Some("the forge holds as many withdrawn untracked records as it may".to_owned())
                    } else if !can_read && !known && as_u64(total) >= limits.pre_withdrawn_for_strangers
                    {
                        Some(
                            "the forge's room for withdrawals by accounts it does not know is used up"
                                .to_owned(),
                        )
                    } else {
                        None
                    };
                    if let Some(message) = capped {
                        return Ok((Withdrawn::Capped { message }, false));
                    }
                    tx.execute(
                        "INSERT INTO signature_records
                         (did, rkey, cid, rev, first_proved_at, revoked_at, revoked_reason, pre_withdrawn)
                         VALUES (?, ?, ?, '', ?, ?, ?, 1)",
                        Some(vec![
                            param_text(did.as_str()),
                            param_text(rkey.as_str()),
                            param_text(cid.as_str()),
                            param_integer(now),
                            param_integer(now),
                            param_text(RevokedReason::Withdrawn.as_str()),
                        ]),
                    )
                    .await?;
                    append(&tx, SOURCE, "signature.revoked", event).await?;
                    Ok((Withdrawn::PreWithdrawn, true))
                },
                SqliteTransactionOptions {
                    name: Some("withdraw_signature"),
                    timeout: None,
                },
            )
            .await?;
        proof_cache().drop_record(&did, &rkey);
        if recorded {
            tracing::info!(%did, %rkey, "signature record withdrawn by its signer");
            self.flush_later(ctx);
        }
        Ok(outcome)
    }
}

impl Handler<RecordSignature> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: RecordSignature) -> Result<bool> {
        parse_did(&action.did)?;
        anyhow::ensure!(
            Tid::parse(&action.rkey).is_ok(),
            "the record key {:?} is not a TID",
            action.rkey
        );
        let now = now_ms();
        let RecordSignature {
            did,
            rkey,
            cid,
            rev,
        } = action;
        let did_tx = did.clone();
        let inserted = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    // An existing row, revoked or not, is left exactly as it is.
                    let inserted = tx
                        .execute(
                            "INSERT OR IGNORE INTO signature_records
                             (did, rkey, cid, rev, first_proved_at) VALUES (?, ?, ?, ?, ?)",
                            Some(vec![
                                param_text(did_tx.as_str()),
                                param_text(rkey.as_str()),
                                param_text(cid.as_str()),
                                param_text(rev.as_str()),
                                param_integer(now),
                            ]),
                        )
                        .await?
                        .changes
                        > 0;
                    Ok(inserted)
                },
                SqliteTransactionOptions {
                    name: Some("record_signature"),
                    timeout: None,
                },
            )
            .await?;
        let (known, never_checked) = self.know(&ctx, &did, KnownAs::Signer).await?;
        ctx.sql()
            .execute(
                "UPDATE people SET last_signed_at = ? WHERE did = ?",
                Some(vec![param_integer(now), param_text(did.as_str())]),
            )
            .await?;
        if known && never_checked {
            let background = ctx.clone();
            ctx.wait_until(async move {
                self.refresh_quietly(background, did).await;
            });
        }
        Ok(inserted)
    }
}

impl Handler<ReproveSignature> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReproveSignature) -> Result<Reproved> {
        self.reprove(&ctx, &action.did, &action.rkey).await
    }
}

impl Handler<WithdrawSignature> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: WithdrawSignature,
    ) -> Result<Withdrawn> {
        self.withdraw(&ctx, action).await
    }
}

impl Handler<SignerStanding> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: SignerStanding,
    ) -> Result<Vec<Standing>> {
        anyhow::ensure!(
            action.pairs.len() <= MAX_STANDING_PAIRS,
            "at most {MAX_STANDING_PAIRS} pairs per call"
        );
        let mut standing = Vec::with_capacity(action.pairs.len());
        for pair in &action.pairs {
            let record = ctx
                .sql()
                .query(
                    "SELECT revoked_at, revoked_reason FROM signature_records
                     WHERE did = ? AND rkey = ?",
                    Some(vec![
                        param_text(pair.did.as_str()),
                        param_text(pair.rkey.as_str()),
                    ]),
                )
                .await?;
            let (revoked_at, revoked_reason) = match record.rows.first() {
                Some(row) => (optional_integer(row, 0)?, optional_text(row, 1)?),
                None => (None, None),
            };
            let person = ctx
                .sql()
                .query(
                    "SELECT status FROM people WHERE did = ?",
                    Some(vec![param_text(pair.did.as_str())]),
                )
                .await?;
            let status = match person.rows.first() {
                Some(row) => text(row, 0)?,
                None => "active".to_owned(),
            };
            standing.push(Standing {
                revoked_at,
                revoked_reason: revoked_reason.as_deref().and_then(RevokedReason::parse),
                inactive: status_is_inactive(&status),
                status,
            });
        }
        Ok(standing)
    }
}

impl Handler<SignatureRecordQuery> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: SignatureRecordQuery,
    ) -> Result<Option<SignatureRecordInfo>> {
        let result = ctx
            .sql()
            .query(
                "SELECT cid, rev, first_proved_at, revoked_at, revoked_reason, pre_withdrawn
                 FROM signature_records WHERE did = ? AND rkey = ?",
                Some(vec![
                    param_text(action.did.as_str()),
                    param_text(action.rkey.as_str()),
                ]),
            )
            .await?;
        result
            .rows
            .first()
            .map(|row| {
                Ok(SignatureRecordInfo {
                    did: action.did.clone(),
                    rkey: action.rkey.clone(),
                    cid: text(row, 0)?,
                    rev: text(row, 1)?,
                    first_proved_at: integer(row, 2)?,
                    revoked_at: optional_integer(row, 3)?,
                    revoked_reason: optional_text(row, 4)?
                        .as_deref()
                        .and_then(RevokedReason::parse),
                    pre_withdrawn: integer(row, 5)? == 1,
                })
            })
            .transpose()
    }
}

impl Handler<GetJetstreamCursor> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: GetJetstreamCursor,
    ) -> Result<Option<i64>> {
        let result = ctx
            .sql()
            .query("SELECT cursor_us FROM jetstream WHERE id = 1", None)
            .await?;
        Ok(result
            .rows
            .first()
            .map(|row| optional_integer(row, 0))
            .transpose()?
            .flatten())
    }
}

impl Handler<SetJetstreamCursor> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: SetJetstreamCursor) -> Result<()> {
        // Bookkeeping: no event (design D3, *Housekeeping*).
        ctx.sql()
            .execute(
                "INSERT INTO jetstream (id, cursor_us) VALUES (1, ?)
                 ON CONFLICT (id) DO UPDATE SET cursor_us = excluded.cursor_us",
                Some(vec![param_integer(action.cursor_us)]),
            )
            .await?;
        Ok(())
    }
}
