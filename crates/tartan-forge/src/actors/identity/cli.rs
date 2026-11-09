//! The one-time codes of the `tt login` hand-off (design D16).

use std::sync::Arc;

use anyhow::Result;
use rivetkit::prelude::*;
use tartan_identity_api::{
    CliRefusal, IssueCliCode, MAX_CLI_CODES_PER_DID, MAX_DEVICE_CHARS, NewSession, RedeemCliCode,
    SessionKind,
};

use super::secrets::{constant_time_eq, is_base64url, random_secret, s256, sha256};
use super::sessions::insert_session;
use super::{Identity, MINUTE_MS, is_display_text, parse_did};
use crate::sql::{integer, now_ms, param_blob, param_integer, param_text, text};

/// How long a CLI code can be redeemed.
const CODE_LIFETIME_MS: i64 = MINUTE_MS;

fn invalid(message: &str) -> CliRefusal {
    CliRefusal::Invalid {
        message: message.to_owned(),
    }
}

/// Whether `verifier` is a PKCE code verifier: 43 to 128 unreserved characters (RFC 7636).
fn is_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

impl Handler<IssueCliCode> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: IssueCliCode,
    ) -> Result<Result<String, CliRefusal>> {
        parse_did(&action.did)?;
        if action.challenge.len() != 43 || !is_base64url(&action.challenge) {
            return Ok(Err(invalid(
                "the challenge is a base64url SHA-256: 43 characters",
            )));
        }
        if !is_display_text(&action.device, MAX_DEVICE_CHARS) {
            return Ok(Err(invalid(
                "the device name is 1 to 64 printable characters",
            )));
        }
        let now = now_ms();
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let live = tx
                        .execute(
                            "SELECT COUNT(*) FROM cli_codes WHERE did = ? AND expires_at > ?",
                            Some(vec![param_text(action.did.as_str()), param_integer(now)]),
                        )
                        .await?;
                    let count = live.rows.first().map_or(Ok(0), |row| integer(row, 0))?;
                    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_CLI_CODES_PER_DID {
                        return Ok(Err(CliRefusal::TooMany));
                    }
                    let code = random_secret()?;
                    tx.execute(
                        "INSERT INTO cli_codes
                         (code_sha256, did, challenge, device, client_ip, created_at, expires_at)
                         VALUES (?, ?, ?, ?, ?, ?, ?)",
                        Some(vec![
                            param_blob(sha256(&code)),
                            param_text(action.did.as_str()),
                            param_text(action.challenge.as_str()),
                            param_text(action.device.as_str()),
                            param_text(action.client_ip.as_str()),
                            param_integer(now),
                            param_integer(now + CODE_LIFETIME_MS),
                        ]),
                    )
                    .await?;
                    Ok(Ok(code))
                },
                SqliteTransactionOptions {
                    name: Some("issue_cli_code"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<RedeemCliCode> for Identity {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: RedeemCliCode,
    ) -> Result<Result<NewSession, CliRefusal>> {
        let now = now_ms();
        // The code is deleted before anything is checked, in its own statement, so a wrong
        // verifier still consumes it.
        let taken = ctx
            .sql()
            .execute(
                "DELETE FROM cli_codes WHERE code_sha256 = ?
                 RETURNING did, challenge, device, expires_at",
                Some(vec![param_blob(sha256(&action.code))]),
            )
            .await?;
        let Some(row) = taken.rows.first() else {
            return Ok(Err(CliRefusal::Refused));
        };
        let (did, challenge, device) = (text(row, 0)?, text(row, 1)?, text(row, 2)?);
        if integer(row, 3)? <= now
            || !is_verifier(&action.verifier)
            || !constant_time_eq(s256(&action.verifier).as_bytes(), challenge.as_bytes())
        {
            return Ok(Err(CliRefusal::Refused));
        }
        let session = ctx
            .sql()
            .transaction(
                move |tx| async move {
                    insert_session(&tx, &did, SessionKind::Cli, Some(device), now).await
                },
                SqliteTransactionOptions {
                    name: Some("redeem_cli_code"),
                    timeout: None,
                },
            )
            .await?;
        Ok(Ok(session))
    }
}

#[cfg(test)]
mod tests {
    use super::is_verifier;

    #[test]
    fn verifiers() {
        assert!(is_verifier(&"a".repeat(43)));
        assert!(is_verifier(&"a-._~".repeat(20)));
        assert!(!is_verifier(&"a".repeat(42)));
        assert!(!is_verifier(&"a".repeat(129)));
        assert!(!is_verifier(&format!("{}+", "a".repeat(43))));
    }
}
