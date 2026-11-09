//! Commit signature verification in the repository actor (design D8) and the signing rule
//! (design D11).
//!
//! Every update of a branch or tag is queued in `signature_checks` with a copy of the rule in
//! force, in the transaction that records `ref.updated`. The scheduled `verify` action works the
//! queue: it reads what the update introduced from the Git host, verifies each object that has
//! no stored result yet, stores final results as soon as they are known, and judges the update
//! once every object has a result or the check gives up. Each judged update writes its own
//! `update_signing` row; `ref_signing` follows the newest judged update of each ref.
//!
//! Liveness rests on one invariant: while any row is queued, either a run holds `verifying` (and
//! calls [`Repo::ensure_verify_at`] when it ends) or a `verify` schedule that Rivet still holds
//! is pending at or before the earliest `next_at`.

mod api;
pub(crate) mod object;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use rivetkit::prelude::*;
use rivetkit::{BindParam, ColumnValue, SqliteTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tartan_config::signing::{Coverage, SigningRule};
use tartan_identity_api::{RecordKey, RecordSignature, SignerStanding, Standing};

use crate::actors::repo::Repo;
use crate::clients::identity_handle;
use crate::clock::real_time_ms;
use crate::event_log::append;
use crate::git_objects::{ObjectKind, ObjectNotServed, RawObject, WALK_LIMIT};
use crate::outbox::SubscriberSource;
use crate::services::services;
use crate::sql::{
    integer, now_ms, optional_integer, optional_text, param_integer, param_optional_text,
    param_text, text,
};

pub use api::{
    BranchSignatures, ConfigStatus, ConfigStatusQuery, ProofReferenced, RecheckOutcome,
    RecheckSignature, RefSignatures, SignatureQuery, SignatureView, VerifySchedules,
};
use object::{Failure, Local, Outcome, Status, Stored, Verifier, classify};

/// The branch whose configuration the repository applies.
pub(crate) const CONFIG_BRANCH: &str = "refs/heads/main";

/// The tables of signature verification (design D8, *Repo actor schema*). Beyond the design's
/// tables: `signature_checks` keeps each attempt's state (`first_attempt_at`, the walk's
/// outcome and the objects still failing), `check_objects` the last walk's listing so a check
/// that gives up after a failed walk still judges what it listed before, `object_signatures.recorded`
/// whether the identity actor was told of a verified record, and `config_applied` the last
/// applied configuration's commit.
pub(crate) const SIGNING_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS signature_checks (
        seq INTEGER NOT NULL,
        ref_name TEXT NOT NULL,
        old TEXT,
        new TEXT NOT NULL,
        rule TEXT,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_at INTEGER NOT NULL,
        last_error TEXT,
        fetched_dids TEXT NOT NULL DEFAULT '[]',
        first_attempt_at INTEGER,
        walked INTEGER NOT NULL DEFAULT 0,
        truncated INTEGER NOT NULL DEFAULT 0,
        tip_not_commit TEXT,
        failures TEXT NOT NULL DEFAULT '[]',
        PRIMARY KEY (seq, ref_name)
    );
    CREATE TABLE IF NOT EXISTS check_objects (
        seq INTEGER NOT NULL,
        ref_name TEXT NOT NULL,
        position INTEGER NOT NULL,
        oid TEXT NOT NULL,
        kind TEXT NOT NULL,
        PRIMARY KEY (seq, ref_name, position)
    );
    CREATE INDEX IF NOT EXISTS check_objects_by_oid ON check_objects (oid);
    CREATE TABLE IF NOT EXISTS object_signatures (
        oid TEXT PRIMARY KEY,
        kind TEXT NOT NULL,
        status TEXT NOT NULL,
        did TEXT,
        rkey TEXT,
        record_cid TEXT,
        record_rev TEXT,
        proof_sha256 TEXT,
        detail TEXT,
        committed_at INTEGER,
        checked_at INTEGER NOT NULL,
        recorded INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS object_signatures_by_proof ON object_signatures (proof_sha256);
    CREATE TABLE IF NOT EXISTS update_signing (
        seq INTEGER NOT NULL,
        ref_name TEXT NOT NULL,
        old TEXT,
        new TEXT NOT NULL,
        rule TEXT,
        compliant INTEGER NOT NULL,
        problem TEXT,
        violations TEXT NOT NULL DEFAULT '[]',
        checked_at INTEGER NOT NULL,
        PRIMARY KEY (seq, ref_name)
    );
    CREATE TABLE IF NOT EXISTS update_objects (
        seq INTEGER NOT NULL,
        ref_name TEXT NOT NULL,
        oid TEXT NOT NULL,
        deferred TEXT,
        PRIMARY KEY (seq, ref_name, oid)
    );
    CREATE INDEX IF NOT EXISTS update_objects_by_oid ON update_objects (oid);
    CREATE TABLE IF NOT EXISTS ref_signing (
        ref_name TEXT PRIMARY KEY,
        seq INTEGER NOT NULL,
        head TEXT NOT NULL,
        compliant INTEGER NOT NULL,
        violation_seq INTEGER,
        checked_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS verify_schedule (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        event_id TEXT NOT NULL,
        at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS signing_rule (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        rule TEXT NOT NULL,
        config_commit TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS config_applied (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        sha TEXT NOT NULL
    )
";

/// The most due rows one run works.
const ROWS_PER_RUN: usize = 50;
/// One run's wall-clock budget.
const RUN_BUDGET: Duration = Duration::from_secs(5 * 60);
/// One attempt's deadline.
const ATTEMPT_DEADLINE: Duration = Duration::from_secs(90);
/// At most this many objects are checked at once.
const OBJECTS_IN_FLIGHT: usize = 8;
/// New signer DIDs one attempt may fetch for.
const NEW_SIGNERS_PER_ATTEMPT: usize = 16;
/// Signer DIDs one update may fetch for over its life.
const SIGNERS_PER_UPDATE: usize = 64;
/// How long a check retries before it gives up.
const WINDOW_MS: i64 = 60 * 60 * 1000;
/// How long an update waiting on its signers retries before it gives up.
const SIGNER_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The retry interval of an update waiting on its signers after its first hour.
const HOURLY_MS: i64 = 60 * 60 * 1000;

/// The detail of a deferred object.
const DEFERRED_DETAIL: &str = "more than 64 signers in one update";
/// The detail of an object still carried when its check gave up.
const CARRIED_DETAIL: &str = "not reached before the check gave up";

/// The backoff after `attempts` failed attempts: 5 s doubling, at most 5 minutes.
fn backoff_ms(attempts: i64) -> i64 {
    let exponent = u32::try_from(attempts.clamp(0, 16)).unwrap_or(16);
    (5_000_i64.saturating_mul(1_i64 << exponent)).min(5 * 60 * 1000)
}

/// Whether `oid` is all zeros (a ref deletion or creation).
pub(crate) fn is_zero(oid: &str) -> bool {
    oid.bytes().all(|b| b == b'0')
}

/// Whether a ref update is checked: a branch or tag that was not deleted.
pub(crate) fn is_checked_ref(ref_name: &str, new: &str) -> bool {
    (ref_name.starts_with("refs/heads/") || ref_name.starts_with("refs/tags/")) && !is_zero(new)
}

/// Runs the `verify` action now. The Repo actor's schedule calls it; tests call it directly.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Verify {}

impl Action for Verify {
    type Output = ();

    const NAME: &'static str = "verify";
}

/// Queues a check of a ref update in the transaction that records it.
pub(crate) async fn enqueue(
    tx: &SqliteTransaction,
    seq: i64,
    ref_name: &str,
    old: &str,
    new: &str,
) -> Result<()> {
    let rule = tx
        .execute("SELECT rule FROM signing_rule WHERE id = 1", None)
        .await?
        .rows
        .first()
        .map(|row| text(row, 0))
        .transpose()?;
    tx.execute(
        "INSERT OR IGNORE INTO signature_checks (seq, ref_name, old, new, rule, next_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        Some(vec![
            param_integer(seq),
            param_text(ref_name),
            param_optional_text((!is_zero(old)).then(|| old.to_owned())),
            param_text(new),
            param_optional_text(rule),
            param_integer(now_ms()),
        ]),
    )
    .await?;
    Ok(())
}

/// One queued check.
#[derive(Clone, Debug)]
pub(crate) struct CheckRow {
    pub(crate) seq: i64,
    pub(crate) ref_name: String,
    pub(crate) old: Option<String>,
    pub(crate) new: String,
    pub(crate) rule: Option<String>,
    pub(crate) attempts: i64,
    pub(crate) next_at: i64,
    pub(crate) last_error: Option<String>,
    pub(crate) fetched_dids: Vec<String>,
    pub(crate) first_attempt_at: Option<i64>,
    pub(crate) failures: Vec<(String, StoredFailure)>,
}

/// A failure kept between attempts, for the pending answer and the seven-day rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredFailure {
    pub(crate) reason: String,
    pub(crate) waiting: bool,
    pub(crate) did: Option<String>,
}

impl From<&Failure> for StoredFailure {
    fn from(failure: &Failure) -> Self {
        Self {
            reason: failure.reason.clone(),
            waiting: failure.waiting,
            did: failure.did.clone(),
        }
    }
}

const CHECK_COLUMNS: &str = "seq, ref_name, old, new, rule, attempts, next_at, last_error, \
    fetched_dids, first_attempt_at, failures";

fn check_row(row: &[ColumnValue]) -> Result<CheckRow> {
    Ok(CheckRow {
        seq: integer(row, 0)?,
        ref_name: text(row, 1)?,
        old: optional_text(row, 2)?,
        new: text(row, 3)?,
        rule: optional_text(row, 4)?,
        attempts: integer(row, 5)?,
        next_at: integer(row, 6)?,
        last_error: optional_text(row, 7)?,
        fetched_dids: serde_json::from_str(&text(row, 8)?).unwrap_or_default(),
        first_attempt_at: optional_integer(row, 9)?,
        failures: serde_json::from_str(&text(row, 10)?).unwrap_or_default(),
    })
}

/// The parsed rule a check row copied, if any.
pub(crate) fn parse_rule(rule: Option<&str>) -> Option<SigningRule> {
    rule.and_then(|rule| SigningRule::from_json(rule).ok())
}

/// What the rule copied into a row asks of its ref.
pub(crate) fn coverage(rule: Option<&str>, ref_name: &str) -> Coverage {
    match parse_rule(rule) {
        Some(rule) => rule.coverage(ref_name),
        None => Coverage::Uncovered,
    }
}

/// One judged object of an update.
#[derive(Clone, Debug, Serialize)]
struct Judged {
    oid: String,
    kind: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// The objects an update lists for judging, and whether its walk stopped at the limit.
struct Listing {
    listed: Vec<(String, ObjectKind)>,
    truncated: bool,
    tip_not_commit: Option<String>,
    /// No walk ever succeeded, so only the tip is known.
    no_walk: bool,
}

/// What an attempt left unfinished, with why.
#[derive(Default)]
struct Unfinished {
    failures: HashMap<String, Failure>,
    carried: HashSet<String>,
    deferred: HashSet<String>,
}

fn options(name: &'static str) -> SqliteTransactionOptions<'static> {
    SqliteTransactionOptions {
        name: Some(name),
        timeout: None,
    }
}

impl Repo {
    /// Makes sure a `verify` schedule is pending at or before `at` (clock milliseconds),
    /// registering one only when none is pending or the pending one is later (design D8, *One
    /// pending schedule*).
    pub(crate) async fn ensure_verify_at(&self, ctx: &Ctx<Self>, at: i64) -> Result<()> {
        let _lock = self.verify_schedule_lock.lock().await;
        let stored = ctx
            .sql()
            .query(
                "SELECT event_id, at FROM verify_schedule WHERE id = 1",
                None,
            )
            .await?;
        if let Some(row) = stored.rows.first() {
            let (event_id, pending_at) = (text(row, 0)?, integer(row, 1)?);
            if ctx.schedule().get(&event_id).await?.is_some() {
                if pending_at <= at {
                    return Ok(());
                }
                ctx.schedule().cancel(&event_id).await?;
            }
            ctx.sql()
                .execute("DELETE FROM verify_schedule WHERE id = 1", None)
                .await?;
        }
        let real = real_time_ms(services()?.clock.as_ref(), at);
        let args = rivetkit::action::encode_positional(&Verify {})?;
        let event_id = ctx.schedule().at(real, Verify::NAME, &args).await?;
        ctx.sql()
            .execute(
                "INSERT INTO verify_schedule (id, event_id, at) VALUES (1, ?, ?)
                 ON CONFLICT (id) DO UPDATE SET event_id = excluded.event_id, at = excluded.at",
                Some(vec![param_text(event_id.as_str()), param_integer(at)]),
            )
            .await?;
        Ok(())
    }

    /// The time the next run should start: now when due rows remain, else the earliest
    /// `next_at`, or `None` when nothing is queued.
    async fn next_verify_at(&self, ctx: &Ctx<Self>) -> Result<Option<i64>> {
        let result = ctx
            .sql()
            .query("SELECT MIN(next_at) FROM signature_checks", None)
            .await?;
        let earliest = result
            .rows
            .first()
            .map(|row| optional_integer(row, 0))
            .transpose()?
            .flatten();
        Ok(earliest.map(|at| at.max(now_ms())))
    }

    /// Schedules `verify` for the queue, as `on_start` and the end of each run do.
    pub(crate) async fn schedule_queue(&self, ctx: &Ctx<Self>) -> Result<()> {
        if let Some(at) = self.next_verify_at(ctx).await? {
            self.ensure_verify_at(ctx, at).await?;
        }
        Ok(())
    }

    /// The `verify` action: one serialised run over the due rows, then exactly one
    /// [`Self::ensure_verify_at`] on every exit path (design D8, *Serialised runs*).
    pub(crate) async fn verify(self: &Arc<Self>, ctx: &Ctx<Self>) -> Result<()> {
        let Ok(guard) = self.verifying.try_lock() else {
            self.verify_rerun.store(true, Ordering::SeqCst);
            return Ok(());
        };
        let passed = self.verify_pass(ctx).await;
        drop(guard);
        let scheduled = self.schedule_queue(ctx).await;
        passed.and(scheduled)
    }

    async fn verify_pass(self: &Arc<Self>, ctx: &Ctx<Self>) -> Result<()> {
        let started = tokio::time::Instant::now();
        let budget_end = started + RUN_BUDGET;
        let verifier = Arc::new(Verifier::new(ctx.client()?, services()?.resolver.clone()));
        let mut worked = 0usize;
        loop {
            self.verify_rerun.store(false, Ordering::SeqCst);
            while worked < ROWS_PER_RUN && tokio::time::Instant::now() < budget_end {
                let Some(row) = self.next_due(ctx).await? else {
                    break;
                };
                let deadline = (tokio::time::Instant::now() + ATTEMPT_DEADLINE).min(budget_end);
                self.attempt(ctx, &verifier, row, deadline).await?;
                worked += 1;
            }
            let more = self.verify_rerun.swap(false, Ordering::SeqCst);
            if !more || worked >= ROWS_PER_RUN || tokio::time::Instant::now() >= budget_end {
                return Ok(());
            }
        }
    }

    /// The next due row by priority: the head update, then rows whose copied rule covers
    /// their ref, then the rest, each in `seq` order (design D8, *Bounded passes*).
    async fn next_due(&self, ctx: &Ctx<Self>) -> Result<Option<CheckRow>> {
        let due = ctx
            .sql()
            .query(
                &format!(
                    "SELECT {CHECK_COLUMNS} FROM signature_checks WHERE next_at <= ? ORDER BY seq"
                ),
                Some(vec![param_integer(now_ms())]),
            )
            .await?;
        let rows: Vec<CheckRow> = due
            .rows
            .iter()
            .map(|r| check_row(r))
            .collect::<Result<_>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let head = self.head_update(ctx).await?;
        let rank = |row: &CheckRow| {
            if head.as_ref() == Some(&(row.seq, row.ref_name.clone())) {
                0
            } else if coverage(row.rule.as_deref(), &row.ref_name).is_covered() {
                1
            } else {
                2
            }
        };
        Ok(rows.into_iter().min_by_key(|row| (rank(row), row.seq)))
    }

    /// The head update (design D8, *The head update*): the `main` update with the greatest
    /// `seq` across queued and judged updates whose `new` is `config_head`.
    pub(crate) async fn head_update(&self, ctx: &Ctx<Self>) -> Result<Option<(i64, String)>> {
        let result = ctx
            .sql()
            .query(
                "SELECT MAX(seq) FROM (
                   SELECT seq FROM signature_checks WHERE ref_name = ?1
                     AND new = (SELECT sha FROM config_head WHERE id = 1)
                   UNION ALL
                   SELECT seq FROM update_signing WHERE ref_name = ?1
                     AND new = (SELECT sha FROM config_head WHERE id = 1))",
                Some(vec![param_text(CONFIG_BRANCH)]),
            )
            .await?;
        Ok(result
            .rows
            .first()
            .map(|row| optional_integer(row, 0))
            .transpose()?
            .flatten()
            .map(|seq| (seq, CONFIG_BRANCH.to_owned())))
    }

    /// The oids among `oids` that have a stored result.
    async fn stored_among(
        &self,
        ctx: &Ctx<Self>,
        oids: &[String],
    ) -> Result<HashMap<String, StoredRow>> {
        let mut out = HashMap::new();
        for chunk in oids.chunks(400) {
            let marks = vec!["?"; chunk.len()].join(", ");
            let params: Vec<BindParam> = chunk.iter().map(|oid| param_text(oid.as_str())).collect();
            let result = ctx
                .sql()
                .query(
                    &format!(
                        "SELECT {STORED_COLUMNS} FROM object_signatures WHERE oid IN ({marks})"
                    ),
                    Some(params),
                )
                .await?;
            for row in &result.rows {
                let stored = stored_row(row)?;
                out.insert(stored.oid.clone(), stored);
            }
        }
        Ok(out)
    }

    /// One attempt at one queued row (design D8, *Bounded attempts*).
    async fn attempt(
        self: &Arc<Self>,
        ctx: &Ctx<Self>,
        verifier: &Arc<Verifier>,
        row: CheckRow,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        let now = now_ms();
        let first = row.first_attempt_at.unwrap_or(now);
        let (org, repo) = self.path(ctx).await?;
        let hidden: Vec<String> = row.old.iter().cloned().collect();
        let host = services()?.git_host.clone();
        // Results already stored are reused, not read again.
        let known: HashSet<String> = ctx
            .sql()
            .query("SELECT oid FROM object_signatures", None)
            .await?
            .rows
            .iter()
            .map(|r| text(r, 0))
            .collect::<Result<_>>()?;
        let walked = host
            .read_new_objects(
                org.as_str(),
                repo.as_str(),
                &row.new,
                &hidden,
                WALK_LIMIT,
                &move |oid| known.contains(oid),
            )
            .await;
        let walk = match walked {
            Ok(walk) => walk,
            Err(error) => {
                let gone = error.downcast_ref::<ObjectNotServed>().is_some();
                let reason = if gone {
                    format!("object {} is no longer on the host", row.new)
                } else {
                    format!("the Git host failed: {error:#}")
                };
                let listing = self.stored_listing(ctx, &row).await?;
                let mut unfinished = Unfinished::default();
                for (oid, failure) in &row.failures {
                    unfinished.failures.insert(
                        oid.clone(),
                        Failure {
                            reason: failure.reason.clone(),
                            waiting: false,
                            did: failure.did.clone(),
                        },
                    );
                }
                let tip_failure = Failure {
                    reason: reason.clone(),
                    waiting: false,
                    did: None,
                };
                if gone || now - first >= WINDOW_MS {
                    return self
                        .give_up(ctx, row, listing, unfinished, Some(tip_failure))
                        .await;
                }
                return self
                    .retry_later(ctx, &row, first, &reason, &[], false)
                    .await;
            }
        };
        self.save_listing(
            ctx,
            &row,
            &walk.listed,
            walk.truncated,
            walk.tip_not_commit.as_deref(),
        )
        .await?;
        let listing = Listing {
            listed: walk.listed.clone(),
            truncated: walk.truncated,
            tip_not_commit: walk.tip_not_commit.clone(),
            no_walk: false,
        };

        // Local steps first; Tartan signatures go to the network within the signer caps.
        let mode = services()?.resolver.mode();
        let mut finals: Vec<Stored> = Vec::new();
        let mut tartan: Vec<(RawObject, Local)> = Vec::new();
        for raw in walk.raw {
            match classify(&raw, mode) {
                Local::Final(stored) => finals.push(stored),
                local @ Local::Tartan { .. } => tartan.push((raw, local)),
            }
        }
        let mut fetched: Vec<String> = row.fetched_dids.clone();
        let mut costed: HashSet<String> = HashSet::new();
        let mut unfinished = Unfinished::default();
        let mut to_check = Vec::new();
        for (raw, local) in tartan {
            let Local::Tartan { block, .. } = &local else {
                continue;
            };
            let did = block.did.to_string();
            let in_update = fetched.contains(&did);
            if !in_update && fetched.len() >= SIGNERS_PER_UPDATE {
                unfinished.deferred.insert(raw.oid.clone());
                continue;
            }
            let costs = !verifier.has_resolved(&did) && !costed.contains(&did);
            if costs && costed.len() >= NEW_SIGNERS_PER_ATTEMPT {
                unfinished.carried.insert(raw.oid.clone());
                continue;
            }
            if costs {
                costed.insert(did.clone());
            }
            if !in_update {
                fetched.push(did);
            }
            to_check.push((raw, local));
        }

        let permits = Arc::new(tokio::sync::Semaphore::new(OBJECTS_IN_FLIGHT));
        let mut tasks = tokio::task::JoinSet::new();
        for (raw, local) in to_check {
            let Local::Tartan {
                block,
                payload,
                committed_at,
            } = local
            else {
                continue;
            };
            let (verifier, permits) = (verifier.clone(), permits.clone());
            tasks.spawn(async move {
                let oid = raw.oid.clone();
                let checked = tokio::time::timeout_at(deadline, async {
                    let _permit = permits.acquire().await;
                    verifier.verify(&raw, block, payload, committed_at).await
                })
                .await;
                (oid, checked.ok())
            });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((_, Some(Outcome::Final(stored)))) => finals.push(stored),
                Ok((oid, Some(Outcome::Failed(failure)))) => {
                    unfinished.failures.insert(oid, failure);
                }
                Ok((oid, None)) => {
                    unfinished.carried.insert(oid);
                }
                Err(error) => {
                    tracing::error!(repo = %self.id, %error, "a signature check panicked")
                }
            }
        }
        // Progress is kept: final results are stored whatever happens to the update.
        self.store_results(ctx, finals).await?;

        if unfinished.failures.is_empty() && unfinished.carried.is_empty() {
            return self.resolve(ctx, row, listing, unfinished, false).await;
        }
        if unfinished.failures.is_empty() {
            // Only carried objects: continue at once, without counting an attempt.
            ctx.sql()
                .execute(
                    "UPDATE signature_checks SET next_at = ?, fetched_dids = ?,
                       first_attempt_at = COALESCE(first_attempt_at, ?), failures = '[]'
                     WHERE seq = ? AND ref_name = ?",
                    Some(vec![
                        param_integer(now_ms()),
                        param_text(serde_json::to_string(&fetched)?),
                        param_integer(first),
                        param_integer(row.seq),
                        param_text(row.ref_name.as_str()),
                    ]),
                )
                .await?;
            return Ok(());
        }
        let cover = coverage(row.rule.as_deref(), &row.ref_name);
        let waiting = cover.is_covered()
            && unfinished.failures.values().all(|failure| {
                failure.waiting && failure.did.as_deref().is_some_and(|did| cover.allows(did))
            });
        let window = if waiting { SIGNER_WINDOW_MS } else { WINDOW_MS };
        if now - first >= window {
            return self.give_up(ctx, row, listing, unfinished, None).await;
        }
        let failures: Vec<(String, StoredFailure)> = unfinished
            .failures
            .iter()
            .map(|(oid, failure)| (oid.clone(), StoredFailure::from(failure)))
            .collect();
        let reason = unfinished
            .failures
            .values()
            .next()
            .map(|failure| failure.reason.clone())
            .unwrap_or_default();
        let mut stored_row = row;
        stored_row.fetched_dids = fetched;
        self.retry_later(ctx, &stored_row, first, &reason, &failures, waiting)
            .await
    }

    /// Records a failed attempt: `attempts`, `next_at` and `last_error`; registers nothing.
    async fn retry_later(
        &self,
        ctx: &Ctx<Self>,
        row: &CheckRow,
        first: i64,
        reason: &str,
        failures: &[(String, StoredFailure)],
        waiting: bool,
    ) -> Result<()> {
        let now = now_ms();
        let attempts = row.attempts + 1;
        let next_at = if waiting && now - first >= WINDOW_MS {
            now + HOURLY_MS
        } else {
            now + backoff_ms(attempts - 1)
        };
        ctx.sql()
            .execute(
                "UPDATE signature_checks SET attempts = ?, next_at = ?, last_error = ?,
                   fetched_dids = ?, first_attempt_at = COALESCE(first_attempt_at, ?),
                   failures = ?
                 WHERE seq = ? AND ref_name = ?",
                Some(vec![
                    param_integer(attempts),
                    param_integer(next_at),
                    param_text(reason),
                    param_text(serde_json::to_string(&row.fetched_dids)?),
                    param_integer(first),
                    param_text(serde_json::to_string(failures)?),
                    param_integer(row.seq),
                    param_text(row.ref_name.as_str()),
                ]),
            )
            .await?;
        tracing::info!(repo = %self.id, seq = row.seq, ref_name = %row.ref_name, %reason,
            attempts, "signature check failed; retrying later");
        Ok(())
    }

    /// Keeps the latest walk's listing for a row.
    async fn save_listing(
        &self,
        ctx: &Ctx<Self>,
        row: &CheckRow,
        listed: &[(String, ObjectKind)],
        truncated: bool,
        tip_not_commit: Option<&str>,
    ) -> Result<()> {
        let (seq, ref_name) = (row.seq, row.ref_name.clone());
        let listed = listed.to_vec();
        let tip_not_commit = tip_not_commit.map(str::to_owned);
        ctx.sql()
            .transaction(
                move |tx| async move {
                    tx.execute(
                        "DELETE FROM check_objects WHERE seq = ? AND ref_name = ?",
                        Some(vec![param_integer(seq), param_text(ref_name.as_str())]),
                    )
                    .await?;
                    for (position, (oid, kind)) in listed.iter().enumerate() {
                        tx.execute(
                            "INSERT INTO check_objects (seq, ref_name, position, oid, kind)
                             VALUES (?, ?, ?, ?, ?)",
                            Some(vec![
                                param_integer(seq),
                                param_text(ref_name.as_str()),
                                param_integer(i64::try_from(position)?),
                                param_text(oid.as_str()),
                                param_text(kind.as_str()),
                            ]),
                        )
                        .await?;
                    }
                    tx.execute(
                        "UPDATE signature_checks SET walked = 1, truncated = ?, tip_not_commit = ?
                         WHERE seq = ? AND ref_name = ?",
                        Some(vec![
                            param_integer(i64::from(truncated)),
                            param_optional_text(tip_not_commit),
                            param_integer(seq),
                            param_text(ref_name.as_str()),
                        ]),
                    )
                    .await?;
                    Ok(())
                },
                options("save_listing"),
            )
            .await
    }

    /// The listing an earlier walk of `row` saved, or the tip alone when no walk succeeded.
    async fn stored_listing(&self, ctx: &Ctx<Self>, row: &CheckRow) -> Result<Listing> {
        let state = ctx
            .sql()
            .query(
                "SELECT walked, truncated, tip_not_commit FROM signature_checks
                 WHERE seq = ? AND ref_name = ?",
                Some(vec![
                    param_integer(row.seq),
                    param_text(row.ref_name.as_str()),
                ]),
            )
            .await?;
        let (walked, truncated, tip_not_commit) = match state.rows.first() {
            Some(r) => (
                integer(r, 0)? == 1,
                integer(r, 1)? == 1,
                optional_text(r, 2)?,
            ),
            None => (false, false, None),
        };
        if !walked {
            return Ok(Listing {
                listed: vec![(row.new.clone(), ObjectKind::Commit)],
                truncated: false,
                tip_not_commit: None,
                no_walk: true,
            });
        }
        let listed = ctx
            .sql()
            .query(
                "SELECT oid, kind FROM check_objects WHERE seq = ? AND ref_name = ? ORDER BY position",
                Some(vec![param_integer(row.seq), param_text(row.ref_name.as_str())]),
            )
            .await?
            .rows
            .iter()
            .map(|r| {
                Ok((
                    text(r, 0)?,
                    ObjectKind::parse(&text(r, 1)?).unwrap_or(ObjectKind::Commit),
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Listing {
            listed,
            truncated,
            tip_not_commit,
            no_walk: false,
        })
    }

    /// Writes final results and their bundles, then tells the identity actor of each new
    /// verified record (design D8 step 6).
    async fn store_results(&self, ctx: &Ctx<Self>, results: Vec<Stored>) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        let objects = services()?.objects.clone();
        for result in &results {
            if let Some((key, _, bytes)) = &result.bundle {
                let path = object_store::path::Path::from(key.as_str());
                let put = objects
                    .put_opts(
                        &path,
                        bytes.clone().into(),
                        object_store::PutOptions::from(object_store::PutMode::Create),
                    )
                    .await;
                match put {
                    Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let now = now_ms();
        let rows = results.clone();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    for result in rows {
                        insert_result(&tx, &result, now).await?;
                    }
                    Ok(())
                },
                options("store_signature_results"),
            )
            .await?;
        self.record_verified(ctx, results.iter().filter(|r| r.status == Status::Verified))
            .await;
        Ok(())
    }

    /// Calls `RecordSignature` for verified results and marks them recorded. A failure leaves
    /// them unrecorded, and `on_start` replays them.
    async fn record_verified<'a>(
        &self,
        ctx: &Ctx<Self>,
        results: impl Iterator<Item = &'a Stored>,
    ) {
        let Ok(client) = ctx.client() else {
            return;
        };
        let Ok(identity) = identity_handle(&client) else {
            return;
        };
        for result in results {
            let (Some(did), Some(rkey), Some(cid), Some(rev)) = (
                result.did.clone(),
                result.rkey.clone(),
                result.record_cid.clone(),
                result.record_rev.clone(),
            ) else {
                continue;
            };
            let recorded = identity
                .send(RecordSignature {
                    did,
                    rkey,
                    cid,
                    rev,
                })
                .await;
            match recorded {
                Ok(_) => {
                    let marked = ctx
                        .sql()
                        .execute(
                            "UPDATE object_signatures SET recorded = 1 WHERE oid = ?",
                            Some(vec![param_text(result.oid.as_str())]),
                        )
                        .await;
                    if let Err(error) = marked {
                        tracing::warn!(repo = %self.id, %error, "could not mark a record as recorded");
                    }
                }
                Err(error) => {
                    tracing::warn!(repo = %self.id, oid = %result.oid, %error,
                        "could not track a verified signature record; it is replayed on start");
                }
            }
        }
    }

    /// Replays `RecordSignature` for verified results the identity actor was not told of.
    pub(crate) async fn replay_recorded(&self, ctx: &Ctx<Self>) -> Result<()> {
        let pending = ctx
            .sql()
            .query(
                &format!(
                    "SELECT {STORED_COLUMNS} FROM object_signatures
                     WHERE status = 'verified' AND recorded = 0"
                ),
                None,
            )
            .await?;
        let rows: Vec<Stored> = pending
            .rows
            .iter()
            .map(|r| stored_row(r).map(StoredRow::into_stored))
            .collect::<Result<_>>()?;
        self.record_verified(ctx, rows.iter()).await;
        Ok(())
    }

    /// Gives up on a row (design D8, *Giving up*): failing objects become `unverifiable`, carried
    /// and deferred objects are judged `not_checked`, and the update is judged.
    async fn give_up(
        self: &Arc<Self>,
        ctx: &Ctx<Self>,
        row: CheckRow,
        listing: Listing,
        mut unfinished: Unfinished,
        tip_failure: Option<Failure>,
    ) -> Result<()> {
        if let Some(failure) = tip_failure
            && listing.no_walk
        {
            unfinished
                .failures
                .entry(row.new.clone())
                .or_insert(failure);
        }
        tracing::warn!(repo = %self.id, seq = row.seq, ref_name = %row.ref_name,
            "signature check gave up");
        self.resolve(ctx, row, listing, unfinished, true).await
    }

    /// Judges a row's update and removes it from the queue (design D8 step 6, D11
    /// *Compliance*), then hands a resolved head update to reconciliation.
    async fn resolve(
        self: &Arc<Self>,
        ctx: &Ctx<Self>,
        row: CheckRow,
        listing: Listing,
        unfinished: Unfinished,
        gave_up: bool,
    ) -> Result<()> {
        let now = now_ms();
        let oids: Vec<String> = listing.listed.iter().map(|(oid, _)| oid.clone()).collect();
        let mut stored = self.stored_among(ctx, &oids).await?;

        // Objects still failing when the check gives up are stored as `unverifiable`.
        let mut unverifiable = Vec::new();
        if gave_up {
            for (oid, kind) in &listing.listed {
                if stored.contains_key(oid) {
                    continue;
                }
                if let Some(failure) = unfinished.failures.get(oid) {
                    let result = Stored {
                        oid: oid.clone(),
                        kind: *kind,
                        status: Status::Unverifiable,
                        did: failure.did.clone(),
                        rkey: None,
                        record_cid: None,
                        record_rev: None,
                        detail: Some(failure.reason.clone()),
                        committed_at: None,
                        bundle: None,
                    };
                    stored.insert(oid.clone(), StoredRow::from_stored(&result, now));
                    unverifiable.push(result);
                }
            }
        }

        // Standing of every verified result judged (design D8, *Standing*).
        let pairs: Vec<RecordKey> = listing
            .listed
            .iter()
            .filter_map(|(oid, _)| stored.get(oid))
            .filter(|s| s.status == Status::Verified)
            .filter_map(|s| {
                Some(RecordKey {
                    did: s.did.clone()?,
                    rkey: s.rkey.clone()?,
                })
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let standing = self.standing(ctx, &pairs).await?;

        let cover = coverage(row.rule.as_deref(), &row.ref_name);
        let mut judged = Vec::new();
        let mut problem: Option<String> = None;
        // Every commit that keeps the update from complying, as the ref answer lists them
        // (design D15, *Answer shapes*); `problem` is the first, as text.
        let mut violations: Vec<Value> = Vec::new();
        let mut update_objects: Vec<(String, Option<String>)> = Vec::new();
        let mut note = |oid: &str, reason: String, did: Option<&str>| {
            let text = format!("{oid} {reason}");
            if problem.is_none() {
                problem = Some(text);
            }
            violations.push(json!({
                "oid": oid,
                "problem": reason,
                "did": did,
                "allowed": did.is_some_and(|did| cover.allows(did)),
            }));
        };
        for (oid, kind) in &listing.listed {
            match stored.get(oid) {
                Some(result) => {
                    match result.status {
                        Status::Verified => {
                            let key = (result.did.clone(), result.rkey.clone());
                            let standing = match key {
                                (Some(did), Some(rkey)) => standing.get(&(did, rkey)),
                                _ => None,
                            };
                            let did = result.did.as_deref();
                            if standing.is_some_and(|s| s.revoked_at.is_some()) {
                                note(oid, "signature was revoked".to_owned(), did);
                            } else if standing.is_some_and(|s| s.inactive) {
                                note(oid, "signer's account is not active".to_owned(), did);
                            } else if let Some(did) = did
                                && !cover.allows(did)
                            {
                                note(
                                    oid,
                                    format!(
                                        "is signed by {did}, who may not sign {}",
                                        row.ref_name
                                    ),
                                    Some(did),
                                );
                            }
                        }
                        Status::Unverifiable => {
                            update_objects.push((oid.clone(), None));
                            note(oid, "is unverifiable".to_owned(), result.did.as_deref());
                        }
                        other => note(oid, format!("is {}", other.as_str()), result.did.as_deref()),
                    }
                    judged.push(Judged {
                        oid: oid.clone(),
                        kind: kind.as_str(),
                        status: result.status.as_str(),
                        did: result.did.clone(),
                        detail: result.detail.clone(),
                    });
                }
                None => {
                    let detail = if unfinished.deferred.contains(oid) {
                        DEFERRED_DETAIL
                    } else {
                        CARRIED_DETAIL
                    };
                    update_objects.push((oid.clone(), Some(detail.to_owned())));
                    note(oid, format!("was not checked: {detail}"), None);
                    judged.push(Judged {
                        oid: oid.clone(),
                        kind: kind.as_str(),
                        status: Status::NotChecked.as_str(),
                        did: None,
                        detail: Some(detail.to_owned()),
                    });
                }
            }
        }
        if let Some(target) = &listing.tip_not_commit {
            let detail = "the tag does not point to a commit";
            note(target, format!("was not checked: {detail}"), None);
            judged.push(Judged {
                oid: target.clone(),
                kind: "object",
                status: Status::NotChecked.as_str(),
                did: None,
                detail: Some(detail.to_owned()),
            });
        }
        if listing.truncated {
            note(
                &row.new,
                "was not checked in full: too many commits in one push".to_owned(),
                None,
            );
        }
        if listing.no_walk && !update_objects.iter().any(|(oid, _)| *oid == row.new) {
            // The commits the update adds were never listed.
            update_objects.push((row.new.clone(), None));
            note(&row.new, "is unverifiable".to_owned(), None);
        }
        let compliant = problem.is_none();
        let covered = cover.is_covered();

        let source = self.source();
        let row_for_tx = row.clone();
        let problem_for_tx = problem.clone();
        let violations_text = Value::Array(violations.clone()).to_string();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let row = row_for_tx;
                    for result in &unverifiable {
                        insert_result(&tx, result, now).await?;
                    }
                    tx.execute(
                        "INSERT INTO update_signing
                           (seq, ref_name, old, new, rule, compliant, problem, violations,
                            checked_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                         ON CONFLICT (seq, ref_name) DO UPDATE SET old = excluded.old,
                           new = excluded.new, rule = excluded.rule,
                           compliant = excluded.compliant, problem = excluded.problem,
                           violations = excluded.violations, checked_at = excluded.checked_at",
                        Some(vec![
                            param_integer(row.seq),
                            param_text(row.ref_name.as_str()),
                            param_optional_text(row.old.clone()),
                            param_text(row.new.as_str()),
                            param_optional_text(row.rule.clone()),
                            param_integer(i64::from(compliant)),
                            param_optional_text(problem_for_tx.clone()),
                            param_text(violations_text.as_str()),
                            param_integer(now),
                        ]),
                    )
                    .await?;
                    tx.execute(
                        "DELETE FROM update_objects WHERE seq = ? AND ref_name = ?",
                        Some(vec![param_integer(row.seq), param_text(row.ref_name.as_str())]),
                    )
                    .await?;
                    for (oid, deferred) in &update_objects {
                        tx.execute(
                            "INSERT OR REPLACE INTO update_objects (seq, ref_name, oid, deferred)
                             VALUES (?, ?, ?, ?)",
                            Some(vec![
                                param_integer(row.seq),
                                param_text(row.ref_name.as_str()),
                                param_text(oid.as_str()),
                                param_optional_text(deferred.clone()),
                            ]),
                        )
                        .await?;
                    }
                    let violation = (covered && !compliant).then_some(row.seq);
                    tx.execute(
                        "INSERT INTO ref_signing (ref_name, seq, head, compliant, violation_seq, checked_at)
                         VALUES (?, ?, ?, ?, ?, ?)
                         ON CONFLICT (ref_name) DO UPDATE SET seq = excluded.seq,
                           head = excluded.head, compliant = excluded.compliant,
                           violation_seq = excluded.violation_seq, checked_at = excluded.checked_at
                         WHERE excluded.seq >= ref_signing.seq",
                        Some(vec![
                            param_text(row.ref_name.as_str()),
                            param_integer(row.seq),
                            param_text(row.new.as_str()),
                            param_integer(i64::from(compliant)),
                            violation.map_or(BindParam::Null, param_integer),
                            param_integer(now),
                        ]),
                    )
                    .await?;
                    let data = json!({
                        "ref_name": row.ref_name,
                        "host_seq": row.seq,
                        "old": row.old,
                        "new": row.new,
                        "covered": covered,
                        "compliant": compliant,
                        "problem": problem_for_tx,
                        "truncated": listing.truncated,
                        "objects": judged,
                    });
                    append(&tx, &source, "ref.signatures.checked", data).await?;
                    if covered && !compliant {
                        let data = json!({
                            "ref_name": row.ref_name,
                            "host_seq": row.seq,
                            "new": row.new,
                            "problem": problem_for_tx,
                            "violations": violations,
                        });
                        append(&tx, &source, "ref.signature.violation", data).await?;
                    }
                    for statement in [
                        "DELETE FROM signature_checks WHERE seq = ? AND ref_name = ?",
                        "DELETE FROM check_objects WHERE seq = ? AND ref_name = ?",
                    ] {
                        tx.execute(
                            statement,
                            Some(vec![param_integer(row.seq), param_text(row.ref_name.as_str())]),
                        )
                        .await?;
                    }
                    Ok(())
                },
                options("judge_update"),
            )
            .await?;
        self.outbox.flush_later(ctx, SubscriberSource::Forge);
        tracing::info!(repo = %self.id, seq = row.seq, ref_name = %row.ref_name, compliant,
            problem = problem.as_deref().unwrap_or(""), "ref update judged");

        // Hand-off: a resolved head update is decided now, inside this scheduled wake.
        if row.ref_name == CONFIG_BRANCH
            && self.head_update(ctx).await? == Some((row.seq, row.ref_name.clone()))
        {
            match self.reconcile(ctx, false).await {
                Ok(crate::actors::repo::Reconciled::Done) => {}
                Ok(crate::actors::repo::Reconciled::Later(_)) | Err(_) => self.reconcile_later(ctx),
            }
        }
        Ok(())
    }

    /// The standing of each pair, keyed by `(did, rkey)`, from local state only.
    async fn standing(
        &self,
        ctx: &Ctx<Self>,
        pairs: &[RecordKey],
    ) -> Result<HashMap<(String, String), Standing>> {
        let mut out = HashMap::new();
        if pairs.is_empty() {
            return Ok(out);
        }
        let identity = identity_handle(&ctx.client()?)?;
        for chunk in pairs.chunks(tartan_identity_api::MAX_STANDING_PAIRS) {
            let answers = identity
                .send(SignerStanding {
                    pairs: chunk.to_vec(),
                })
                .await?;
            for (pair, standing) in chunk.iter().zip(answers) {
                out.insert((pair.did.clone(), pair.rkey.clone()), standing);
            }
        }
        Ok(out)
    }

    /// Whether the head update holds the configuration back: its row is queued and its copied
    /// rule covers `main` (design D11, *Configuration gating*). Answers the refusal when its
    /// judged row is not compliant under a rule that covers `main`.
    pub(crate) async fn signing_gate(&self, ctx: &Ctx<Self>) -> Result<Gate> {
        let Some((seq, _)) = self.head_update(ctx).await? else {
            return Ok(Gate::Open);
        };
        let queued = ctx
            .sql()
            .query(
                "SELECT rule FROM signature_checks WHERE seq = ? AND ref_name = ?",
                Some(vec![param_integer(seq), param_text(CONFIG_BRANCH)]),
            )
            .await?;
        if let Some(r) = queued.rows.first() {
            return Ok(
                if coverage(optional_text(r, 0)?.as_deref(), CONFIG_BRANCH).is_covered() {
                    Gate::Waiting
                } else {
                    Gate::Open
                },
            );
        }
        let judged = ctx
            .sql()
            .query(
                "SELECT rule, compliant, problem FROM update_signing WHERE seq = ? AND ref_name = ?",
                Some(vec![param_integer(seq), param_text(CONFIG_BRANCH)]),
            )
            .await?;
        let Some(r) = judged.rows.first() else {
            return Ok(Gate::Open);
        };
        let covered = coverage(optional_text(r, 0)?.as_deref(), CONFIG_BRANCH).is_covered();
        if covered && integer(r, 1)? == 0 {
            let problem =
                optional_text(r, 2)?.unwrap_or_else(|| "the update is not compliant".into());
            return Ok(Gate::Refused(format!("signing: {problem}")));
        }
        Ok(Gate::Open)
    }
}

/// What the signing rule says of the head configuration.
pub(crate) enum Gate {
    /// No signing check holds it.
    Open,
    /// The head update's check is still queued.
    Waiting,
    /// The head update is not compliant: the refusal's problem.
    Refused(String),
}

const STORED_COLUMNS: &str = "oid, kind, status, did, rkey, record_cid, record_rev, proof_sha256, \
    detail, committed_at, checked_at";

/// A stored result as read back.
#[derive(Clone, Debug)]
pub(crate) struct StoredRow {
    pub(crate) oid: String,
    pub(crate) kind: ObjectKind,
    pub(crate) status: Status,
    pub(crate) did: Option<String>,
    pub(crate) rkey: Option<String>,
    pub(crate) record_cid: Option<String>,
    pub(crate) record_rev: Option<String>,
    pub(crate) proof_sha256: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) committed_at: Option<i64>,
    pub(crate) checked_at: i64,
}

impl StoredRow {
    fn from_stored(stored: &Stored, now: i64) -> Self {
        Self {
            oid: stored.oid.clone(),
            kind: stored.kind,
            status: stored.status,
            did: stored.did.clone(),
            rkey: stored.rkey.clone(),
            record_cid: stored.record_cid.clone(),
            record_rev: stored.record_rev.clone(),
            proof_sha256: stored.bundle.as_ref().map(|(_, sha, _)| sha.clone()),
            detail: stored.detail.clone(),
            committed_at: stored.committed_at,
            checked_at: now,
        }
    }

    fn into_stored(self) -> Stored {
        Stored {
            oid: self.oid,
            kind: self.kind,
            status: self.status,
            did: self.did,
            rkey: self.rkey,
            record_cid: self.record_cid,
            record_rev: self.record_rev,
            detail: self.detail,
            committed_at: self.committed_at,
            bundle: None,
        }
    }
}

pub(crate) fn stored_row(row: &[ColumnValue]) -> Result<StoredRow> {
    Ok(StoredRow {
        oid: text(row, 0)?,
        kind: ObjectKind::parse(&text(row, 1)?).unwrap_or(ObjectKind::Commit),
        status: Status::parse(&text(row, 2)?).unwrap_or(Status::Unverifiable),
        did: optional_text(row, 3)?,
        rkey: optional_text(row, 4)?,
        record_cid: optional_text(row, 5)?,
        record_rev: optional_text(row, 6)?,
        proof_sha256: optional_text(row, 7)?,
        detail: optional_text(row, 8)?,
        committed_at: optional_integer(row, 9)?,
        checked_at: integer(row, 10)?,
    })
}

async fn insert_result(tx: &SqliteTransaction, result: &Stored, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO object_signatures
           (oid, kind, status, did, rkey, record_cid, record_rev, proof_sha256, detail,
            committed_at, checked_at, recorded)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        Some(vec![
            param_text(result.oid.as_str()),
            param_text(result.kind.as_str()),
            param_text(result.status.as_str()),
            param_optional_text(result.did.clone()),
            param_optional_text(result.rkey.clone()),
            param_optional_text(result.record_cid.clone()),
            param_optional_text(result.record_rev.clone()),
            param_optional_text(result.bundle.as_ref().map(|(_, sha, _)| sha.clone())),
            param_optional_text(result.detail.clone()),
            result.committed_at.map_or(BindParam::Null, param_integer),
            param_integer(now),
        ]),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_from_five_seconds_to_five_minutes() {
        assert_eq!(backoff_ms(0), 5_000);
        assert_eq!(backoff_ms(1), 10_000);
        assert_eq!(backoff_ms(5), 160_000);
        assert_eq!(backoff_ms(6), 300_000);
        assert_eq!(backoff_ms(40), 300_000);
    }

    #[test]
    fn checks_branches_and_tags_but_not_deletions() {
        let oid = "a".repeat(40);
        assert!(is_checked_ref("refs/heads/main", &oid));
        assert!(is_checked_ref("refs/tags/v1", &oid));
        assert!(!is_checked_ref("refs/notes/commits", &oid));
        assert!(!is_checked_ref("refs/heads/main", &"0".repeat(40)));
    }
}
