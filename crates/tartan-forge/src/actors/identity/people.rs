//! People: the DIDs the forge watches, their verified PDS, key and account status (design D3,
//! D10, D13), and the scheduled sweep.

use std::sync::Arc;

use anyhow::Result;
use rivetkit::prelude::*;
use rivetkit::{ColumnValue, SqliteTransaction};
use serde_json::json;
use tartan_atproto::did::Did;
use tartan_atproto::document::AtprotoIdentity;
use tartan_atproto::fetch::FetchClient;
use tartan_atproto::handle::HandleVerdict;
use tartan_atproto::resolver::VerifyingResolver;
use tartan_identity_api::{
    INVALID_HANDLE, KnowDid, KnownAs, MAX_SIGNERS, People, Person, RefreshIdentity, Refreshed,
    SignedIn, Sweep, SweepReport, WatchedDids,
};

use super::{DAY_MS, HOUR_MS, Identity, SOURCE, parse_did};
use crate::event_log::append;
use crate::services::services;
use crate::sql::{
    integer, now_ms, optional_integer, optional_text, param_integer, param_optional_text,
    param_text, text,
};

/// A known DID whose status is older than this is refreshed by the sweep.
const REFRESH_AFTER_MS: i64 = 6 * HOUR_MS;
/// A DID known only as a signer, or only as signed in, is dropped after this long without a
/// verified signature, or without a live session or token.
const DROP_AFTER_MS: i64 = 30 * DAY_MS;
/// The most DIDs one sweep refreshes; the rest wait for the next run 15 minutes later.
const REFRESH_BATCH: i64 = 100;
/// How many refreshes one sweep runs at once.
const REFRESH_CONCURRENCY: usize = 10;
/// The largest `getRepoStatus` answer read.
const REPO_STATUS_CAP: usize = 64 * 1024;
/// The most DIDs one `People` call looks up.
const PEOPLE_LIMIT: usize = 1000;

/// `known_as` as stored: a sorted JSON array without duplicates, so equal sets compare equal
/// as text.
fn encode_known_as(known_as: &[KnownAs]) -> String {
    let mut set = known_as.to_vec();
    set.sort();
    set.dedup();
    serde_json::to_string(&set).unwrap_or_else(|_| "[]".to_owned())
}

fn decode_known_as(stored: &str) -> Vec<KnownAs> {
    serde_json::from_str(stored).unwrap_or_default()
}

const PERSON_COLUMNS: &str = "did, handle, pds, atproto_key, status, status_checked_at, known_as, \
     first_seen_at, last_signed_at, handle_checked_at";

fn person_from_row(row: &[ColumnValue]) -> Result<Person> {
    Ok(Person {
        did: text(row, 0)?,
        handle: optional_text(row, 1)?,
        pds: optional_text(row, 2)?,
        atproto_key: optional_text(row, 3)?,
        status: text(row, 4)?,
        status_checked_at: optional_integer(row, 5)?,
        known_as: decode_known_as(&text(row, 6)?),
        first_seen_at: integer(row, 7)?,
        last_signed_at: optional_integer(row, 8)?,
        handle_checked_at: optional_integer(row, 9)?,
    })
}

/// Inserts a row for `did` known for nothing yet, if there is none.
async fn ensure_person(tx: &SqliteTransaction, did: &str, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO people (did, known_as, first_seen_at) VALUES (?, '[]', ?)",
        Some(vec![param_text(did), param_integer(now)]),
    )
    .await?;
    Ok(())
}

/// What one refresh learned from the network.
#[derive(Debug, Default)]
struct Learned {
    /// The verified identity, when the DID resolved to an active document.
    identity: Option<AtprotoIdentity>,
    /// The account status, when it could be learned.
    status: Option<String>,
    /// The handle check: the verified handle, [`INVALID_HANDLE`] for an affirmative mismatch,
    /// or `None` when it could not be completed (the last verified handle is kept).
    handle: Option<String>,
}

impl Identity {
    /// Makes `did` known for `because`. Answers whether it is known for that reason now, and
    /// whether its status has never been learned (so the caller should refresh it).
    pub(super) async fn know(
        &self,
        ctx: &Ctx<Self>,
        did: &str,
        because: KnownAs,
    ) -> Result<(bool, bool)> {
        parse_did(did)?;
        let now = now_ms();
        let did = did.to_owned();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let existing = tx
                        .execute(
                            "SELECT known_as, status_checked_at FROM people WHERE did = ?",
                            Some(vec![param_text(did.as_str())]),
                        )
                        .await?;
                    let (mut known_as, never_checked) = match existing.rows.first() {
                        Some(row) => (
                            decode_known_as(&text(row, 0)?),
                            optional_integer(row, 1)?.is_none(),
                        ),
                        None => (Vec::new(), true),
                    };
                    if known_as.contains(&because) {
                        return Ok((true, never_checked));
                    }
                    if because == KnownAs::Signer {
                        let signers = tx
                            .execute(
                                "SELECT COUNT(*) FROM people WHERE known_as LIKE '%\"signer\"%'",
                                None,
                            )
                            .await?;
                        let count = signers.rows.first().map_or(Ok(0), |row| integer(row, 0))?;
                        if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_SIGNERS {
                            tracing::warn!(%did, "the forge-wide signer cap is reached; not watching");
                            return Ok((false, false));
                        }
                    }
                    known_as.push(because);
                    ensure_person(&tx, &did, now).await?;
                    tx.execute(
                        "UPDATE people SET known_as = ? WHERE did = ?",
                        Some(vec![
                            param_text(encode_known_as(&known_as)),
                            param_text(did.as_str()),
                        ]),
                    )
                    .await?;
                    Ok((true, never_checked))
                },
                SqliteTransactionOptions {
                    name: Some("know_did"),
                    timeout: None,
                },
            )
            .await
    }

    /// Re-learns `did`'s identity, handle and account status (design D9, D10 *Account
    /// status*). A changed `#atproto` key or PDS drops the DID's proof-reuse cache.
    pub(super) async fn refresh(&self, ctx: &Ctx<Self>, did: &str) -> Result<Refreshed> {
        let parsed = parse_did(did)?;
        let resolver = services()?.resolver.clone();
        // Identity work runs in its own task, so a panic on hostile input is a failed fetch
        // (which keeps the previous status) rather than a crashed action.
        let learned = match tokio::spawn(learn(resolver, parsed)).await {
            Ok(learned) => learned,
            Err(error) => {
                tracing::warn!(%did, %error, "the identity refresh failed");
                Learned::default()
            }
        };
        let now = now_ms();
        let pds = learned
            .identity
            .as_ref()
            .and_then(|identity| identity.pds.clone());
        let key = learned
            .identity
            .as_ref()
            .and_then(|identity| identity.signing_key.as_ref())
            .map(tartan_atproto::keys::AtprotoKey::did_key);
        let resolved = learned.identity.is_some();
        let did_owned = did.to_owned();
        let stored = (pds.clone(), key.clone());
        let learned_handle = learned.handle.clone();
        let learned_status = learned.status.clone();
        let (status, handle, evidence_changed, recorded) = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    ensure_person(&tx, &did_owned, now).await?;
                    let current = tx
                        .execute(
                            "SELECT status, handle, pds, atproto_key FROM people WHERE did = ?",
                            Some(vec![param_text(did_owned.as_str())]),
                        )
                        .await?;
                    let row = current
                        .rows
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("the person row is missing"))?;
                    let previous_status = text(row, 0)?;
                    let previous_handle = optional_text(row, 1)?;
                    let previous_evidence = (optional_text(row, 2)?, optional_text(row, 3)?);
                    let mut recorded = false;

                    let mut evidence_changed = false;
                    if resolved {
                        evidence_changed = previous_evidence != stored;
                        tx.execute(
                            "UPDATE people SET pds = ?, atproto_key = ? WHERE did = ?",
                            Some(vec![
                                param_optional_text(stored.0),
                                param_optional_text(stored.1),
                                param_text(did_owned.as_str()),
                            ]),
                        )
                        .await?;
                    }

                    let mut handle = previous_handle.clone();
                    if let Some(new) = learned_handle {
                        tx.execute(
                            "UPDATE people SET handle = ?, handle_checked_at = ? WHERE did = ?",
                            Some(vec![
                                param_text(new.as_str()),
                                param_integer(now),
                                param_text(did_owned.as_str()),
                            ]),
                        )
                        .await?;
                        if let Some(old) = &previous_handle
                            && *old != new
                        {
                            let data = json!({ "did": did_owned, "old": old, "new": new });
                            append(&tx, SOURCE, "person.handle.changed", data).await?;
                            recorded = true;
                        }
                        handle = Some(new);
                    }

                    let Some(status) = learned_status else {
                        return Ok((previous_status, handle, evidence_changed, recorded));
                    };
                    tx.execute(
                        "UPDATE people SET status = ?, status_checked_at = ? WHERE did = ?",
                        Some(vec![
                            param_text(status.as_str()),
                            param_integer(now),
                            param_text(did_owned.as_str()),
                        ]),
                    )
                    .await?;
                    if status != previous_status {
                        let data = json!({ "did": did_owned, "status": status });
                        append(&tx, SOURCE, "person.status.changed", data).await?;
                        recorded = true;
                    }
                    Ok((status, handle, evidence_changed, recorded))
                },
                SqliteTransactionOptions {
                    name: Some("refresh_identity"),
                    timeout: None,
                },
            )
            .await?;
        if evidence_changed {
            crate::proof_cache::proof_cache().drop_did(did);
        }
        if recorded {
            self.flush_later(ctx);
        }
        Ok(Refreshed {
            status,
            pds,
            atproto_key: if resolved { key } else { None },
            handle,
            fresh: learned.status.is_some(),
        })
    }

    /// Refreshes `did` and logs a failure: for background refreshes.
    pub(super) async fn refresh_quietly(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        did: String,
    ) -> Option<String> {
        match self.refresh(&ctx, &did).await {
            Ok(_) => Some(did),
            Err(error) => {
                tracing::warn!(%did, %error, "could not refresh an identity");
                None
            }
        }
    }

    /// Runs the sweep (design D3, *Scheduled sweep*).
    async fn sweep(self: Arc<Self>, ctx: Ctx<Self>) -> Result<SweepReport> {
        let expired = delete_expired(&ctx).await?;
        let mut dropped: Vec<(String, KnownAs)> = drop_former_owners(&ctx)
            .await?
            .into_iter()
            .map(|did| (did, KnownAs::Owner))
            .collect();

        let now = now_ms();
        let stale = ctx
            .sql()
            .query(
                "SELECT did FROM people
                 WHERE known_as <> '[]' AND (status_checked_at IS NULL OR status_checked_at <= ?)
                 ORDER BY COALESCE(status_checked_at, 0) LIMIT ?",
                Some(vec![
                    param_integer(now - REFRESH_AFTER_MS),
                    param_integer(REFRESH_BATCH),
                ]),
            )
            .await?;
        let stale: Vec<String> = stale
            .rows
            .iter()
            .map(|row| text(row, 0))
            .collect::<Result<_>>()?;
        let mut refreshed = Vec::new();
        for batch in stale.chunks(REFRESH_CONCURRENCY) {
            let mut tasks = tokio::task::JoinSet::new();
            for did in batch {
                tasks.spawn(Arc::clone(&self).refresh_quietly(ctx.clone(), did.clone()));
            }
            while let Some(done) = tasks.join_next().await {
                if let Ok(Some(did)) = done {
                    refreshed.push(did);
                }
            }
        }
        refreshed.sort();

        let now = now_ms();
        let signers = ctx
            .sql()
            .execute(
                "UPDATE people SET known_as = '[]'
                 WHERE known_as = '[\"signer\"]' AND COALESCE(last_signed_at, first_seen_at) <= ?
                 RETURNING did",
                Some(vec![param_integer(now - DROP_AFTER_MS)]),
            )
            .await?;
        for row in &signers.rows {
            dropped.push((text(row, 0)?, KnownAs::Signer));
        }
        for did in idle_signed_in(&ctx, now).await? {
            ctx.sql()
                .execute(
                    "UPDATE people SET known_as = '[]' WHERE did = ? AND known_as = '[\"signed_in\"]'",
                    Some(vec![param_text(did.as_str())]),
                )
                .await?;
            dropped.push((did, KnownAs::SignedIn));
        }
        Ok(SweepReport {
            expired,
            refreshed,
            dropped,
        })
    }
}

/// Fetches what a refresh learns: the verified identity and the account status.
async fn learn(resolver: VerifyingResolver, did: Did) -> Learned {
    let resolved = match resolver.resolve_did(&did).await {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::info!(%did, %error, "could not resolve a DID; keeping its status");
            return Learned::default();
        }
    };
    let Some(identity) = resolved.identity else {
        // The last valid PLC operation is a tombstone.
        return Learned {
            identity: None,
            status: Some("deleted".to_owned()),
            handle: None,
        };
    };
    let status_check = async {
        match &identity.pds {
            Some(pds) => repo_status(resolver.fetch(), pds, did.as_str()).await,
            None => None,
        }
    };
    let (status, verdict) = tokio::join!(status_check, resolver.verify_handle(&identity));
    let handle = match verdict {
        HandleVerdict::Verified(handle) => Some(handle.as_str().to_owned()),
        HandleVerdict::Invalid(reason) => {
            tracing::info!(%did, %reason, "a handle no longer verifies");
            Some(INVALID_HANDLE.to_owned())
        }
        HandleVerdict::Unavailable(reason) => {
            tracing::info!(%did, %reason, "could not check a handle; keeping the last one");
            None
        }
    };
    Learned {
        identity: Some(identity),
        status,
        handle,
    }
}

/// The account status `pds` reports for `did` through `com.atproto.sync.getRepoStatus`, or
/// `None` when the answer is a transport failure or malformed.
async fn repo_status(fetch: &FetchClient, pds: &str, did: &str) -> Option<String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{pds}/xrpc/com.atproto.sync.getRepoStatus"),
        &[("did", did)],
    )
    .ok()?;
    let fetched = match fetch.get(url.as_str(), REPO_STATUS_CAP).await {
        Ok(fetched) => fetched,
        Err(error) => {
            tracing::info!(%did, %pds, %error, "getRepoStatus failed; keeping the status");
            return None;
        }
    };
    let body: serde_json::Value = serde_json::from_slice(&fetched.body).ok()?;
    match fetched.status.as_u16() {
        200 => match body.get("active")?.as_bool()? {
            true => Some("active".to_owned()),
            false => Some(
                body.get("status")
                    .and_then(serde_json::Value::as_str)
                    .filter(|status| is_status_word(status))
                    .unwrap_or("inactive")
                    .to_owned(),
            ),
        },
        400 if body.get("error").and_then(serde_json::Value::as_str) == Some("RepoNotFound") => {
            Some("deleted".to_owned())
        }
        _ => None,
    }
}

/// Whether `status` looks like an account status word, so a hostile PDS cannot store
/// arbitrary text; any other value is stored as `inactive`.
fn is_status_word(status: &str) -> bool {
    (1..=64).contains(&status.len())
        && status
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Deletes expired pending sign-ins, outcome records, CLI codes and sessions, and answers how
/// many rows went. A session's DID is first marked as holding a live credential until now.
pub(super) async fn delete_expired(ctx: &Ctx<Identity>) -> Result<u64> {
    let now = now_ms();
    let idle_before = now - super::sessions::SESSION_IDLE_MS;
    ctx.sql()
        .transaction(
            move |tx| async move {
                tx.execute(
                    "UPDATE people SET last_credential_at = ? WHERE did IN
                     (SELECT did FROM sessions WHERE expires_at <= ? OR last_used_at <= ?)",
                    Some(vec![
                        param_integer(now),
                        param_integer(now),
                        param_integer(idle_before),
                    ]),
                )
                .await?;
                let mut deleted = tx
                    .execute(
                        "DELETE FROM sessions WHERE expires_at <= ? OR last_used_at <= ?",
                        Some(vec![param_integer(now), param_integer(idle_before)]),
                    )
                    .await?
                    .changes;
                for table in ["oauth_requests", "auth_outcomes", "cli_codes"] {
                    deleted += tx
                        .execute(
                            &format!("DELETE FROM {table} WHERE expires_at <= ?"),
                            Some(vec![param_integer(now)]),
                        )
                        .await?
                        .changes;
                }
                Ok(u64::try_from(deleted).unwrap_or_default())
            },
            SqliteTransactionOptions {
                name: Some("delete_expired"),
                timeout: None,
            },
        )
        .await
}

/// Drops the `owner` tag from DIDs no longer in `owners`, and answers them.
pub(super) async fn drop_former_owners(ctx: &Ctx<Identity>) -> Result<Vec<String>> {
    let services = services()?;
    let tagged = ctx
        .sql()
        .query(
            "SELECT did, known_as FROM people WHERE known_as LIKE '%\"owner\"%'",
            None,
        )
        .await?;
    let mut dropped = Vec::new();
    for row in &tagged.rows {
        let did = text(row, 0)?;
        if services.is_owner(&did) {
            continue;
        }
        let known_as: Vec<KnownAs> = decode_known_as(&text(row, 1)?)
            .into_iter()
            .filter(|reason| *reason != KnownAs::Owner)
            .collect();
        ctx.sql()
            .execute(
                "UPDATE people SET known_as = ? WHERE did = ?",
                Some(vec![
                    param_text(encode_known_as(&known_as)),
                    param_text(did.as_str()),
                ]),
            )
            .await?;
        dropped.push(did);
    }
    Ok(dropped)
}

/// Non-owner DIDs known only as signed in that have had no live session or token for 30 days.
async fn idle_signed_in(ctx: &Ctx<Identity>, now: i64) -> Result<Vec<String>> {
    let services = services()?;
    let candidates = ctx
        .sql()
        .query(
            "SELECT did, COALESCE(last_credential_at, first_seen_at) FROM people
             WHERE known_as = '[\"signed_in\"]'",
            None,
        )
        .await?;
    let mut idle = Vec::new();
    for row in &candidates.rows {
        let did = text(row, 0)?;
        if services.is_owner(&did) {
            continue;
        }
        let mut last_live = integer(row, 1)?;
        let sessions = ctx
            .sql()
            .query(
                "SELECT COUNT(*) FROM sessions WHERE did = ?",
                Some(vec![param_text(did.as_str())]),
            )
            .await?;
        // Expired sessions were deleted above, so any remaining row is live.
        if sessions.rows.first().map_or(Ok(0), |row| integer(row, 0))? > 0 {
            continue;
        }
        let tokens = ctx
            .sql()
            .query(
                "SELECT MAX(MIN(expires_at, COALESCE(revoked_at, expires_at))) FROM tokens
                 WHERE did = ?",
                Some(vec![param_text(did.as_str())]),
            )
            .await?;
        if let Some(token_live_until) = tokens
            .rows
            .first()
            .map(|row| optional_integer(row, 0))
            .transpose()?
            .flatten()
        {
            last_live = last_live.max(token_live_until);
        }
        if last_live <= now - DROP_AFTER_MS {
            idle.push(did);
        }
    }
    Ok(idle)
}

impl Handler<KnowDid> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: KnowDid) -> Result<bool> {
        let (known, never_checked) = self.know(&ctx, &action.did, action.because).await?;
        if known && never_checked {
            let background = ctx.clone();
            ctx.wait_until(async move {
                self.refresh_quietly(background, action.did).await;
            });
        }
        Ok(known)
    }
}

impl Handler<SignedIn> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: SignedIn) -> Result<bool> {
        // A stranger to a private forge leaves no event and is not watched (design D13).
        if !services()?.can_read(&action.did) {
            return Ok(false);
        }
        self.know(&ctx, &action.did, KnownAs::SignedIn).await?;
        let now = now_ms();
        let SignedIn { did, handle } = action;
        ctx.sql()
            .transaction(
                move |tx| async move {
                    if let Some(handle) = &handle {
                        tx.execute(
                            "UPDATE people SET handle = ?, handle_checked_at = ? WHERE did = ?",
                            Some(vec![
                                param_text(handle.as_str()),
                                param_integer(now),
                                param_text(did.as_str()),
                            ]),
                        )
                        .await?;
                    }
                    tx.execute(
                        "UPDATE people SET last_credential_at = ? WHERE did = ?",
                        Some(vec![param_integer(now), param_text(did.as_str())]),
                    )
                    .await?;
                    let data = json!({ "did": did, "handle": handle });
                    append(&tx, SOURCE, "person.signed_in", data).await?;
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("signed_in"),
                    timeout: None,
                },
            )
            .await?;
        self.flush_later(&ctx);
        Ok(true)
    }
}

impl Handler<RefreshIdentity> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: RefreshIdentity) -> Result<Refreshed> {
        self.refresh(&ctx, &action.did).await
    }
}

impl Handler<People> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: People,
    ) -> Result<Vec<Option<Person>>> {
        if action.dids.len() > PEOPLE_LIMIT {
            anyhow::bail!("at most {PEOPLE_LIMIT} DIDs per lookup");
        }
        let mut people = Vec::with_capacity(action.dids.len());
        for did in &action.dids {
            let result = ctx
                .sql()
                .query(
                    &format!("SELECT {PERSON_COLUMNS} FROM people WHERE did = ?"),
                    Some(vec![param_text(did.as_str())]),
                )
                .await?;
            people.push(
                result
                    .rows
                    .first()
                    .map(|row| person_from_row(row))
                    .transpose()?,
            );
        }
        Ok(people)
    }
}

impl Handler<Sweep> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Sweep) -> Result<SweepReport> {
        self.sweep(ctx).await
    }
}

impl Handler<WatchedDids> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: WatchedDids) -> Result<Vec<String>> {
        let rows = ctx
            .sql()
            .query(
                "SELECT did FROM people WHERE known_as <> '[]' ORDER BY did",
                None,
            )
            .await?;
        rows.rows.iter().map(|row| text(row, 0)).collect()
    }
}

/// Whether `did`'s stored account status refuses its sessions and tokens.
pub(super) async fn is_inactive(tx: &SqliteTransaction, did: &str) -> Result<bool> {
    let result = tx
        .execute(
            "SELECT status FROM people WHERE did = ?",
            Some(vec![param_text(did)]),
        )
        .await?;
    Ok(match result.rows.first() {
        Some(row) => tartan_identity_api::status_is_inactive(&text(row, 0)?),
        None => false,
    })
}

/// Records that `did` holds a live credential now.
pub(super) async fn mark_credential(tx: &SqliteTransaction, did: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE people SET last_credential_at = ? WHERE did = ?",
        Some(vec![param_integer(now), param_text(did)]),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_as_is_stored_canonically() {
        assert_eq!(
            encode_known_as(&[KnownAs::Signer, KnownAs::Owner, KnownAs::Signer]),
            "[\"owner\",\"signer\"]"
        );
        assert_eq!(encode_known_as(&[KnownAs::Signer]), "[\"signer\"]");
        assert_eq!(encode_known_as(&[KnownAs::SignedIn]), "[\"signed_in\"]");
        assert_eq!(encode_known_as(&[]), "[]");
        assert_eq!(
            decode_known_as("[\"rule_signer\",\"owner\"]"),
            vec![KnownAs::RuleSigner, KnownAs::Owner]
        );
    }

    #[test]
    fn status_words() {
        assert!(is_status_word("takendown"));
        assert!(is_status_word("some_future-status2"));
        assert!(!is_status_word(""));
        assert!(!is_status_word("Taken Down"));
        assert!(!is_status_word(&"a".repeat(65)));
    }
}
