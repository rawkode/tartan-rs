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

/// Every branch head's answer, as [`RefSignatures`] gives it, for branches with a judged or
/// queued update.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BranchSignatures {}

impl Action for BranchSignatures {
    type Output = Vec<Value>;

    const NAME: &'static str = "branchSignatures";
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
            // The stored-result shape of design D15 (*Answer shapes the web UI reads*); `kind`,
            // `committed_at`, `checked_at`, `proof_sha256` and `handle` are extra.
            let record_uri = match (&stored.did, &stored.rkey) {
                (Some(did), Some(rkey)) => Some(format!(
                    "at://{did}/{}/{rkey}",
                    tartan_atproto::signature::COLLECTION
                )),
                _ => None,
            };
            let idents = stored
                .idents
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or_else(|| json!({ "author": null, "committer": null }));
            let verified = stored.status == super::Status::Verified;
            let record_missing = stored.status == super::Status::Unverifiable
                && stored
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("not found"));
            let mut view = json!({
                "oid": stored.oid,
                "kind": stored.kind.as_str(),
                "status": stored.status.as_str(),
                "did": stored.did,
                "detail": stored.detail,
                "rkey": stored.rkey,
                "record_cid": stored.record_cid,
                "record_uri": record_uri,
                "record_rev": stored.record_rev,
                "verified_at": verified.then_some(stored.checked_at),
                "revoked_at": null,
                "revoked_reason": null,
                "did_deactivated_at": null,
                "did_status": null,
                "record_missing": record_missing,
                "author": idents["author"],
                "committer": idents["committer"],
                "proof_sha256": stored.proof_sha256,
                "proof_url": null,
                "annotations": [],
                "committed_at": stored.committed_at,
                "checked_at": stored.checked_at,
            });
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
            return Ok(Some(bare_view(oid, "not_checked", Some(text(row, 0)?))));
        }
        Ok(None)
    }
}

/// The stored-result shape of design D15 for an object with no stored result (a deferred
/// object, or a judged head without one): every field present, with no signer, record or proof.
pub(crate) fn bare_view(oid: &str, status: &str, detail: Option<String>) -> Value {
    json!({
        "oid": oid,
        "kind": null,
        "status": status,
        "did": null,
        "handle": null,
        "detail": detail,
        "rkey": null,
        "record_cid": null,
        "record_uri": null,
        "record_rev": null,
        "verified_at": null,
        "revoked_at": null,
        "revoked_reason": null,
        "did_deactivated_at": null,
        "did_status": null,
        "record_missing": false,
        "author": null,
        "committer": null,
        "proof_sha256": null,
        "proof_url": null,
        "annotations": [],
        "committed_at": null,
        "checked_at": null,
    })
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
        "record_missing": failure.is_some_and(|f| f.waiting && f.reason.contains("not found")),
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
        self.ref_view(&ctx, &action.ref_name).await
    }
}

impl Handler<BranchSignatures> for Repo {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        _action: BranchSignatures,
    ) -> Result<Vec<Value>> {
        let refs = ctx
            .sql()
            .query(
                "SELECT ref_name FROM ref_signing WHERE ref_name LIKE 'refs/heads/%' AND head != ''
                 UNION
                 SELECT ref_name FROM signature_checks WHERE ref_name LIKE 'refs/heads/%'
                 ORDER BY 1",
                None,
            )
            .await?;
        let mut out = Vec::new();
        for row in &refs.rows {
            let ref_name = text(row, 0)?;
            if let Some(view) = self.ref_view(&ctx, &ref_name).await? {
                out.push(view);
            }
        }
        Ok(out)
    }
}

impl Repo {
    /// A ref's answer (design D15, *Answer shapes*): its head's result, stored or pending,
    /// plus `{ref, oid, covered, compliant, violations}` for its latest update. `None` for a
    /// ref never judged or queued.
    async fn ref_view(&self, ctx: &Ctx<Self>, ref_name: &str) -> Result<Option<Value>> {
        let judged = ctx
            .sql()
            .query(
                "SELECT r.seq, r.head, r.compliant, u.problem, u.rule, u.violations
                 FROM ref_signing r LEFT JOIN update_signing u
                   ON u.seq = r.seq AND u.ref_name = r.ref_name
                 WHERE r.ref_name = ?",
                Some(vec![param_text(ref_name)]),
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
                Some(vec![param_text(ref_name)]),
            )
            .await?;
        if let Some(row) = queued.rows.first() {
            let row = check_row(row)?;
            if judged_seq.is_none_or(|seq| row.seq > seq) {
                let mut view = pending_view(&row.new, &row.failures, &row);
                view["ref"] = json!(ref_name);
                view["host_seq"] = json!(row.seq);
                view["covered"] = json!(coverage(row.rule.as_deref(), ref_name).is_covered());
                view["compliant"] = Value::Null;
                view["violations"] = json!([]);
                view["problem"] = Value::Null;
                return Ok(Some(view));
            }
        }
        let Some(r) = judged.rows.first() else {
            return Ok(None);
        };
        let head = text(r, 1)?;
        if head.is_empty() {
            // Deleted, with no newer update queued.
            return Ok(None);
        }
        let rule = optional_text(r, 4)?;
        let covered = coverage(rule.as_deref(), ref_name).is_covered();
        let violations = optional_text(r, 5)?
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or_else(|| json!([]));
        let mut view = self
            .signature_view(ctx, &head)
            .await?
            .unwrap_or_else(|| bare_view(&head, "not_checked", None));
        view["ref"] = json!(ref_name);
        view["oid"] = json!(head);
        view["host_seq"] = json!(integer(r, 0)?);
        view["covered"] = json!(covered);
        // `null` when the rule does not cover the ref (design D15).
        view["compliant"] = if covered {
            json!(integer(r, 2)? == 1)
        } else {
            Value::Null
        };
        view["problem"] = json!(optional_text(r, 3)?);
        view["violations"] = violations;
        Ok(Some(view))
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
            let mut check = Value::Null;
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
                        waiting_on = json!("signature_check");
                        check = json!({
                            "oid": row.new,
                            "attempts": row.attempts,
                            "next_at": row.next_at,
                            "last_error": row.last_error,
                        });
                    }
                } else {
                    let judged = ctx
                        .sql()
                        .query(
                            "SELECT compliant, rule FROM update_signing
                             WHERE seq = ? AND ref_name = ?",
                            Some(vec![param_integer(seq), param_text(CONFIG_BRANCH)]),
                        )
                        .await?;
                    if let Some(r) = judged.rows.first()
                        && integer(r, 0)? == 0
                        && coverage(optional_text(r, 1)?.as_deref(), CONFIG_BRANCH).is_covered()
                    {
                        waiting_on = json!("signing_violation");
                    }
                }
            }
            if waiting_on.is_null() {
                waiting_on = json!("evaluation");
            }
            // `waiting_on` is the word of design D15; `check` details a signature check.
            pending = json!({ "commit": commit, "waiting_on": waiting_on, "check": check });
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
                        // A row still queued for this update (a re-check of a re-check) is
                        // replaced with the next generation, so an attempt working the old
                        // one writes nothing over it.
                        let generation = tx
                            .execute(
                                "SELECT generation FROM signature_checks
                                 WHERE seq = ? AND ref_name = ?",
                                Some(vec![
                                    param_integer(*seq),
                                    param_text(ref_name.as_str()),
                                ]),
                            )
                            .await?
                            .rows
                            .first()
                            .map(|r| integer(r, 0))
                            .transpose()?
                            .map_or(0, |generation| generation + 1);
                        tx.execute(
                            "INSERT OR REPLACE INTO signature_checks
                               (seq, ref_name, old, new, rule, attempts, next_at, fetched_dids,
                                generation)
                             VALUES (?, ?, ?, ?, ?, 0, ?, '[]', ?)",
                            Some(vec![
                                param_integer(*seq),
                                param_text(ref_name.as_str()),
                                old.map_or(BindParam::Null, param_text),
                                param_text(new.as_str()),
                                rule.clone().map_or(BindParam::Null, param_text),
                                param_integer(now),
                                param_integer(generation),
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

#[cfg(test)]
mod tests {
    use super::bare_view;

    #[test]
    fn a_deferred_object_answers_the_full_stored_shape() {
        let view = bare_view("ab", "not_checked", Some("more than 64 signers".into()));
        assert_eq!(view["status"], "not_checked");
        assert_eq!(view["detail"], "more than 64 signers");
        assert_eq!(view["record_missing"], false);
        assert_eq!(view["annotations"], serde_json::json!([]));
        for key in [
            "did",
            "record_uri",
            "verified_at",
            "revoked_at",
            "author",
            "committer",
            "proof_url",
        ] {
            assert!(
                view.get(key).is_some_and(serde_json::Value::is_null),
                "{key}"
            );
        }
    }
}
