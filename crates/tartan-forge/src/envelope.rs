use serde::{Deserialize, Serialize};

/// One event: a state change recorded by the actor named by `source`, at position `seq` of that
/// actor's log. Events from one source are delivered in `seq` order; `(source, seq)` identifies an
/// event, so consumers deduplicate on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// A ULID, unique across the forge.
    pub id: String,
    /// The actor that recorded the event: `forge`, `org:<id>` or `repo:<id>`.
    pub source: String,
    /// The event's position in its source's log, from 1.
    pub seq: u64,
    /// The event type, dot-separated, such as `ref.updated`.
    #[serde(rename = "type")]
    pub kind: String,
    /// When it was recorded, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The event's payload.
    pub data: serde_json::Value,
}
