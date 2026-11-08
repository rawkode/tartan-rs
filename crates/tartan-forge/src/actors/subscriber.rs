//! The subscriber actor: one per subscription. It stores what it receives and remembers, per
//! source, the last sequence number stored, so a repeated delivery is stored once.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use rivetkit::prelude::*;
use rivetkit::{Handler, action};
use serde::{Deserialize, Serialize};

use crate::Envelope;
use crate::actors::apply_schema;
use crate::event_log::READ_LIMIT;
use crate::sql::{integer, param_integer, param_text, text};

/// The subscriber actor.
#[derive(Debug)]
pub struct Subscriber;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS cursors (
        source TEXT PRIMARY KEY,
        seq INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS received (
        n INTEGER PRIMARY KEY AUTOINCREMENT,
        source TEXT NOT NULL,
        seq INTEGER NOT NULL,
        id TEXT NOT NULL,
        type TEXT NOT NULL,
        at_ms INTEGER NOT NULL,
        data TEXT NOT NULL,
        UNIQUE (source, seq)
    );
";

/// Events from one source. `events` are those of `(after, through]` that match the
/// subscriber's pattern; the source's cursor for this subscriber moves to `through` once this
/// returns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deliver {
    /// The source.
    pub source: String,
    /// The source's cursor for this subscriber before this delivery.
    pub after: u64,
    /// The last sequence number this delivery covers.
    pub through: u64,
    /// The matching events, in order.
    pub events: Vec<Envelope>,
}

impl Action for Deliver {
    type Output = ();

    const NAME: &'static str = "deliver";
}

/// The received events after position `after`, in the order received.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Received {
    /// The position to read after; 0 reads from the start.
    pub after: u64,
    /// The most events to return; capped at 500.
    pub limit: u32,
}

/// A received event and its position in the subscriber's store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReceivedEvent {
    /// The position, increasing in the order events were received.
    pub position: u64,
    /// The event.
    pub event: Envelope,
}

impl Action for Received {
    type Output = Vec<ReceivedEvent>;

    const NAME: &'static str = "received";
}

#[async_trait]
impl Actor for Subscriber {
    type Input = ();
    type Actions = (Deliver, Received);
    type Events = ();
    type ConnParams = ();
    type Action = action::Raw;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        apply_schema(ctx.sql(), SCHEMA).await?;
        Ok(Self)
    }
}

impl Handler<Deliver> for Subscriber {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, action: Deliver) -> Result<()> {
        ctx.sql()
            .transaction(
                move |tx| async move {
                    let current = tx
                        .execute(
                            "SELECT seq FROM cursors WHERE source = ?",
                            Some(vec![param_text(action.source.as_str())]),
                        )
                        .await?;
                    let cursor = match current.rows.first() {
                        Some(row) => u64::try_from(integer(row, 0)?)?,
                        None => 0,
                    };
                    if action.after > cursor {
                        tracing::warn!(source = %action.source, cursor, after = action.after,
                            "events between the subscriber's cursor and this delivery are missing");
                    }
                    for event in action.events.iter().filter(|event| event.seq > cursor) {
                        tx.execute(
                            "INSERT OR IGNORE INTO received (source, seq, id, type, at_ms, data)
                             VALUES (?, ?, ?, ?, ?, ?)",
                            Some(vec![
                                param_text(event.source.as_str()),
                                param_integer(i64::try_from(event.seq)?),
                                param_text(event.id.as_str()),
                                param_text(event.kind.as_str()),
                                param_integer(event.at_ms),
                                param_text(event.data.to_string()),
                            ]),
                        )
                        .await?;
                    }
                    tx.execute(
                        "INSERT INTO cursors (source, seq) VALUES (?, ?)
                         ON CONFLICT (source) DO UPDATE SET seq = MAX(seq, excluded.seq)",
                        Some(vec![
                            param_text(action.source.as_str()),
                            param_integer(i64::try_from(action.through)?),
                        ]),
                    )
                    .await?;
                    Ok(())
                },
                SqliteTransactionOptions {
                    name: Some("deliver"),
                    timeout: None,
                },
            )
            .await
    }
}

impl Handler<Received> for Subscriber {
    async fn handle(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        action: Received,
    ) -> Result<Vec<ReceivedEvent>> {
        let limit = action.limit.clamp(1, READ_LIMIT);
        let result = ctx
            .sql()
            .query(
                "SELECT n, source, seq, id, type, at_ms, data FROM received
                 WHERE n > ? ORDER BY n LIMIT ?",
                Some(vec![
                    param_integer(i64::try_from(action.after)?),
                    param_integer(i64::from(limit)),
                ]),
            )
            .await?;
        result
            .rows
            .iter()
            .map(|row| {
                Ok(ReceivedEvent {
                    position: u64::try_from(integer(row, 0)?)?,
                    event: Envelope {
                        source: text(row, 1)?,
                        seq: u64::try_from(integer(row, 2)?)?,
                        id: text(row, 3)?,
                        kind: text(row, 4)?,
                        at_ms: integer(row, 5)?,
                        data: serde_json::from_str(&text(row, 6)?)?,
                    },
                })
            })
            .collect()
    }
}
