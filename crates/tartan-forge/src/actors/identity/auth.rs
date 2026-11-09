//! Sign-in bookkeeping (design D1): pending OAuth requests, bound to the browser that started
//! them, and the details of failed sign-ins, readable once by that browser.
//!
//! Both are secret-bearing bookkeeping: they record no events, expire after 10 minutes and are
//! deleted by the sweep. A pending request holds jacquard's serialized `AuthRequestData`, whose
//! DPoP private key is the only private key the forge stores, and only until the callback.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rivetkit::SqliteTransaction;
use rivetkit::prelude::*;
use serde::{Deserialize, Serialize};
use tartan_identity_api::{
    BeginAuth, CompleteAuth, MAX_OUTCOMES, MAX_OUTCOMES_PER_ADDRESS, MAX_PENDING_SIGN_INS,
    MAX_PENDING_SIGN_INS_PER_ADDRESS, PendingSignIn, PutOutcome, SIGN_IN_LIFETIME_MS,
    SignInCapacity, SignInRefusal, TakeOutcome,
};

use super::Identity;
use super::secrets::digest_from_hex;
use crate::sql::{integer, now_ms, optional_text, param_blob, param_integer, param_text, text};

/// The longest text kept in an outcome's details, in characters.
const MAX_DETAIL_CHARS: usize = 300;
/// The longest host kept in an outcome's details, in characters.
const MAX_HOST_CHARS: usize = 253;

/// What the `auth_request` column holds: jacquard's request and the verified handle typed.
#[derive(Serialize, Deserialize)]
struct StoredRequest {
    request: String,
    handle: Option<String>,
}

fn count(rows: &[Vec<rivetkit::ColumnValue>]) -> Result<usize> {
    let row = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("COUNT answered no row"))?;
    Ok(usize::try_from(integer(row, 0)?).unwrap_or(usize::MAX))
}

/// Whether `client_ip` may have another pending sign-in.
async fn capacity(
    tx: &SqliteTransaction,
    client_ip: &str,
    now: i64,
) -> Result<Result<(), SignInRefusal>> {
    let all = tx
        .execute(
            "SELECT COUNT(*) FROM oauth_requests WHERE expires_at > ?",
            Some(vec![param_integer(now)]),
        )
        .await?;
    if count(&all.rows)? >= MAX_PENDING_SIGN_INS {
        return Ok(Err(SignInRefusal::Busy));
    }
    let mine = tx
        .execute(
            "SELECT COUNT(*) FROM oauth_requests WHERE client_ip = ? AND expires_at > ?",
            Some(vec![param_text(client_ip), param_integer(now)]),
        )
        .await?;
    if count(&mine.rows)? >= MAX_PENDING_SIGN_INS_PER_ADDRESS {
        return Ok(Err(SignInRefusal::RateLimited));
    }
    Ok(Ok(()))
}

/// Cuts `text` to `max` characters and drops control characters.
fn clip(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

impl Handler<SignInCapacity> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: SignInCapacity,
    ) -> Result<Result<(), SignInRefusal>> {
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move { capacity(&tx, &action.client_ip, now).await },
                SqliteTransactionOptions {
                    name: Some("sign_in_capacity"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<BeginAuth> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: BeginAuth,
    ) -> Result<Result<(), SignInRefusal>> {
        let pending = action.pending;
        super::parse_did(&pending.did)?;
        let browser = digest_from_hex(&pending.browser_sha256)
            .ok_or_else(|| anyhow::anyhow!("browser_sha256 is not a SHA-256"))?;
        let stored = serde_json::to_string(&StoredRequest {
            request: pending.auth_request,
            handle: pending.handle,
        })?;
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    if let Err(refusal) = capacity(&tx, &pending.client_ip, now).await? {
                        return Ok(Err(refusal));
                    }
                    tx.execute(
                        "INSERT INTO oauth_requests (state, did, issuer, pds, auth_request,
                         return_to, client_ip, browser_sha256, created_at, expires_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        Some(vec![
                            param_text(pending.state),
                            param_text(pending.did),
                            param_text(pending.issuer),
                            param_text(pending.pds),
                            param_text(stored),
                            param_text(pending.return_to),
                            param_text(pending.client_ip),
                            param_blob(browser),
                            param_integer(now),
                            param_integer(now + SIGN_IN_LIFETIME_MS),
                        ]),
                    )
                    .await?;
                    Ok(Ok(()))
                },
                SqliteTransactionOptions {
                    name: Some("begin_auth"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<CompleteAuth> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: CompleteAuth,
    ) -> Result<Option<PendingSignIn>> {
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let result = tx
                        .execute(
                            "SELECT did, issuer, pds, auth_request, return_to, client_ip,
                             lower(hex(browser_sha256)), expires_at
                             FROM oauth_requests WHERE state = ?",
                            Some(vec![param_text(action.state.as_str())]),
                        )
                        .await?;
                    // The state is single-use, whatever the outcome.
                    tx.execute(
                        "DELETE FROM oauth_requests WHERE state = ?",
                        Some(vec![param_text(action.state.as_str())]),
                    )
                    .await?;
                    let Some(row) = result.rows.first() else {
                        return Ok(None);
                    };
                    if integer(row, 7)? <= now {
                        return Ok(None);
                    }
                    let stored: StoredRequest = serde_json::from_str(&text(row, 3)?)?;
                    Ok(Some(PendingSignIn {
                        state: action.state,
                        did: text(row, 0)?,
                        handle: stored.handle,
                        issuer: text(row, 1)?,
                        pds: text(row, 2)?,
                        auth_request: stored.request,
                        return_to: text(row, 4)?,
                        client_ip: text(row, 5)?,
                        browser_sha256: text(row, 6)?,
                    }))
                },
                SqliteTransactionOptions {
                    name: Some("complete_auth"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<PutOutcome> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: PutOutcome) -> Result<String> {
        let browser = digest_from_hex(&action.browser_sha256)
            .ok_or_else(|| anyhow::anyhow!("browser_sha256 is not a SHA-256"))?;
        let details: BTreeMap<String, String> = action
            .details
            .into_iter()
            .map(|(name, value)| {
                let max = if name == "host" {
                    MAX_HOST_CHARS
                } else {
                    MAX_DETAIL_CHARS
                };
                (name, clip(&value, max))
            })
            .collect();
        let details = serde_json::to_string(&details)?;
        let mut id = [0_u8; 16];
        getrandom::fill(&mut id)
            .map_err(|error| anyhow::anyhow!("the OS random source failed: {error}"))?;
        let id = URL_SAFE_NO_PAD.encode(id);
        let now = now_ms();
        let answer = id.clone();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    // Beyond a cap the oldest details are dropped; only their text is lost.
                    tx.execute(
                        "DELETE FROM auth_outcomes WHERE id IN (SELECT id FROM auth_outcomes
                         WHERE client_ip = ? ORDER BY created_at DESC LIMIT -1 OFFSET ?)",
                        Some(vec![
                            param_text(action.client_ip.as_str()),
                            param_integer(MAX_OUTCOMES_PER_ADDRESS as i64 - 1),
                        ]),
                    )
                    .await?;
                    tx.execute(
                        "DELETE FROM auth_outcomes WHERE id IN (SELECT id FROM auth_outcomes
                         ORDER BY created_at DESC LIMIT -1 OFFSET ?)",
                        Some(vec![param_integer(MAX_OUTCOMES as i64 - 1)]),
                    )
                    .await?;
                    tx.execute(
                        "INSERT INTO auth_outcomes
                         (id, code, details, browser_sha256, client_ip, created_at, expires_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?)",
                        Some(vec![
                            param_text(id),
                            param_text(action.code),
                            param_text(details),
                            param_blob(browser),
                            param_text(action.client_ip),
                            param_integer(now),
                            param_integer(now + SIGN_IN_LIFETIME_MS),
                        ]),
                    )
                    .await?;
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("put_outcome"),
                    timeout: None,
                },
            )
            .await?;
        Ok(answer)
    }
}

impl Handler<TakeOutcome> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: TakeOutcome,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let Some(browser) = digest_from_hex(&action.browser_sha256) else {
            return Ok(None);
        };
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let result = tx
                        .execute(
                            "SELECT details FROM auth_outcomes
                             WHERE id = ? AND code = ? AND browser_sha256 = ? AND expires_at > ?",
                            Some(vec![
                                param_text(action.id.as_str()),
                                param_text(action.code),
                                param_blob(browser),
                                param_integer(now),
                            ]),
                        )
                        .await?;
                    let Some(row) = result.rows.first() else {
                        return Ok(None);
                    };
                    let details = optional_text(row, 0)?.unwrap_or_default();
                    tx.execute(
                        "DELETE FROM auth_outcomes WHERE id = ?",
                        Some(vec![param_text(action.id)]),
                    )
                    .await?;
                    Ok(Some(serde_json::from_str(&details)?))
                },
                SqliteTransactionOptions {
                    name: Some("take_outcome"),
                    timeout: None,
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn details_are_clipped_and_lose_control_characters() {
        assert_eq!(clip("a\nb\u{7}c", 300), "abc");
        assert_eq!(clip(&"x".repeat(400), 300).len(), 300);
    }
}
