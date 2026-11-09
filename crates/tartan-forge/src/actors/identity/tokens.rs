//! Access tokens (design D5): created from a session or by the operator for an owner, stored
//! by the SHA-256 of their secret, revocable by their DID or any owner.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use rivetkit::prelude::*;
use rivetkit::{ColumnValue, SqliteTransaction};
use serde_json::json;
use tartan_identity_api::{
    AdminCreateToken, AdminListTokens, AdminRevokeToken, CreateToken, ListTokens, MAX_TOKEN_DAYS,
    MAX_TOKEN_NAME_CHARS, MAX_TOKENS_PER_DID, NewToken, RevokeToken, TOKEN_PREFIX, TokenInfo,
    TokenRefusal, TokenRequest, TokenScope,
};

use super::people::mark_credential;
use super::secrets::{random_secret, sha256};
use super::sessions::live_session;
use super::{DAY_MS, Identity, SOURCE, is_display_text, parse_did};
use crate::Slug;
use crate::event_log::append;
use crate::services::services;
use crate::sql::{
    integer, now_ms, optional_integer, optional_text, param_blob, param_integer,
    param_optional_text, param_text, text,
};

/// The most repository entries one token may list.
const MAX_REPOS: usize = 100;

/// Who revoked a token through the operator path.
const OPERATOR: &str = "operator";

/// A token request after validation.
struct Validated {
    name: String,
    scopes: Vec<TokenScope>,
    repos: Option<Vec<String>>,
    lifetime_ms: i64,
}

fn invalid(message: impl Into<String>) -> TokenRefusal {
    TokenRefusal::Invalid {
        message: message.into(),
    }
}

/// Checks a token request: a printable name, a non-empty set of distinct scopes, distinct
/// `org/repo` or `org/*` entries, and an expiry of 1 to 365 days.
fn validate(request: TokenRequest) -> Result<Validated, TokenRefusal> {
    let name = request.name.trim().to_owned();
    if !is_display_text(&name, MAX_TOKEN_NAME_CHARS) {
        return Err(invalid(format!(
            "a token name is 1 to {MAX_TOKEN_NAME_CHARS} printable characters"
        )));
    }
    if request.scopes.is_empty() {
        return Err(invalid("a token needs at least one scope"));
    }
    let scopes: BTreeSet<TokenScope> = request.scopes.iter().copied().collect();
    if scopes.len() != request.scopes.len() {
        return Err(invalid("a scope is listed twice"));
    }
    let repos = match request.repos {
        None => None,
        Some(repos) => {
            if repos.is_empty() || repos.len() > MAX_REPOS {
                return Err(invalid(format!(
                    "repos lists 1 to {MAX_REPOS} entries, or is left out"
                )));
            }
            let mut seen = BTreeSet::new();
            for entry in &repos {
                if !valid_repo_entry(entry) {
                    return Err(invalid(format!(
                        "{entry:?} is not org/repo or org/* with valid slugs"
                    )));
                }
                if !seen.insert(entry.as_str()) {
                    return Err(invalid(format!("{entry:?} is listed twice")));
                }
            }
            Some(repos)
        }
    };
    if !(1..=MAX_TOKEN_DAYS).contains(&request.expires_in_days) {
        return Err(invalid(format!(
            "expires_in_days is 1 to {MAX_TOKEN_DAYS}, not {}",
            request.expires_in_days
        )));
    }
    Ok(Validated {
        name,
        scopes: scopes.into_iter().collect(),
        repos,
        lifetime_ms: i64::from(request.expires_in_days) * DAY_MS,
    })
}

/// Whether `entry` is `org/repo` or `org/*`.
fn valid_repo_entry(entry: &str) -> bool {
    let Some((org, repo)) = entry.split_once('/') else {
        return false;
    };
    org.parse::<Slug>().is_ok() && (repo == "*" || repo.parse::<Slug>().is_ok())
}

const TOKEN_COLUMNS: &str = "id, did, name, scopes, repos, created_at, expires_at, last_used_at, \
     revoked_at, revoked_by";

fn token_from_row(row: &[ColumnValue]) -> Result<TokenInfo> {
    Ok(TokenInfo {
        id: text(row, 0)?,
        did: text(row, 1)?,
        name: text(row, 2)?,
        scopes: serde_json::from_str(&text(row, 3)?)?,
        repos: optional_text(row, 4)?
            .map(|repos| serde_json::from_str(&repos))
            .transpose()?,
        created_at: integer(row, 5)?,
        expires_at: integer(row, 6)?,
        last_used_at: optional_integer(row, 7)?,
        revoked_at: optional_integer(row, 8)?,
        revoked_by: optional_text(row, 9)?,
    })
}

/// Creates a token for `did` in `tx`, within the per-DID cap, recording `token.created`
/// without the secret.
async fn insert_token(
    tx: &SqliteTransaction,
    did: &str,
    request: Validated,
    now: i64,
) -> Result<Result<NewToken, TokenRefusal>> {
    let live = tx
        .execute(
            "SELECT COUNT(*) FROM tokens WHERE did = ? AND revoked_at IS NULL AND expires_at > ?",
            Some(vec![param_text(did), param_integer(now)]),
        )
        .await?;
    let count = live.rows.first().map_or(Ok(0), |row| integer(row, 0))?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_TOKENS_PER_DID {
        return Ok(Err(TokenRefusal::TooMany));
    }
    let secret = format!("{TOKEN_PREFIX}{}", random_secret()?);
    let token = TokenInfo {
        id: ulid::Ulid::new().to_string(),
        did: did.to_owned(),
        name: request.name,
        scopes: request.scopes,
        repos: request.repos,
        created_at: now,
        expires_at: now + request.lifetime_ms,
        last_used_at: None,
        revoked_at: None,
        revoked_by: None,
    };
    tx.execute(
        "INSERT INTO tokens (id, secret_sha256, did, name, scopes, repos, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        Some(vec![
            param_text(token.id.as_str()),
            param_blob(sha256(&secret)),
            param_text(did),
            param_text(token.name.as_str()),
            param_text(serde_json::to_string(&token.scopes)?),
            param_optional_text(
                token
                    .repos
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            ),
            param_integer(token.created_at),
            param_integer(token.expires_at),
        ]),
    )
    .await?;
    mark_credential(tx, did, now).await?;
    let data = json!({
        "id": token.id,
        "did": token.did,
        "name": token.name,
        "scopes": token.scopes,
        "repos": token.repos,
        "expires_at": token.expires_at,
    });
    append(tx, SOURCE, "token.created", data).await?;
    Ok(Ok(NewToken { token, secret }))
}

/// Revokes token `id` in `tx` when `allowed` accepts its DID, recording `token.revoked`.
/// Revoking a revoked token changes nothing.
async fn revoke_token(
    tx: &SqliteTransaction,
    id: &str,
    by: &str,
    allowed: impl Fn(&str) -> bool,
    now: i64,
) -> Result<Result<bool, TokenRefusal>> {
    let result = tx
        .execute(
            "SELECT did, revoked_at FROM tokens WHERE id = ?",
            Some(vec![param_text(id)]),
        )
        .await?;
    let Some(row) = result.rows.first() else {
        return Ok(Err(TokenRefusal::NotFound));
    };
    let did = text(row, 0)?;
    // Someone who may not revoke the token learns nothing about it either.
    if !allowed(&did) {
        return Ok(Err(TokenRefusal::NotFound));
    }
    if optional_integer(row, 1)?.is_some() {
        return Ok(Ok(false));
    }
    tx.execute(
        "UPDATE tokens SET revoked_at = ?, revoked_by = ? WHERE id = ?",
        Some(vec![param_integer(now), param_text(by), param_text(id)]),
    )
    .await?;
    mark_credential(tx, &did, now).await?;
    append(
        tx,
        SOURCE,
        "token.revoked",
        json!({ "id": id, "did": did, "by": by }),
    )
    .await?;
    Ok(Ok(true))
}

impl Identity {
    async fn list_tokens(ctx: &Ctx<Self>, did: &str) -> Result<Vec<TokenInfo>> {
        let result = ctx
            .sql()
            .query(
                &format!(
                    "SELECT {TOKEN_COLUMNS} FROM tokens WHERE did = ?
                     ORDER BY created_at DESC, id DESC"
                ),
                Some(vec![param_text(did)]),
            )
            .await?;
        result.rows.iter().map(|row| token_from_row(row)).collect()
    }

    /// Creates a token for `did`, or for the person of session `session_id` when `did` is
    /// `None`.
    async fn create_token(
        &self,
        ctx: &Ctx<Self>,
        owner: Option<String>,
        session_id: Option<String>,
        request: TokenRequest,
    ) -> Result<Result<NewToken, TokenRefusal>> {
        let request = match validate(request) {
            Ok(request) => request,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let now = now_ms();
        let created = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    let did = match (owner, session_id) {
                        (Some(did), _) => did,
                        (None, Some(session)) => match live_session(&tx, &session, now).await? {
                            Some((did, _)) => did,
                            None => return Ok(Err(TokenRefusal::NoSession)),
                        },
                        (None, None) => return Ok(Err(TokenRefusal::NoSession)),
                    };
                    insert_token(&tx, &did, request, now).await
                },
                SqliteTransactionOptions {
                    name: Some("create_token"),
                    timeout: None,
                },
            )
            .await?;
        if created.is_ok() {
            self.flush_later(ctx);
        }
        Ok(created)
    }

    async fn revoke(
        &self,
        ctx: &Ctx<Self>,
        id: String,
        by: String,
        allowed: impl Fn(&str) -> bool + Send + 'static,
    ) -> Result<Result<(), TokenRefusal>> {
        let now = now_ms();
        let revoked = ctx
            .sql()
            .transaction(
                move |tx| async move { revoke_token(&tx, &id, &by, allowed, now).await },
                SqliteTransactionOptions {
                    name: Some("revoke_token"),
                    timeout: None,
                },
            )
            .await?;
        if revoked == Ok(true) {
            self.flush_later(ctx);
        }
        Ok(revoked.map(|_| ()))
    }
}

/// The operator path's check: `did` must be one of the forge's owners.
fn require_owner(did: &str) -> Result<Result<(), TokenRefusal>> {
    if services()?.is_owner(did) {
        Ok(Ok(()))
    } else {
        Ok(Err(TokenRefusal::NotOwner {
            did: did.to_owned(),
        }))
    }
}

impl Handler<CreateToken> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: CreateToken,
    ) -> Result<Result<NewToken, TokenRefusal>> {
        self.create_token(&ctx, None, Some(action.session_id), action.request)
            .await
    }
}

impl Handler<ListTokens> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ListTokens) -> Result<Vec<TokenInfo>> {
        Self::list_tokens(&ctx, &action.did).await
    }
}

impl Handler<RevokeToken> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: RevokeToken,
    ) -> Result<Result<(), TokenRefusal>> {
        let by = action.by.clone();
        let by_owner = services()?.is_owner(&by);
        self.revoke(&ctx, action.id, action.by, move |did| by_owner || did == by)
            .await
    }
}

impl Handler<AdminCreateToken> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: AdminCreateToken,
    ) -> Result<Result<NewToken, TokenRefusal>> {
        parse_did(&action.did)?;
        if let Err(refusal) = require_owner(&action.did)? {
            return Ok(Err(refusal));
        }
        self.create_token(&ctx, Some(action.did), None, action.request)
            .await
    }
}

impl Handler<AdminListTokens> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: AdminListTokens,
    ) -> Result<Result<Vec<TokenInfo>, TokenRefusal>> {
        if let Err(refusal) = require_owner(&action.did)? {
            return Ok(Err(refusal));
        }
        Ok(Ok(Self::list_tokens(&ctx, &action.did).await?))
    }
}

impl Handler<AdminRevokeToken> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: AdminRevokeToken,
    ) -> Result<Result<(), TokenRefusal>> {
        if let Err(refusal) = require_owner(&action.did)? {
            return Ok(Err(refusal));
        }
        let owner = action.did;
        self.revoke(&ctx, action.id, OPERATOR.to_owned(), move |did| {
            did == owner
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> TokenRequest {
        TokenRequest {
            name: "ci".into(),
            scopes: vec![TokenScope::Read],
            repos: None,
            expires_in_days: 30,
        }
    }

    fn refused(request: TokenRequest) -> bool {
        matches!(validate(request), Err(TokenRefusal::Invalid { .. }))
    }

    #[test]
    fn accepts_a_plain_request() {
        let validated = validate(request()).expect("valid");
        assert_eq!(validated.scopes, vec![TokenScope::Read]);
        assert_eq!(validated.lifetime_ms, 30 * DAY_MS);
    }

    #[test]
    fn refuses_malformed_requests() {
        assert!(refused(TokenRequest {
            expires_in_days: 400,
            ..request()
        }));
        assert!(refused(TokenRequest {
            expires_in_days: 0,
            ..request()
        }));
        assert!(refused(TokenRequest {
            scopes: vec![],
            ..request()
        }));
        assert!(refused(TokenRequest {
            scopes: vec![TokenScope::Read, TokenScope::Read],
            ..request()
        }));
        assert!(refused(TokenRequest {
            name: " ".into(),
            ..request()
        }));
        for repos in [
            vec![],
            vec!["acme".to_owned()],
            vec!["acme/".to_owned()],
            vec!["Acme/api".to_owned()],
            vec!["acme/api/x".to_owned()],
            vec!["*/api".to_owned()],
            vec!["acme/api".to_owned(), "acme/api".to_owned()],
        ] {
            assert!(
                refused(TokenRequest {
                    repos: Some(repos.clone()),
                    ..request()
                }),
                "{repos:?}"
            );
        }
    }

    #[test]
    fn repository_entries() {
        assert!(valid_repo_entry("acme/api"));
        assert!(valid_repo_entry("acme/*"));
        assert!(!valid_repo_entry("acme"));
        assert!(!valid_repo_entry("acme/a*"));
    }
}
