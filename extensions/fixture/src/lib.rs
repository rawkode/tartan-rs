//! `tartan.fixture`: a test extension for the host's event delivery.
//!
//! - `on-event` records each event it receives in its `seen` table and emits `seen` with the
//!   event's sequence number; an update of `refs/heads/fail` makes it fail instead.
//! - the `seen` tool returns what it has recorded, oldest first.

wit_bindgen::generate!({
    path: "../../wit",
    world: "extension",
});

use serde_json::{Value, json};

use crate::tartan::ext::effects;
use crate::tartan::ext::sql::{self, Value as SqlValue};
use crate::tartan::ext::types::Verdict;

struct Fixture;

impl Guest for Fixture {
    fn init() -> Result<(), Error> {
        Ok(())
    }

    fn on_event(event: Event) -> Result<(), Error> {
        let data: Value = serde_json::from_str(&event.data)
            .map_err(|error| Error::Invalid(error.to_string()))?;
        if data["ref_name"] == "refs/heads/fail" {
            return Err(Error::Internal("refs/heads/fail always fails".to_owned()));
        }
        let seq = i64::try_from(event.seq).map_err(|error| Error::Invalid(error.to_string()))?;
        sql::exec(
            "INSERT OR IGNORE INTO seen (source, seq, type) VALUES (?, ?, ?)",
            &[
                SqlValue::Text(event.source.clone()),
                SqlValue::Integer(seq),
                SqlValue::Text(event.kind.clone()),
            ],
        )?;
        let payload = json!({ "source": event.source, "seq": event.seq, "type": event.kind });
        effects::emit("seen", None, &payload.to_string())
    }

    fn on_timer(_key: String) -> Result<(), Error> {
        Ok(())
    }

    fn gate(_point: String, _input: String, _ctx: SlotContext) -> Result<GateDecision, Error> {
        Ok(GateDecision {
            verdict: Verdict::Allow,
            message: String::new(),
            annotations: "[]".to_owned(),
            full_scan: false,
        })
    }

    fn echo(_event: Event, _input: String) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }

    fn render(slot: String, _ctx: SlotContext, _props: String) -> Result<String, Error> {
        Err(Error::NotFound(format!("slot {slot}")))
    }

    fn on_action(action: String, _payload: String, _ctx: SlotContext) -> Result<String, Error> {
        Err(Error::NotFound(format!("action {action}")))
    }

    fn call_tool(name: String, _args: String, _ctx: SlotContext) -> Result<String, Error> {
        if name != "seen" {
            return Err(Error::NotFound(format!("tool {name}")));
        }
        let rows = sql::exec("SELECT source, seq, type FROM seen ORDER BY source, seq", &[])?;
        let seen: Vec<Value> = rows
            .values
            .iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Text(source), SqlValue::Integer(seq), SqlValue::Text(kind)] => {
                    json!({ "source": source, "seq": seq, "type": kind })
                }
                _ => Value::Null,
            })
            .collect();
        Ok(json!({ "seen": seen }).to_string())
    }

    fn provide_context(_request: String) -> Result<String, Error> {
        Ok("{}".to_owned())
    }
}

export!(Fixture);
