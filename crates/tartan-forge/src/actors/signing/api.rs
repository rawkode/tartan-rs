//! What the repository actor answers about signatures (design D8, *Pending is visible*,
//! *Statuses are final*, *Annotations are computed on read*; D11 *Configuration gating*).

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use rivetkit::prelude::*;
use rivetkit::{BindParam, Handler};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tartan_identity_api::{People, RecordKey, SignerStanding, status_is_inactive};

use super::{
    CHECK_COLUMNS, CONFIG_BRANCH, STORED_COLUMNS, StoredFailure, Verify, check_row, coverage,
    options, stored_row,
};
use crate::actors::repo::Repo;
use crate::clients::identity_handle;
use crate::sql::{integer, now_ms, optional_text, param_integer, param_text, text};

/// The result for one object: stored with its annotations, `pending` while queued,
/// `not_checked` when deferred, or `None` (404).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignatureQuery {
    /// The object id.
    pub oid: String,
}

/// A signature answer, as JSON.
pub type SignatureView = Value;

impl Action for SignatureQuery {
    type Output = Option<SignatureView>;

    const NAME: &'static str = "signature";
}

/// The result for a ref's latest update: `pending` while it is queued, else the judged
/// update and its head's result, or `None` (404) for a ref never judged or queued.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefSignatures {
    /// The full ref name.
    pub ref_name: String,
}

impl Action for RefSignatures {
    type Output = Option<Value>;

    const NAME: &'static str = "refSignatures";
}

/// The applied configuration and any pending one with what it waits on.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConfigStatusQuery {}

/// The configuration status, as JSON:
/// `{applied: {commit}|null, pending: {commit, waiting_on}|null, rule, config_commit}`.
pub type ConfigStatus = Value;

impl Action for ConfigStatusQuery {
    type Output = ConfigStatus;

    const NAME: &'static str = "configStatus";
}

/// Re-queues an `unverifiable` result, or an object the bounds left unchecked, with every
/// update that judged it (design D8, *Statuses are final*).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecheckSignature {
    /// The object id.
    pub oid: String,
    /// The caller's DID, when the caller is not an owner. Only the DID stored on the result
    /// may re-check it then.
    pub signer: Option<String>,
}

/// What a [`RecheckSignature`] did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RecheckOutcome {
    /// The updates that judged the object are queued again.
    Requeued {
        /// How many updates.
        updates: usize,
    },
    /// No update judged the object, or its result is final (409 to an owner).
    Conflict,
    /// The caller may not re-check it (404 to a signer).
    NotFound,
}

impl Action for RecheckSignature {
    type Output = RecheckOutcome;

    const NAME: &'static str = "recheckSignature";
}

/// Whether the repository's stored results reference proof bundle `sha256`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProofReferenced {
    /// The bundle's hex SHA-256.
    pub sha256: String,
}

impl Action for ProofReferenced {
    type Output = bool;

    const NAME: &'static str = "proofReferenced";
}

/// The pending `verify` schedules, as `(event id, real time in milliseconds)`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VerifySchedules {}

impl Action for VerifySchedules {
    type Output = Vec<(String, i64)>;

    const NAME: &'static str = "verifySchedules";
}

impl Handler<Verify> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Verify) -> Result<()> {
        self.verify(&ctx).await
    }
}

impl Handler<VerifySchedules> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: VerifySchedules,
    ) -> Result<Vec<(String, i64)>> {
        Ok(ctx
            .schedule()
            .list()
            .await?
            .into_iter()
            .filter(|event| event.action == Verify::NAME)
            .map(|event| (event.id, event.run_at))
            .collect())
    }
}

impl Handler<ProofReferenced> for Repo {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ProofReferenced) -> Result<bool> {
        let found = ctx
            .sql()
            .query(
                "SELECT 1 FROM object_signatures WHERE proof_sha256 = ? LIMIT 1",
                Some(vec![param_text(action.sha256.as_str())]),
            )
            .await?;
        Ok(!found.rows.is_empty())
    }
}

impl Repo {
    /// The pending answer for `oid` while a queued check lists it.
    async fn pending(&self, ctx: &Ctx<Self>, oid: &str) -> Result<Option<Value>> {
        let queued = ctx
            .sql()
            .query(
                &format!(
                    "SELECT {CHECK_COLUMNS} FROM signature_checks
                     WHERE new = ?1 OR (seq, ref_name) IN
                       (SELECT seq, ref_name FROM check_objects WHERE oid = ?1)
                     ORDER BY seq DESC LIMIT 1"
                ),
                Some(vec![param_text(oid)]),
            )
            .await?;
        let Some(row) = queued.rows.first() else {
            return Ok(None);
        };
        let row = check_row(row)?;
        Ok(Some(pending_view(oid, &row.failures, &row)))
    }

    /// The stored result of `oid` with its annotations.
    pub(crate) async fn signature_view(&self, ctx: &Ctx<Self>, oid: &str) -> Result<Option<Value>> {
        let result = ctx
            .sql()
            .query(
                &format!("SELECT {STORED_COLUMNS} FROM object_signatures WHERE oid = ?"),
                Some(vec![param_text(oid)]),
            )
            .await?;
        if let Some(row) = result.rows.first() {
            let stored = stored_row(row)?;
            let mut view = json!({
                "oid": stored.oid,
                "kind": stored.kind.as_str(),
                "status": stored.status.as_str(),
                "did": stored.did,
                "detail": stored.detail,
                "committed_at": stored.committed_at,
                "checked_at": stored.checked_at,
            });
            if let (Some(did), Some(rkey)) = (&stored.did, &stored.rkey) {
                view["record"] = json!({
                    "uri": format!("at://{did}/{}/{rkey}", tartan_atproto::signature::COLLECTION),
                    "rkey": rkey,
                    "cid": stored.record_cid,
                    "rev": stored.record_rev,
                });
            }
            if let Some(sha) = &stored.proof_sha256 {
                view["proof_sha256"] = json!(sha);
            }
            if let Some(did) = stored.did.clone() {
                let identity = identity_handle(&ctx.client()?)?;
                if let Some(rkey) = stored.rkey.clone() {
                    let standing = identity
                        .send(SignerStanding {
                            pairs: vec![RecordKey {
                                did: did.clone(),
                                rkey,
                            }],
                        })
                        .await?;
                    if let Some(standing) = standing.into_iter().next() {
                        view["revoked_at"] = json!(standing.revoked_at);
                        view["revoked_reason"] =
                            json!(standing.revoked_reason.map(|reason| reason.as_str()));
                    }
                }
                let people = identity.send(People { dids: vec![did] }).await?;
                if let Some(Some(person)) = people.into_iter().next() {
                    view["handle"] = json!(person.handle);
                    view["did_status"] = json!(person.status);
                    if status_is_inactive(&person.status) {
                        view["did_deactivated_at"] = json!(person.status_checked_at);
                    }
                }
            }
            return Ok(Some(view));
        }
        if let Some(pending) = self.pending(ctx, oid).await? {
            return Ok(Some(pending));
        }
        let deferred = ctx
            .sql()
            .query(
                "SELECT deferred FROM update_objects WHERE oid = ? AND deferred IS NOT NULL LIMIT 1",
                Some(vec![param_text(oid)]),
            )
            .await?;
        if let Some(row) = deferred.rows.first() {
            return Ok(Some(json!({
                "oid": oid,
                "status": "not_checked",
                "detail": text(row, 0)?,
            })));
        }
        Ok(None)
    }
}

fn pending_view(oid: &str, failures: &[(String, StoredFailure)], row: &super::CheckRow) -> Value {
    let failure = failures
        .iter()
        .find(|(failed, _)| failed == oid)
        .or_else(|| failures.first())
        .map(|(_, failure)| failure);
    json!({
        "oid": oid,
        "status": "pending",
        "attempts": row.attempts,
        "next_at": row.next_at,
        "last_error": row.last_error,
        "did": failure.and_then(|f| f.did.clone()),
        "waiting_on_record": failure.is_some_and(|f| f.waiting && f.reason.contains("not found")),
    })
}

impl Handler<SignatureQuery> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: SignatureQuery,
    ) -> Result<Option<Value>> {
        self.signature_view(&ctx, &action.oid).await
    }
}

impl Handler<RefSignatures> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: RefSignatures,
    ) -> Result<Option<Value>> {
        let judged = ctx
            .sql()
            .query(
                "SELECT r.seq, r.head, r.compliant, u.problem, u.rule
                 FROM ref_signing r LEFT JOIN update_signing u
                   ON u.seq = r.seq AND u.ref_name = r.ref_name
                 WHERE r.ref_name = ?",
                Some(vec![param_text(action.ref_name.as_str())]),
            )
            .await?;
        let judged_seq = judged.rows.first().map(|r| integer(r, 0)).transpose()?;
        let queued = ctx
            .sql()
            .query(
                &format!(
                    "SELECT {CHECK_COLUMNS} FROM signature_checks WHERE ref_name = ?
                     ORDER BY seq DESC LIMIT 1"
                ),
                Some(vec![param_text(action.ref_name.as_str())]),
            )
            .await?;
        if let Some(row) = queued.rows.first() {
            let row = check_row(row)?;
            if judged_seq.is_none_or(|seq| row.seq > seq) {
                let mut view = pending_view(&row.new, &row.failures, &row);
                view["ref_name"] = json!(action.ref_name);
                view["head"] = json!(row.new);
                return Ok(Some(view));
            }
        }
        let Some(r) = judged.rows.first() else {
            return Ok(None);
        };
        let head = text(r, 1)?;
        let rule = optional_text(r, 4)?;
        Ok(Some(json!({
            "ref_name": action.ref_name,
            "status": "checked",
            "host_seq": integer(r, 0)?,
            "head": head,
            "compliant": integer(r, 2)? == 1,
            "covered": coverage(rule.as_deref(), &action.ref_name).is_covered(),
            "problem": optional_text(r, 3)?,
            "head_signature": self.signature_view(&ctx, &head).await?,
        })))
    }
}

impl Handler<ConfigStatusQuery> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: ConfigStatusQuery,
    ) -> Result<ConfigStatus> {
        let applied = ctx
            .sql()
            .query("SELECT sha FROM config_applied WHERE id = 1", None)
            .await?
            .rows
            .first()
            .map(|r| text(r, 0))
            .transpose()?;
        let head = ctx
            .sql()
            .query("SELECT sha, applied FROM config_head WHERE id = 1", None)
            .await?;
        let mut pending = Value::Null;
        if let Some(r) = head.rows.first()
            && integer(r, 1)? == 0
        {
            let commit = text(r, 0)?;
            let mut waiting_on = Value::Null;
            if let Some((seq, _)) = self.head_update(&ctx).await? {
                let queued = ctx
                    .sql()
                    .query(
                        &format!(
                            "SELECT {CHECK_COLUMNS} FROM signature_checks WHERE seq = ? AND ref_name = ?"
                        ),
                        Some(vec![param_integer(seq), param_text(CONFIG_BRANCH)]),
                    )
                    .await?;
                if let Some(row) = queued.rows.first() {
                    let row = check_row(row)?;
                    if coverage(row.rule.as_deref(), CONFIG_BRANCH).is_covered() {
                        waiting_on = json!({
                            "signature_check": row.new,
                            "attempts": row.attempts,
                            "next_at": row.next_at,
                            "last_error": row.last_error,
                        });
                    }
                }
            }
            pending = json!({ "commit": commit, "waiting_on": waiting_on });
        }
        let rule = ctx
            .sql()
            .query(
                "SELECT rule, config_commit FROM signing_rule WHERE id = 1",
                None,
            )
            .await?;
        let (rule, config_commit) = match rule.rows.first() {
            Some(r) => (
                serde_json::from_str::<Value>(&text(r, 0)?).unwrap_or(Value::Null),
                Value::String(text(r, 1)?),
            ),
            None => (Value::Null, Value::Null),
        };
        Ok(json!({
            "applied": applied.map(|commit| json!({ "commit": commit })),
            "pending": pending,
            "rule": rule,
            "config_commit": config_commit,
        }))
    }
}

/// The updates that list `oid` in `update_objects`.
async fn updates_listing(
    ctx: &Ctx<Repo>,
    oids: &BTreeSet<String>,
) -> Result<BTreeSet<(i64, String)>> {
    let mut out = BTreeSet::new();
    let oids: Vec<&String> = oids.iter().collect();
    for chunk in oids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let params: Vec<BindParam> = chunk.iter().map(|oid| param_text(oid.as_str())).collect();
        let rows = ctx
            .sql()
            .query(
                &format!("SELECT seq, ref_name FROM update_objects WHERE oid IN ({marks})"),
                Some(params),
            )
            .await?;
        for row in &rows.rows {
            out.insert((integer(row, 0)?, text(row, 1)?));
        }
    }
    Ok(out)
}

impl Handler<RecheckSignature> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: RecheckSignature,
    ) -> Result<RecheckOutcome> {
        let refused = || {
            if action.signer.is_some() {
                RecheckOutcome::NotFound
            } else {
                RecheckOutcome::Conflict
            }
        };
        let start = BTreeSet::from([action.oid.clone()]);
        let mut updates = updates_listing(&ctx, &start).await?;
        if updates.is_empty() {
            return Ok(refused());
        }
        if let Some(signer) = &action.signer {
            let stored = ctx
                .sql()
                .query(
                    "SELECT did FROM object_signatures WHERE oid = ? AND status = 'unverifiable'",
                    Some(vec![param_text(action.oid.as_str())]),
                )
                .await?;
            let did = stored
                .rows
                .first()
                .map(|r| optional_text(r, 0))
                .transpose()?
                .flatten();
            if did.as_deref() != Some(signer.as_str()) {
                return Ok(RecheckOutcome::NotFound);
            }
        }
        // The closure over `update_objects`: every object those updates list, and every update
        // that lists one of them.
        let mut objects: BTreeSet<String> = BTreeSet::new();
        loop {
            let mut added = false;
            for (seq, ref_name) in updates.clone() {
                let listed = ctx
                    .sql()
                    .query(
                        "SELECT oid FROM update_objects WHERE seq = ? AND ref_name = ?",
                        Some(vec![param_integer(seq), param_text(ref_name.as_str())]),
                    )
                    .await?;
                for row in &listed.rows {
                    added |= objects.insert(text(row, 0)?);
                }
            }
            let more = updates_listing(&ctx, &objects).await?;
            for update in more {
                added |= updates.insert(update);
            }
            if !added {
                break;
            }
        }
        let head = self.head_update(&ctx).await?;
        let now = now_ms();
        let count = updates.len();
        let (updates_tx, objects_tx) = (updates.clone(), objects.clone());
        ctx.sql()
            .transaction(
                move |tx| async move {
                    for oid in &objects_tx {
                        tx.execute(
                            "DELETE FROM object_signatures WHERE oid = ? AND status = 'unverifiable'",
                            Some(vec![param_text(oid.as_str())]),
                        )
                        .await?;
                    }
                    for (seq, ref_name) in &updates_tx {
                        let judged = tx
                            .execute(
                                "SELECT old, new, rule, compliant FROM update_signing
                                 WHERE seq = ? AND ref_name = ?",
                                Some(vec![param_integer(*seq), param_text(ref_name.as_str())]),
                            )
                            .await?;
                        let Some(r) = judged.rows.first() else {
                            continue;
                        };
                        let (old, new, rule, compliant) = (
                            optional_text(r, 0)?,
                            text(r, 1)?,
                            optional_text(r, 2)?,
                            integer(r, 3)? == 1,
                        );
                        tx.execute(
                            "INSERT OR REPLACE INTO signature_checks
                               (seq, ref_name, old, new, rule, attempts, next_at, fetched_dids)
                             VALUES (?, ?, ?, ?, ?, 0, ?, '[]')",
                            Some(vec![
                                param_integer(*seq),
                                param_text(ref_name.as_str()),
                                old.map_or(BindParam::Null, param_text),
                                param_text(new.as_str()),
                                rule.clone().map_or(BindParam::Null, param_text),
                                param_integer(now),
                            ]),
                        )
                        .await?;
                        let is_head = head.as_ref() == Some(&(*seq, ref_name.clone()));
                        if is_head
                            && !compliant
                            && coverage(rule.as_deref(), CONFIG_BRANCH).is_covered()
                        {
                            tx.execute("UPDATE config_head SET applied = 0 WHERE id = 1", None)
                                .await?;
                        }
                    }
                    Ok(())
                },
                options("recheck_signature"),
            )
            .await?;
        self.ensure_verify_at(&ctx, now).await?;
        tracing::info!(repo = %self.id, oid = %action.oid, updates = count,
            "unverifiable signature re-queued");
        Ok(RecheckOutcome::Requeued { updates: count })
    }
}
