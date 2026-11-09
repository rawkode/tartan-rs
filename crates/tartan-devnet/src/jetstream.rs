//! A Jetstream-compatible v1 `/subscribe` over the devnet's own events.
//!
//! It follows bluesky-social/jetstream's semantics:
//! - the query takes repeated `wantedCollections` and `wantedDids`, a `cursor` in Unix
//!   microseconds and `requireHello`; with `requireHello=true` nothing is sent until the
//!   client's first `options_update`;
//! - every `options_update` replaces the connection's whole filter, and an empty or absent list
//!   matches everything, so an update carrying only `wantedDids` clears the collection filter;
//! - collections filter commit events only (a `prefix.*` entry matches a namespace); identity
//!   and account events are filtered by `wantedDids` alone;
//! - an update with more than 10,000 DIDs or 100 collections is refused by closing the
//!   connection.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};

use crate::Shared;
use crate::util::{all, first, form_pairs};

/// Jetstream's limit on `wantedDids`.
pub(crate) const MAX_WANTED_DIDS: usize = 10_000;
/// Jetstream's limit on `wantedCollections`.
pub(crate) const MAX_WANTED_COLLECTIONS: usize = 100;

/// One event as subscribers receive it.
#[derive(Debug)]
pub(crate) struct Event {
    pub time_us: u64,
    pub did: String,
    pub kind: String,
    pub collection: Option<String>,
    pub text: String,
}

/// A connection's filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    pub collections: Vec<String>,
    pub dids: HashSet<String>,
}

impl Filter {
    fn new(collections: Vec<String>, dids: Vec<String>) -> Result<Self, String> {
        if dids.len() > MAX_WANTED_DIDS {
            return Err(format!("too many wantedDids (at most {MAX_WANTED_DIDS})"));
        }
        if collections.len() > MAX_WANTED_COLLECTIONS {
            return Err(format!(
                "too many wantedCollections (at most {MAX_WANTED_COLLECTIONS})"
            ));
        }
        Ok(Filter {
            collections,
            dids: dids.into_iter().collect(),
        })
    }

    /// Parses an `options_update` message into the filter that replaces the current one.
    pub(crate) fn from_options_update(text: &str) -> Result<Option<Self>, String> {
        let message: Value =
            serde_json::from_str(text).map_err(|error| format!("invalid message: {error}"))?;
        if message.get("type").and_then(Value::as_str) != Some("options_update") {
            return Ok(None);
        }
        let payload = message.get("payload").cloned().unwrap_or(Value::Null);
        let list = |key: &str| -> Vec<String> {
            payload
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Filter::new(list("wantedCollections"), list("wantedDids")).map(Some)
    }

    pub(crate) fn matches(&self, event: &Event) -> bool {
        if !self.dids.is_empty() && !self.dids.contains(&event.did) {
            return false;
        }
        match (&event.collection, event.kind.as_str()) {
            (Some(collection), "commit") => {
                self.collections.is_empty()
                    || self
                        .collections
                        .iter()
                        .any(|wanted| match wanted.strip_suffix(".*") {
                            Some(prefix) => collection
                                .strip_prefix(prefix)
                                .is_some_and(|rest| rest.starts_with('.')),
                            None => wanted == collection,
                        })
            }
            _ => true,
        }
    }
}

/// `GET /subscribe`: upgrades to a WebSocket.
pub(crate) async fn subscribe(State(shared): State<Arc<Shared>>, mut request: Request) -> Response {
    let pairs = form_pairs(request.uri().query().unwrap_or_default().as_bytes());
    let filter = match Filter::new(
        all(&pairs, "wantedCollections")
            .into_iter()
            .map(str::to_owned)
            .collect(),
        all(&pairs, "wantedDids")
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ) {
        Ok(filter) => filter,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let cursor = first(&pairs, "cursor").and_then(|c| c.parse::<u64>().ok());
    let require_hello = first(&pairs, "requireHello").is_some_and(|v| v == "true");
    let Some(key) = request
        .headers()
        .get(header::SEC_WEBSOCKET_KEY)
        .map(|k| derive_accept_key(k.as_bytes()))
    else {
        return (StatusCode::BAD_REQUEST, "a WebSocket upgrade is required").into_response();
    };
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(4 << 20);
        config.max_frame_size = Some(4 << 20);
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config))
                .await;
        serve(shared, socket, filter, cursor, require_hello).await;
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, key)
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve<S>(
    shared: Arc<Shared>,
    socket: WebSocketStream<S>,
    mut filter: Filter,
    cursor: Option<u64>,
    require_hello: bool,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut events = shared.events.subscribe();
    let (mut sink, mut stream) = socket.split();
    // Events up to `sent_until` were replayed or skipped; later ones come from `events`.
    let mut sent_until = 0u64;
    let mut started = false;
    if !require_hello {
        started = true;
        match replay(&shared, &filter, cursor).await {
            Ok((messages, until)) => {
                sent_until = until;
                for message in messages {
                    if sink.send(Message::text(message)).await.is_err() {
                        return;
                    }
                }
            }
            Err(()) => return,
        }
    }
    loop {
        tokio::select! {
            message = stream.next() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                };
                match Filter::from_options_update(&text) {
                    Ok(Some(update)) => filter = update,
                    Ok(None) => continue,
                    Err(reason) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::Policy,
                                reason: reason.into(),
                            })))
                            .await;
                        return;
                    }
                }
                if !started {
                    started = true;
                    let Ok((messages, until)) = replay(&shared, &filter, cursor).await else {
                        return;
                    };
                    sent_until = until;
                    for message in messages {
                        if sink.send(Message::text(message)).await.is_err() {
                            return;
                        }
                    }
                }
            }
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                };
                if !started || event.time_us <= sent_until || !filter.matches(&event) {
                    continue;
                }
                if sink.send(Message::text(event.text.clone())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// The logged events from `cursor` that `filter` matches, and the time of the last logged
/// event (so live events at or before it are not sent twice). Without a cursor nothing is
/// replayed.
async fn replay(
    shared: &Arc<Shared>,
    filter: &Filter,
    cursor: Option<u64>,
) -> Result<(Vec<String>, u64), ()> {
    let state = shared.state.lock().await;
    let until = state.events.last().map_or(0, |event| event.time_us);
    let messages = match cursor {
        Some(cursor) => state
            .events
            .iter()
            .filter(|event| event.time_us >= cursor && filter.matches(event))
            .map(|event| event.text.clone())
            .collect(),
        None => Vec::new(),
    };
    Ok((messages, until))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, did: &str, collection: Option<&str>) -> Event {
        Event {
            time_us: 1,
            did: did.into(),
            kind: kind.into(),
            collection: collection.map(str::to_owned),
            text: String::new(),
        }
    }

    #[test]
    fn an_update_replaces_the_whole_filter() {
        let full = Filter::from_options_update(
            r#"{"type":"options_update","payload":{"wantedCollections":["academy.rawkode.tartan.signature"],"wantedDids":["did:plc:a"]}}"#,
        )
        .expect("valid")
        .expect("update");
        let post = event("commit", "did:plc:a", Some("app.bsky.feed.post"));
        assert!(!full.matches(&post));
        assert!(full.matches(&event(
            "commit",
            "did:plc:a",
            Some("academy.rawkode.tartan.signature")
        )));
        assert!(full.matches(&event("account", "did:plc:a", None)));
        assert!(!full.matches(&event("identity", "did:plc:b", None)));

        let dids_only = Filter::from_options_update(
            r#"{"type":"options_update","payload":{"wantedDids":["did:plc:a"]}}"#,
        )
        .expect("valid")
        .expect("update");
        assert!(dids_only.collections.is_empty());
        assert!(dids_only.matches(&post));
    }

    #[test]
    fn prefixes_and_limits() {
        let filter = Filter::new(vec!["app.bsky.*".into()], vec![]).expect("filter");
        assert!(filter.matches(&event("commit", "x", Some("app.bsky.feed.post"))));
        assert!(!filter.matches(&event("commit", "x", Some("app.bskyx.feed"))));
        let dids: Vec<String> = (0..=MAX_WANTED_DIDS)
            .map(|i| format!("did:plc:{i}"))
            .collect();
        assert!(Filter::new(vec![], dids.clone()).is_err());
        assert!(Filter::new(vec![], dids[..MAX_WANTED_DIDS].to_vec()).is_ok());
    }
}
