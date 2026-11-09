//! Browser and CLI sessions (design D6) and authenticating credentials (design D4).

use std::sync::Arc;

use anyhow::{Result, bail};
use rivetkit::SqliteTransaction;
use rivetkit::prelude::*;
use tartan_identity_api::{
    Authenticate, Authenticated, CLI_SESSION_PREFIX, CreateSession, Credential, CredentialKind,
    EndSession, ListSessions, MAX_DEVICE_CHARS, NewSession, SessionInfo, SessionKind, TokenScope,
};

use super::people::{is_inactive, mark_credential};
use super::secrets::{digest_from_hex, random_secret, sha256};
use super::{DAY_MS, HOUR_MS, Identity, is_display_text, parse_did};
use crate::sql::{
    integer, now_ms, optional_integer, optional_text, param_blob, param_integer,
    param_optional_text, param_text, text,
};

/// A session ends this long after it was created.
pub(super) const SESSION_LIFETIME_MS: i64 = 30 * DAY_MS;
/// A session ends after this long without use.
pub(super) const SESSION_IDLE_MS: i64 = 7 * DAY_MS;
/// `last_used_at` is written at most this often per credential.
const LAST_USED_EVERY_MS: i64 = HOUR_MS;
/// The longest browser user agent kept.
const MAX_USER_AGENT_CHARS: usize = 256;

fn kind_from_str(kind: &str) -> Option<SessionKind> {
    match kind {
        "browser" => Some(SessionKind::Browser),
        "cli" => Some(SessionKind::Cli),
        _ => None,
    }
}

/// The device text stored for a session: a CLI device name as validated, or a browser's user
/// agent with control characters removed and cut to [`MAX_USER_AGENT_CHARS`].
fn device_text(kind: SessionKind, device: Option<String>) -> Result<Option<String>> {
    let Some(device) = device else {
        return Ok(None);
    };
    match kind {
        SessionKind::Cli => {
            if !is_display_text(&device, MAX_DEVICE_CHARS) {
                bail!("a device name is 1 to {MAX_DEVICE_CHARS} printable characters");
            }
            Ok(Some(device))
        }
        SessionKind::Browser => Ok(Some(
            device
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_USER_AGENT_CHARS)
                .collect(),
        )),
    }
}

/// Inserts a session for `did` in `tx` and answers it with its secret.
pub(super) async fn insert_session(
    tx: &SqliteTransaction,
    did: &str,
    kind: SessionKind,
    device: Option<String>,
    now: i64,
) -> Result<NewSession> {
    let secret = match kind {
        SessionKind::Browser => random_secret()?,
        SessionKind::Cli => format!("{CLI_SESSION_PREFIX}{}", random_secret()?),
    };
    let id = ulid::Ulid::new().to_string();
    let expires_at = now + SESSION_LIFETIME_MS;
    tx.execute(
        "INSERT INTO sessions
         (id, secret_sha256, did, kind, device, created_at, expires_at, last_used_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        Some(vec![
            param_text(id.as_str()),
            param_blob(sha256(&secret)),
            param_text(did),
            param_text(kind.as_str()),
            param_optional_text(device),
            param_integer(now),
            param_integer(expires_at),
            param_integer(now),
        ]),
    )
    .await?;
    mark_credential(tx, did, now).await?;
    Ok(NewSession {
        id,
        did: did.to_owned(),
        secret,
        expires_at,
    })
}

/// The live session `id`, as `(did, kind)`.
pub(super) async fn live_session(
    tx: &SqliteTransaction,
    id: &str,
    now: i64,
) -> Result<Option<(String, SessionKind)>> {
    let result = tx
        .execute(
            "SELECT did, kind FROM sessions WHERE id = ? AND expires_at > ? AND last_used_at > ?",
            Some(vec![
                param_text(id),
                param_integer(now),
                param_integer(now - SESSION_IDLE_MS),
            ]),
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    let did = text(row, 0)?;
    Ok(kind_from_str(&text(row, 1)?).map(|kind| (did, kind)))
}

impl Handler<CreateSession> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: CreateSession) -> Result<NewSession> {
        parse_did(&action.did)?;
        let device = device_text(action.kind, action.device)?;
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    insert_session(&tx, &action.did, action.kind, device, now).await
                },
                SqliteTransactionOptions {
                    name: Some("create_session"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<Authenticate> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: Authenticate,
    ) -> Result<Option<Authenticated>> {
        let Some(digest) = digest_from_hex(&action.secret_sha256) else {
            return Ok(None);
        };
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    match action.kind {
                        CredentialKind::BrowserSession => {
                            authenticate_session(&tx, digest, SessionKind::Browser, now).await
                        }
                        CredentialKind::CliSession => {
                            authenticate_session(&tx, digest, SessionKind::Cli, now).await
                        }
                        CredentialKind::Token => authenticate_token(&tx, digest, now).await,
                    }
                },
                SqliteTransactionOptions {
                    name: Some("authenticate"),
                    timeout: None,
                },
            )
            .await
    }
}

async fn authenticate_session(
    tx: &SqliteTransaction,
    digest: Vec<u8>,
    expected: SessionKind,
    now: i64,
) -> Result<Option<Authenticated>> {
    let result = tx
        .execute(
            "SELECT id, did, kind, expires_at, last_used_at FROM sessions WHERE secret_sha256 = ?",
            Some(vec![param_blob(digest)]),
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    let (id, did) = (text(row, 0)?, text(row, 1)?);
    let kind = kind_from_str(&text(row, 2)?);
    let (expires_at, last_used_at) = (integer(row, 3)?, integer(row, 4)?);
    // A cookie value presented as a bearer secret, or the reverse, is refused.
    if kind != Some(expected)
        || expires_at <= now
        || last_used_at <= now - SESSION_IDLE_MS
        || is_inactive(tx, &did).await?
    {
        return Ok(None);
    }
    if now - last_used_at >= LAST_USED_EVERY_MS {
        tx.execute(
            "UPDATE sessions SET last_used_at = ? WHERE id = ?",
            Some(vec![param_integer(now), param_text(id.as_str())]),
        )
        .await?;
        mark_credential(tx, &did, now).await?;
    }
    Ok(Some(Authenticated {
        did,
        credential: Credential::Session { id, kind: expected },
    }))
}

async fn authenticate_token(
    tx: &SqliteTransaction,
    digest: Vec<u8>,
    now: i64,
) -> Result<Option<Authenticated>> {
    let result = tx
        .execute(
            "SELECT id, did, scopes, repos, expires_at, last_used_at, revoked_at FROM tokens
             WHERE secret_sha256 = ?",
            Some(vec![param_blob(digest)]),
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(None);
    };
    let (id, did) = (text(row, 0)?, text(row, 1)?);
    let scopes: Vec<TokenScope> = serde_json::from_str(&text(row, 2)?)?;
    let repos: Option<Vec<String>> = optional_text(row, 3)?
        .map(|repos| serde_json::from_str(&repos))
        .transpose()?;
    let expires_at = integer(row, 4)?;
    let last_used_at = optional_integer(row, 5)?;
    if optional_integer(row, 6)?.is_some() || expires_at <= now || is_inactive(tx, &did).await? {
        return Ok(None);
    }
    if last_used_at.is_none_or(|used| now - used >= LAST_USED_EVERY_MS) {
        tx.execute(
            "UPDATE tokens SET last_used_at = ? WHERE id = ?",
            Some(vec![param_integer(now), param_text(id.as_str())]),
        )
        .await?;
    }
    Ok(Some(Authenticated {
        did,
        credential: Credential::Token { id, scopes, repos },
    }))
}

impl Handler<ListSessions> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: ListSessions,
    ) -> Result<Vec<SessionInfo>> {
        let now = now_ms();
        let result = ctx
            .sql()
            .query(
                "SELECT id, kind, device, created_at, last_used_at, expires_at FROM sessions
                 WHERE did = ? AND expires_at > ? AND last_used_at > ?
                 ORDER BY created_at DESC, id DESC",
                Some(vec![
                    param_text(action.did.as_str()),
                    param_integer(now),
                    param_integer(now - SESSION_IDLE_MS),
                ]),
            )
            .await?;
        result
            .rows
            .iter()
            .filter_map(|row| {
                let kind = kind_from_str(&text(row, 1).ok()?)?;
                Some(Ok(SessionInfo {
                    id: text(row, 0).ok()?,
                    kind,
                    device: optional_text(row, 2).ok()?,
                    created_at: integer(row, 3).ok()?,
                    last_used_at: integer(row, 4).ok()?,
                    expires_at: integer(row, 5).ok()?,
                }))
            })
            .collect()
    }
}

impl Handler<EndSession> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: EndSession) -> Result<bool> {
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let ended = tx
                        .execute(
                            "DELETE FROM sessions WHERE id = ? AND did = ?",
                            Some(vec![
                                param_text(action.id.as_str()),
                                param_text(action.did.as_str()),
                            ]),
                        )
                        .await?
                        .changes
                        > 0;
                    if ended {
                        mark_credential(&tx, &action.did, now).await?;
                    }
                    Ok(ended)
                },
                SqliteTransactionOptions {
                    name: Some("end_session"),
                    timeout: None,
                },
            )
            .await
    }
}
