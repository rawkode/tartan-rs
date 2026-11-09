//! Jetstream as a trigger only (design D10).
//!
//! Each gateway process runs one [`Jetstream`] task against v1 `/subscribe`. It connects with
//! `wantedCollections=academy.rawkode.tartan.signature&requireHello=true` and then sends the
//! whole filter as an `options_update`: the collection together with every DID the forge
//! watches, never DIDs in the URL. Jetstream replaces a connection's whole filter on each
//! update, so every later update (when the watched set changes) carries both again.
//!
//! Nothing an event says is believed. The task filters again itself:
//! - events for DIDs the forge does not watch are ignored, whatever their kind;
//! - a commit becomes a `ReproveSignature` only for a `delete` or `update` in the signature
//!   collection (the identity actor answers an untracked or revoked record without fetching),
//!   after dropping that record's cached proof;
//! - identity and account events become a payload-free `RefreshIdentity`, after dropping the
//!   DID's cached proofs.
//!
//! The cursor is stored in the identity actor every 1000 events or 10 seconds and rewound by 5
//! seconds on every connect; replayed events repeat idempotent work. Frames and messages are
//! capped at 1 MiB. With more DIDs than Jetstream's `wantedDids` limit, or after a server
//! refused a full update by closing the connection, the task sends an empty `wantedDids` (every
//! DID) and relies on its own filter, logging a warning.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rivetkit::client::Client;
use serde::Deserialize;
use serde_json::json;
use tartan_atproto::signature::COLLECTION;
use tartan_forge::identity_handle;
use tartan_forge::proof_cache::proof_cache;
use tartan_identity_api::{
    GetJetstreamCursor, RefreshIdentity, ReproveSignature, SetJetstreamCursor, WatchedDids,
};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_util::sync::CancellationToken;

/// The largest frame or message accepted.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Jetstream's limit on `wantedDids`.
pub const MAX_WANTED_DIDS: usize = 10_000;
/// How far the stored cursor is rewound on connect.
const REWIND_US: i64 = 5_000_000;

/// How the task runs.
#[derive(Clone, Debug)]
pub struct JetstreamOptions {
    /// v1 `/subscribe` (`TARTAN_JETSTREAM_URL`).
    pub url: String,
    /// How often the watched DIDs are read again; a change sends a new `options_update`.
    pub watch_every: Duration,
    /// The cursor is stored at least this often while events arrive.
    pub cursor_every: Duration,
    /// The cursor is stored after this many events.
    pub cursor_events: u64,
    /// The most DIDs sent in `wantedDids`; beyond it the task sends none and filters itself.
    pub max_wanted_dids: usize,
    /// The pause before reconnecting.
    pub reconnect_after: Duration,
    /// A connection the server closes within this time of a full update counts as a refusal
    /// of that update.
    pub refusal_window: Duration,
}

impl JetstreamOptions {
    /// The production options for `url`.
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            watch_every: Duration::from_secs(30),
            cursor_every: Duration::from_secs(10),
            cursor_events: 1000,
            max_wanted_dids: MAX_WANTED_DIDS,
            reconnect_after: Duration::from_secs(5),
            refusal_window: Duration::from_secs(5),
        }
    }
}

/// The filter of one `options_update`, as sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentFilter {
    /// `wantedCollections`.
    pub collections: Vec<String>,
    /// How many `wantedDids` it carried (0: every DID).
    pub dids: usize,
}

/// What the task has done, for logs and tests.
#[derive(Debug, Default)]
pub struct JetstreamStats {
    /// Connections opened.
    pub connections: AtomicU64,
    /// Event messages received.
    pub events: AtomicU64,
    /// Events ignored because their DID is not watched.
    pub ignored_unwatched: AtomicU64,
    /// Commit events received in a collection other than the signature collection.
    pub other_collections: AtomicU64,
    /// Signature-record creates ignored.
    pub creates_ignored: AtomicU64,
    /// `ReproveSignature` calls made.
    pub reproves: AtomicU64,
    /// `RefreshIdentity` calls made.
    pub refreshes: AtomicU64,
    /// Connections that fell back to an empty `wantedDids`.
    pub fallbacks: AtomicU64,
    /// Every `options_update` sent, in order.
    pub filters: Mutex<Vec<SentFilter>>,
}

impl JetstreamStats {
    /// A counter's value.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::SeqCst)
    }

    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::SeqCst);
    }

    /// The `options_update` messages sent so far.
    pub fn filters(&self) -> Vec<SentFilter> {
        self.filters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Starts the task; it runs until `cancel` fires.
pub fn spawn(
    client: Client,
    options: JetstreamOptions,
    cancel: CancellationToken,
) -> Arc<JetstreamStats> {
    let stats = Arc::new(JetstreamStats::default());
    let task = Jetstream {
        client,
        options,
        stats: stats.clone(),
    };
    tokio::spawn(async move { task.run(cancel).await });
    stats
}

/// The `options_update` message carrying `dids` (empty: every DID) with the signature
/// collection.
pub fn options_update(dids: &[String]) -> String {
    json!({
        "type": "options_update",
        "payload": {
            "wantedCollections": [COLLECTION],
            "wantedDids": dids,
            "maxMessageSizeBytes": MAX_MESSAGE_BYTES,
        },
    })
    .to_string()
}

/// The URL to connect to: `base` with the collection, `requireHello=true` and the cursor.
pub fn subscribe_url(base: &str, cursor_us: Option<i64>) -> Result<String, String> {
    let mut url = reqwest::Url::parse(base).map_err(|error| format!("{base}: {error}"))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(format!("{base} is not a ws:// or wss:// URL"));
    }
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("wantedCollections", COLLECTION)
            .append_pair("requireHello", "true");
        if let Some(cursor) = cursor_us {
            query.append_pair("cursor", &cursor.to_string());
        }
    }
    Ok(url.to_string())
}

/// The WebSocket limits: 1 MiB frames and messages.
pub fn socket_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_frame_size = Some(MAX_MESSAGE_BYTES);
    config.max_message_size = Some(MAX_MESSAGE_BYTES);
    config
}

#[derive(Debug, Deserialize)]
struct Event {
    did: String,
    time_us: Option<i64>,
    kind: String,
    commit: Option<Commit>,
}

#[derive(Debug, Deserialize)]
struct Commit {
    operation: String,
    collection: String,
    rkey: String,
}

struct Jetstream {
    client: Client,
    options: JetstreamOptions,
    stats: Arc<JetstreamStats>,
}

/// How one connection ended.
enum Ended {
    Cancelled,
    /// Nothing is watched yet, so no connection was opened.
    Idle,
    /// The server closed or failed; `refused` when that came right after a full update.
    Closed {
        refused: bool,
    },
}

impl Jetstream {
    async fn run(self, cancel: CancellationToken) {
        tracing::info!(url = %self.options.url, "Jetstream: starting");
        // After a refused full update, later connections send an empty `wantedDids` until the
        // watched set changes size.
        let mut fallback_for: Option<usize> = None;
        loop {
            if cancel.is_cancelled() {
                return;
            }
            match self.connection(&cancel, &mut fallback_for).await {
                Ok(Ended::Cancelled) => return,
                Ok(Ended::Idle) => continue,
                Ok(Ended::Closed { refused }) => {
                    if refused {
                        tracing::warn!(
                            "Jetstream: the server closed the connection after a full update; \
                             sending an empty wantedDids and filtering here"
                        );
                    } else {
                        tracing::warn!("Jetstream: disconnected; reconnecting");
                    }
                }
                Err(error) => tracing::warn!(%error, "Jetstream: connection failed; retrying"),
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(self.options.reconnect_after) => {}
            }
        }
    }

    async fn watched(&self) -> anyhow::Result<Vec<String>> {
        identity_handle(&self.client)?.send(WatchedDids {}).await
    }

    async fn connection(
        &self,
        cancel: &CancellationToken,
        fallback_for: &mut Option<usize>,
    ) -> anyhow::Result<Ended> {
        let identity = identity_handle(&self.client)?;
        let mut watched = self.watched().await?;
        if watched.is_empty() {
            // An empty `wantedDids` would ask for every DID; wait until the forge watches one.
            tokio::select! {
                () = cancel.cancelled() => return Ok(Ended::Cancelled),
                () = tokio::time::sleep(self.options.watch_every) => {}
            }
            return Ok(Ended::Idle);
        }
        let cursor = identity
            .send(GetJetstreamCursor {})
            .await?
            .map(|cursor| (cursor - REWIND_US).max(0));
        let url = subscribe_url(&self.options.url, cursor).map_err(anyhow::Error::msg)?;
        let (socket, _) = tokio_tungstenite::connect_async_with_config(
            url.as_str(),
            Some(socket_config()),
            false,
        )
        .await?;
        JetstreamStats::bump(&self.stats.connections);
        let (mut sink, mut stream) = socket.split();

        let mut filter_set: HashSet<String> = watched.iter().cloned().collect();
        if fallback_for.is_some_and(|size| size != watched.len()) {
            *fallback_for = None;
        }
        let mut full_sent_at = self.send_filter(&mut sink, &watched, *fallback_for).await?;

        let mut watch = tokio::time::interval(self.options.watch_every);
        watch.tick().await;
        let mut store = tokio::time::interval(self.options.cursor_every);
        store.tick().await;
        let mut latest: Option<i64> = None;
        let mut stored: Option<i64> = cursor;
        let mut since_stored = 0_u64;

        let ended = loop {
            tokio::select! {
                () = cancel.cancelled() => break Ended::Cancelled,
                _ = watch.tick() => {
                    let now_watched = self.watched().await?;
                    if now_watched.is_empty() {
                        break Ended::Closed { refused: false };
                    }
                    if now_watched != watched {
                        watched = now_watched;
                        filter_set = watched.iter().cloned().collect();
                        if fallback_for.is_some_and(|size| size != watched.len()) {
                            *fallback_for = None;
                        }
                        full_sent_at = self.send_filter(&mut sink, &watched, *fallback_for).await?;
                    }
                }
                _ = store.tick() => {
                    if latest != stored && let Some(cursor) = latest {
                        identity.send(SetJetstreamCursor { cursor_us: cursor }).await?;
                        stored = latest;
                        since_stored = 0;
                    }
                }
                message = stream.next() => {
                    let text = match message {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(_))) | None => {
                            break Ended::Closed { refused: self.refused(full_sent_at) };
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(error)) => {
                            tracing::warn!(%error, "Jetstream: the connection failed");
                            break Ended::Closed { refused: self.refused(full_sent_at) };
                        }
                    };
                    // Events arrived after the update, so the server accepted it.
                    full_sent_at = None;
                    JetstreamStats::bump(&self.stats.events);
                    let Ok(event) = serde_json::from_str::<Event>(text.as_str()) else {
                        continue;
                    };
                    if let Some(time) = event.time_us {
                        latest = Some(latest.map_or(time, |latest| latest.max(time)));
                    }
                    self.handle(&filter_set, event).await;
                    since_stored += 1;
                    if since_stored >= self.options.cursor_events && let Some(cursor) = latest {
                        identity.send(SetJetstreamCursor { cursor_us: cursor }).await?;
                        stored = latest;
                        since_stored = 0;
                    }
                }
            }
        };
        if let Some(cursor) = latest
            && latest != stored
        {
            identity
                .send(SetJetstreamCursor { cursor_us: cursor })
                .await?;
        }
        if let Ended::Closed { refused: true } = ended {
            *fallback_for = Some(watched.len());
            JetstreamStats::bump(&self.stats.fallbacks);
        }
        Ok(ended)
    }

    fn refused(&self, full_sent_at: Option<Instant>) -> bool {
        full_sent_at.is_some_and(|at| at.elapsed() < self.options.refusal_window)
    }

    /// Sends the filter for `watched`; answers when a full (non-empty) DID list went out.
    async fn send_filter<S>(
        &self,
        sink: &mut S,
        watched: &[String],
        fallback_for: Option<usize>,
    ) -> anyhow::Result<Option<Instant>>
    where
        S: futures_util::Sink<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let unfiltered = watched.len() > self.options.max_wanted_dids || fallback_for.is_some();
        if watched.len() > self.options.max_wanted_dids {
            tracing::warn!(
                watched = watched.len(),
                limit = self.options.max_wanted_dids,
                "Jetstream: more watched DIDs than wantedDids allows; filtering here"
            );
        }
        let dids: &[String] = if unfiltered { &[] } else { watched };
        sink.send(Message::text(options_update(dids))).await?;
        self.stats
            .filters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SentFilter {
                collections: vec![COLLECTION.to_owned()],
                dids: dids.len(),
            });
        Ok((!dids.is_empty()).then(Instant::now))
    }

    async fn handle(&self, watched: &HashSet<String>, event: Event) {
        if !watched.contains(&event.did) {
            JetstreamStats::bump(&self.stats.ignored_unwatched);
            return;
        }
        let identity = match identity_handle(&self.client) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(%error, "Jetstream: no identity actor client");
                return;
            }
        };
        match event.kind.as_str() {
            "commit" => {
                let Some(commit) = event.commit else {
                    return;
                };
                if commit.collection != COLLECTION {
                    JetstreamStats::bump(&self.stats.other_collections);
                    return;
                }
                if !matches!(commit.operation.as_str(), "delete" | "update") {
                    JetstreamStats::bump(&self.stats.creates_ignored);
                    return;
                }
                proof_cache().drop_record(&event.did, &commit.rkey);
                JetstreamStats::bump(&self.stats.reproves);
                let reproved = identity
                    .send(ReproveSignature {
                        did: event.did.clone(),
                        rkey: commit.rkey.clone(),
                    })
                    .await;
                match reproved {
                    Ok(outcome) => {
                        tracing::debug!(did = %event.did, rkey = %commit.rkey, ?outcome, "Jetstream: re-proved")
                    }
                    Err(error) => {
                        tracing::warn!(did = %event.did, %error, "Jetstream: re-proof failed")
                    }
                }
            }
            "identity" | "account" => {
                proof_cache().drop_did(&event.did);
                JetstreamStats::bump(&self.stats.refreshes);
                if let Err(error) = identity
                    .send(RefreshIdentity {
                        did: event.did.clone(),
                    })
                    .await
                {
                    tracing::warn!(did = %event.did, %error, "Jetstream: refresh failed");
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_update_carries_the_collection() {
        let message: serde_json::Value =
            serde_json::from_str(&options_update(&["did:plc:a".to_owned()])).expect("json");
        assert_eq!(message["type"], "options_update");
        assert_eq!(message["payload"]["wantedCollections"], json!([COLLECTION]));
        assert_eq!(message["payload"]["wantedDids"], json!(["did:plc:a"]));
        let empty: serde_json::Value = serde_json::from_str(&options_update(&[])).expect("json");
        assert_eq!(empty["payload"]["wantedCollections"], json!([COLLECTION]));
    }

    #[test]
    fn dids_never_go_in_the_url() {
        let url = subscribe_url("wss://jetstream.example/subscribe", Some(42)).expect("url");
        assert!(url.contains("wantedCollections=academy.rawkode.tartan.signature"));
        assert!(url.contains("requireHello=true"));
        assert!(url.contains("cursor=42"));
        assert!(!url.contains("wantedDids"));
        assert!(subscribe_url("https://jetstream.example/subscribe", None).is_err());
    }
}
