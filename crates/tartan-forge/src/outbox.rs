//! Delivery of an actor's events to subscribers.
//!
//! Each source keeps, per subscriber, the last sequence number it delivered (`deliveries`). A
//! flush sends every newer event that matches the subscriber's pattern through the subscriber's
//! `deliver` action and moves the cursor only once the subscriber has stored them, so delivery is
//! in order and at least once. A failed flush is retried by a scheduled `flush` action.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rivetkit::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::actors::subscriber::Deliver;
use crate::event_log::read;
use crate::sql::{integer, param_integer, param_text};
use crate::{SubscriberSpec, subscriber_handle};

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

/// The delivery state of one event source.
#[derive(Debug)]
pub(crate) struct Outbox {
    source: String,
    // One flush at a time per source, so a subscriber sees each source's events in order.
    flushing: Mutex<()>,
}

impl Outbox {
    /// The outbox of the source named `source` (see [`crate::Envelope::source`]).
    pub(crate) fn new(source: String) -> Arc<Self> {
        Arc::new(Self {
            source,
            flushing: Mutex::new(()),
        })
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
        let subscribers = match subscribers {
            SubscriberSource::Known(list) => list,
            SubscriberSource::Forge => {
                crate::forge_handle(&ctx.client()?)?
                    .send(crate::ListSubscribers {})
                    .await?
            }
        };
        let mut failure = None;
        for subscriber in &subscribers {
            if let Err(error) = self.deliver_to(ctx, subscriber).await {
                tracing::warn!(source = %self.source, subscriber = %subscriber.name, %error,
                    "delivery to subscriber failed");
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn deliver_to<A: Actor>(&self, ctx: &Ctx<A>, subscriber: &SubscriberSpec) -> Result<()> {
        loop {
            let cursor = self.cursor(ctx, subscriber.name.as_str()).await?;
            let events = read(ctx.sql(), &self.source, cursor, BATCH).await?;
            let Some(last) = events.last().map(|event| event.seq) else {
                return Ok(());
            };
            let matching: Vec<_> = events
                .into_iter()
                .filter(|event| subscriber.pattern.matches(&event.kind))
                .collect();
            if !matching.is_empty() {
                subscriber_handle(&ctx.client()?, subscriber.name.as_str())?
                    .send(Deliver {
                        source: self.source.clone(),
                        after: cursor,
                        through: last,
                        events: matching,
                    })
                    .await?;
            }
            self.set_cursor(ctx, subscriber.name.as_str(), last).await?;
        }
    }

    async fn cursor<A: Actor>(&self, ctx: &Ctx<A>, subscriber: &str) -> Result<u64> {
        let result = ctx
            .sql()
            .query(
                "SELECT seq FROM deliveries WHERE subscriber = ?",
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
                "INSERT INTO deliveries (subscriber, seq) VALUES (?, ?)
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
    Known(Vec<SubscriberSpec>),
}
