//! The identity actor: one per forge (design D3). It keeps people (the DIDs the forge watches,
//! their verified PDS, key and account status), browser and CLI sessions, access tokens, the
//! one-time codes of the `tt login` hand-off, and the bookkeeping of sign-in and signature
//! records. It logs to the event source `identity`.
//!
//! Like the forge actor it always runs in the default pool: placements name only org and
//! repository targets, so it is never moved.
//!
//! Secrets never reach its database or its events: sessions, tokens and CLI codes are stored by
//! the SHA-256 of their secret, and an action that creates one answers with the secret once.

mod auth;
mod cli;
mod people;
mod secrets;
mod sessions;
mod tokens;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{CronEveryOptions, Handler, action};
use tartan_identity_api::{
    AdminCreateToken, AdminListTokens, AdminRevokeToken, Authenticate, BeginAuth, CompleteAuth,
    CreateSession, CreateToken, EndSession, IssueCliCode, KnowDid, KnownAs, ListSessions,
    ListTokens, People, PutOutcome, RedeemCliCode, RefreshIdentity, RevokeToken, SWEEP_SCHEDULE,
    Schedules, SignInCapacity, SignedIn, Sweep, TakeOutcome,
};

use crate::Envelope;
use crate::actors::apply_schema;
use crate::event_log::{EVENT_LOG_SCHEMA, ReadEvents, read};
use crate::outbox::{Flush, Outbox, SubscriberSource};

/// The source name of the identity actor's events.
pub(crate) const SOURCE: &str = "identity";

/// How often Rivet runs [`Sweep`].
const SWEEP_EVERY: Duration = Duration::from_secs(15 * 60);

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The schema of design D3. `people.last_credential_at` is when the DID last held a live
/// session or token, so the sweep can tell how long a signed-in DID has had none.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS people (
        did TEXT PRIMARY KEY,
        handle TEXT,
        handle_checked_at INTEGER,
        pds TEXT,
        atproto_key TEXT,
        status TEXT NOT NULL DEFAULT 'active',
        status_checked_at INTEGER,
        known_as TEXT NOT NULL,
        first_seen_at INTEGER NOT NULL,
        last_signed_at INTEGER,
        last_credential_at INTEGER
    );
    CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        secret_sha256 BLOB NOT NULL UNIQUE,
        did TEXT NOT NULL,
        kind TEXT NOT NULL,
        device TEXT,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        last_used_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS sessions_did ON sessions (did);
    CREATE TABLE IF NOT EXISTS tokens (
        id TEXT PRIMARY KEY,
        secret_sha256 BLOB NOT NULL UNIQUE,
        did TEXT NOT NULL,
        name TEXT NOT NULL,
        scopes TEXT NOT NULL,
        repos TEXT,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        last_used_at INTEGER,
        revoked_at INTEGER,
        revoked_by TEXT
    );
    CREATE INDEX IF NOT EXISTS tokens_did ON tokens (did);
    CREATE TABLE IF NOT EXISTS oauth_requests (
        state TEXT PRIMARY KEY,
        did TEXT NOT NULL,
        issuer TEXT NOT NULL,
        pds TEXT NOT NULL,
        auth_request TEXT NOT NULL,
        return_to TEXT NOT NULL,
        client_ip TEXT NOT NULL,
        browser_sha256 BLOB NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS auth_outcomes (
        id TEXT PRIMARY KEY,
        code TEXT NOT NULL,
        details TEXT NOT NULL,
        browser_sha256 BLOB NOT NULL,
        client_ip TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS cli_codes (
        code_sha256 BLOB PRIMARY KEY,
        did TEXT NOT NULL,
        challenge TEXT NOT NULL,
        device TEXT NOT NULL,
        client_ip TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS signature_records (
        did TEXT NOT NULL,
        rkey TEXT NOT NULL,
        cid TEXT NOT NULL,
        rev TEXT NOT NULL,
        first_proved_at INTEGER NOT NULL,
        revoked_at INTEGER,
        revoked_reason TEXT,
        pre_withdrawn INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (did, rkey)
    );
    CREATE TABLE IF NOT EXISTS jetstream (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        cursor_us INTEGER
    );
";

/// The identity actor.
#[derive(Debug)]
pub struct Identity {
    outbox: Arc<Outbox>,
}

#[async_trait]
impl Actor for Identity {
    type Input = ();
    type Actions = (
        KnowDid,
        SignedIn,
        RefreshIdentity,
        People,
        CreateSession,
        Authenticate,
        ListSessions,
        EndSession,
        CreateToken,
        ListTokens,
        RevokeToken,
        AdminCreateToken,
        AdminListTokens,
        AdminRevokeToken,
        IssueCliCode,
        RedeemCliCode,
        SignInCapacity,
        BeginAuth,
        CompleteAuth,
        PutOutcome,
        TakeOutcome,
        Sweep,
        Schedules,
        ReadEvents,
        Flush,
    );
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), EVENT_LOG_SCHEMA).await?;
        apply_schema(ctx.sql(), SCHEMA).await?;
        Ok(Self {
            outbox: Outbox::new(SOURCE.to_owned()),
        })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        // Re-registering the same name replaces the entry, so restarts never pile up sweeps.
        let args = rivetkit::action::encode_positional(&Sweep {})?;
        ctx.cron()
            .every(CronEveryOptions {
                name: SWEEP_SCHEDULE,
                interval: SWEEP_EVERY,
                action: <Sweep as Action>::NAME,
                args: &args,
                max_history: None,
            })
            .await?;
        people::delete_expired(&ctx).await?;
        people::drop_former_owners(&ctx).await?;
        // Owners are watched from the start; resolving them waits on the network, so it runs in
        // the background and never delays the actor (design D13).
        let owners = crate::services::services()?.owners.clone();
        let this = Arc::clone(&self);
        let background = ctx.clone();
        ctx.wait_until(async move {
            for owner in owners {
                match this.know(&background, &owner, KnownAs::Owner).await {
                    Ok((true, true)) => {
                        Arc::clone(&this)
                            .refresh_quietly(background.clone(), owner)
                            .await;
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(did = %owner, %error, "could not record an owner"),
                }
            }
        });
        self.outbox.flush_later(&ctx, SubscriberSource::Forge);
        Ok(())
    }
}

impl Identity {
    /// Delivers new events to the forge's subscribers in the background.
    fn flush_later(&self, ctx: &Ctx<Self>) {
        self.outbox.flush_later(ctx, SubscriberSource::Forge);
    }
}

impl Handler<ReadEvents> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: ReadEvents) -> Result<Vec<Envelope>> {
        read(ctx.sql(), SOURCE, action.after, action.limit).await
    }
}

impl Handler<Flush> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Flush) -> Result<()> {
        self.outbox.flush(&ctx, SubscriberSource::Forge).await
    }
}

impl Handler<Schedules> for Identity {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: Schedules) -> Result<Vec<String>> {
        Ok(ctx
            .cron()
            .list()
            .await?
            .into_iter()
            .map(|job| job.name)
            .collect())
    }
}

/// Parses `did` with the syntax the forge's mode accepts.
fn parse_did(did: &str) -> Result<tartan_atproto::did::Did> {
    let mode = crate::services::services()?.resolver.mode();
    tartan_atproto::did::Did::parse(did, mode)
        .map_err(|error| anyhow::anyhow!("{did:?} is not a DID this forge accepts: {error}"))
}

/// Whether `text` is non-empty after trimming, at most `max` characters, and free of control
/// characters.
fn is_display_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::is_display_text;

    #[test]
    fn display_text() {
        assert!(is_display_text("laptop", 64));
        assert!(!is_display_text("  ", 64));
        assert!(!is_display_text("a\nb", 64));
        assert!(!is_display_text(&"x".repeat(65), 64));
        assert!(is_display_text(&"é".repeat(64), 64));
    }
}
