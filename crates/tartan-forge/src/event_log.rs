use anyhow::Result;
use rivetkit::prelude::*;
use rivetkit::{SqliteDb, SqliteTransaction};
use serde::{Deserialize, Serialize};

use crate::Envelope;
use crate::sql::{integer, now_ms, param_integer, param_text, text};

/// The tables every event source keeps: its log, and how far each subscriber has been sent.
pub(crate) const EVENT_LOG_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS _tartan_events (
        seq INTEGER PRIMARY KEY,
        id TEXT NOT NULL UNIQUE,
        type TEXT NOT NULL,
        at_ms INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS _tartan_deliveries (
        subscriber TEXT PRIMARY KEY,
        seq INTEGER NOT NULL
    );
";

/// The most events returned by one read.
pub(crate) const READ_LIMIT: u32 = 500;

/// Reads a source's events after `after`, at most `limit` of them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadEvents {
    /// The sequence number to read after; 0 reads from the start.
    pub after: u64,
    /// The most events to return; capped at 500.
    pub limit: u32,
}

impl Action for ReadEvents {
    type Output = Vec<Envelope>;

    const NAME: &'static str = "readEvents";
}

/// Appends an event in `transaction`, so it commits or rolls back with the change it records.
pub(crate) async fn append(
    transaction: &SqliteTransaction,
    source: &str,
    kind: &str,
    data: serde_json::Value,
) -> Result<Envelope> {
    let id = ulid::Ulid::new().to_string().to_lowercase();
    let at_ms = now_ms();
    let result = transaction
        .execute(
            "INSERT INTO _tartan_events (id, type, at_ms, data) VALUES (?, ?, ?, ?) RETURNING seq",
            Some(vec![
                param_text(id.clone()),
                param_text(kind),
                param_integer(at_ms),
                param_text(data.to_string()),
            ]),
        )
        .await?;
    let row = result
        .rows
        .first()
        .ok_or_else(|| anyhow!("event insert returned no seq"))?;
    Ok(Envelope {
        id,
        source: source.to_owned(),
        seq: u64::try_from(integer(row, 0)?)?,
        kind: kind.to_owned(),
        at_ms,
        data,
    })
}

/// The events after `after`, oldest first.
pub(crate) async fn read(
    db: &SqliteDb,
    source: &str,
    after: u64,
    limit: u32,
) -> Result<Vec<Envelope>> {
    let limit = limit.clamp(1, READ_LIMIT);
    let result = db
        .query(
            "SELECT seq, id, type, at_ms, data FROM _tartan_events WHERE seq > ? ORDER BY seq LIMIT ?",
            Some(vec![
                param_integer(i64::try_from(after)?),
                param_integer(i64::from(limit)),
            ]),
        )
        .await?;
    result
        .rows
        .iter()
        .map(|row| {
            Ok(Envelope {
                seq: u64::try_from(integer(row, 0)?)?,
                id: text(row, 1)?,
                kind: text(row, 2)?,
                at_ms: integer(row, 3)?,
                data: serde_json::from_str(&text(row, 4)?)?,
                source: source.to_owned(),
            })
        })
        .collect()
}
