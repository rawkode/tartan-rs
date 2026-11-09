//! Delivery of an actor's events to subscribers.
//!
//! Each source keeps, per subscriber, the last sequence number it delivered (`_tartan_deliveries`). A
//! flush sends every newer event that matches the subscriber's pattern through the subscriber's
//! `deliver` action and moves the cursor only once the subscriber has stored them, so delivery is
//! in order and at least once. A failed flush is retried by a scheduled `flush` action.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rivetkit::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::actors::extension::DeliverEvents;
use crate::actors::subscriber::Deliver;
use crate::event_log::read;
use crate::sql::{integer, param_integer, param_text};
use crate::{ListRecipients, Recipient, extension_handle, subscriber_handle};

/// The most events sent to a subscriber in one call.
const BATCH: u32 = 100;

/// How long a source waits before retrying a failed delivery.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// Delivers any events not yet delivered. Every event source handles it; the source also
/// schedules it to retry a failed delivery.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Flush {}

impl Action for Flush {
    type Output = ();

    const NAME: &'static str = "flush";
}

/// Flushes a repository and every extension installed on it: the sources a repository's
/// installation receives events from. Sent when an installation subscribes, so it receives the
/// history it matches without waiting for the next event.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FlushTree {}

impl Action for FlushTree {
    type Output = ();

    const NAME: &'static str = "flushTree";
}

/// The delivery state of one event source.
#[derive(Debug)]
pub(crate) struct Outbox {
    source: String,
    // The repository the source belongs to, once known: the repository itself, or an extension
    // installed on it. Installations on that repository receive its events.
    repo: std::sync::Mutex<Option<String>>,
    // One flush at a time per source, so a subscriber sees each source's events in order.
    flushing: Mutex<()>,
}

impl Outbox {
    /// The outbox of the source named `source` (see [`crate::Envelope::source`]).
    pub(crate) fn new(source: String) -> Arc<Self> {
        Arc::new(Self {
            source,
            repo: std::sync::Mutex::new(None),
            flushing: Mutex::new(()),
        })
    }

    /// Records the repository the source belongs to.
    pub(crate) fn set_repo(&self, repo_id: String) {
        *self
            .repo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(repo_id);
    }

    fn repo(&self) -> Option<String> {
        self.repo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether events of this source go to `installation_id`, an installation on repository
    /// `repo_id`: the source belongs to that repository and is not the installation itself, so
    /// an extension that emits what it subscribes to cannot feed itself.
    fn reaches(&self, installation_id: &str, repo_id: &str) -> bool {
        self.repo().as_deref() == Some(repo_id)
            && self
                .source
                .strip_prefix("extension:")
                .is_none_or(|own| own != installation_id)
    }

    /// Flushes in the background, and schedules a retry if delivery fails.
    pub(crate) fn flush_later<A>(self: &Arc<Self>, ctx: &Ctx<A>, subscribers: SubscriberSource)
    where
        A: Actor + Handler<Flush>,
    {
        let outbox = Arc::clone(self);
        let ctx = ctx.clone();
        ctx.clone().wait_until(async move {
            if let Err(error) = outbox.flush(&ctx, subscribers).await {
                tracing::warn!(source = %outbox.source, %error, "event delivery failed; retrying");
                outbox.schedule_retry(&ctx).await;
            }
        });
    }

    /// Delivers every undelivered event to every subscriber. An error means at least one
    /// subscriber was not reached; the others were.
    pub(crate) async fn flush<A: Actor>(
        &self,
        ctx: &Ctx<A>,
        subscribers: SubscriberSource,
    ) -> Result<()> {
        let _flushing = self.flushing.lock().await;
        let recipients = match subscribers {
            SubscriberSource::Known(list) => list,
            SubscriberSource::Forge => {
                crate::forge_handle(&ctx.client()?)?
                    .send(ListRecipients {})
                    .await?
            }
        };
        let mut failure = None;
        for recipient in &recipients {
            if let Err(error) = self.deliver_to(ctx, recipient).await {
                tracing::warn!(source = %self.source, recipient = %recipient.delivery_key(),
                    %error, "delivery failed");
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn deliver_to<A: Actor>(&self, ctx: &Ctx<A>, recipient: &Recipient) -> Result<()> {
        let pattern = match recipient {
            Recipient::Store { subscriber } => &subscriber.pattern,
            Recipient::Installation {
                installation_id,
                pattern,
                repo_id,
                ..
            } => {
                if !self.reaches(installation_id, repo_id) {
                    return Ok(());
                }
                pattern
            }
        };
        let key = recipient.delivery_key();
        loop {
            let cursor = self.cursor(ctx, &key).await?;
            let events = read(ctx.sql(), &self.source, cursor, BATCH).await?;
            let Some(last) = events.last().map(|event| event.seq) else {
                return Ok(());
            };
            let matching: Vec<_> = events
                .into_iter()
                .filter(|event| pattern.matches(&event.kind))
                .collect();
            if !matching.is_empty() {
                let client = ctx.client()?;
                match recipient {
                    Recipient::Store { subscriber } => {
                        subscriber_handle(&client, subscriber.name.as_str())?
                            .send(Deliver {
                                source: self.source.clone(),
                                after: cursor,
                                through: last,
                                events: matching,
                            })
                            .await?;
                    }
                    Recipient::Installation {
                        installation_id,
                        pool,
                        ..
                    } => {
                        extension_handle(&client, installation_id, pool.clone())?
                            .send(DeliverEvents {
                                source: self.source.clone(),
                                events: matching,
                            })
                            .await?;
                    }
                }
            }
            self.set_cursor(ctx, &key, last).await?;
        }
    }

    async fn cursor<A: Actor>(&self, ctx: &Ctx<A>, subscriber: &str) -> Result<u64> {
        let result = ctx
            .sql()
            .query(
                "SELECT seq FROM _tartan_deliveries WHERE subscriber = ?",
                Some(vec![param_text(subscriber)]),
            )
            .await?;
        match result.rows.first() {
            Some(row) => Ok(u64::try_from(integer(row, 0)?)?),
            None => Ok(0),
        }
    }

    async fn set_cursor<A: Actor>(&self, ctx: &Ctx<A>, subscriber: &str, seq: u64) -> Result<()> {
        ctx.sql()
            .execute(
                "INSERT INTO _tartan_deliveries (subscriber, seq) VALUES (?, ?)
                 ON CONFLICT (subscriber) DO UPDATE SET seq = MAX(seq, excluded.seq)",
                Some(vec![
                    param_text(subscriber),
                    param_integer(i64::try_from(seq)?),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn schedule_retry<A: Actor>(&self, ctx: &Ctx<A>) {
        let scheduled = match rivetkit::action::encode_positional(&Flush {}) {
            Ok(args) => ctx.schedule().after(RETRY_AFTER, Flush::NAME, &args).await,
            Err(error) => Err(error),
        };
        if let Err(error) = scheduled {
            tracing::error!(source = %self.source, %error, "could not schedule a delivery retry");
        }
    }
}

/// Where a flush finds the subscribers.
#[derive(Clone, Debug)]
pub(crate) enum SubscriberSource {
    /// Ask the forge actor.
    Forge,
    /// This list; the forge actor passes its own.
    Known(Vec<Recipient>),
}

#[cfg(test)]
mod tests {
    use super::Outbox;

    #[test]
    fn forge_wide_sources_never_reach_an_installation() {
        // The identity actor never belongs to a repository, so no installation on any
        // repository receives its events.
        let identity = Outbox::new("identity".to_owned());
        assert!(!identity.reaches("installation-1", "repo-1"));
    }

    #[test]
    fn a_repository_reaches_its_own_installations_only() {
        let repo = Outbox::new("repo:repo-1".to_owned());
        repo.set_repo("repo-1".to_owned());
        assert!(repo.reaches("installation-1", "repo-1"));
        assert!(!repo.reaches("installation-2", "repo-2"));
    }
}
