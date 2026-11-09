//! The tarpit: a listener serving `N` distinct did:web hosts `t<i>.localhost:<port>` whose PDS
//! holds every `/xrpc/` request open without answering (design D12, *A tarpit*). It counts
//! connections and requests per host, so a test can assert how many outbound requests one
//! verification attempt made. Development builds resolve every name under `localhost.` to the
//! loopback address, which is how one listener serves many hosts.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;

use crate::server::{self, Conn};
use crate::{Shared, keys};

/// A started tarpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TarpitInfo {
    /// The listener's port.
    pub port: u16,
    /// The did:web DIDs it serves, `did:web:t<i>.localhost%3A<port>`.
    pub dids: Vec<String>,
}

/// What one tarpit host has seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCounts {
    /// Connections whose first request named this host.
    pub connections: u64,
    /// Every request.
    pub requests: u64,
    /// `/xrpc/` requests (each held open).
    pub xrpc_requests: u64,
}

/// Counts per host (`t<i>.localhost:<port>`).
pub type TarpitCounts = BTreeMap<String, HostCounts>;

pub(crate) struct Tarpit {
    port: u16,
    hosts: usize,
    seed: String,
    counts: Mutex<TarpitCounts>,
}

impl Tarpit {
    fn host_index(&self, host: &str) -> Option<usize> {
        let (name, port) = host.rsplit_once(':')?;
        if port.parse::<u16>().ok()? != self.port {
            return None;
        }
        let index: usize = name
            .strip_prefix('t')?
            .strip_suffix(".localhost")?
            .parse()
            .ok()?;
        (index < self.hosts).then_some(index)
    }
}

pub(crate) async fn start(shared: &Arc<Shared>, hosts: usize) -> anyhow::Result<TarpitInfo> {
    let mut slot = shared.tarpit.lock().await;
    if let Some((_, task)) = slot.take() {
        task.abort();
    }
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let port = listener.local_addr()?.port();
    let seed = shared.state.lock().await.seed.clone();
    let tarpit = Arc::new(Tarpit {
        port,
        hosts,
        seed,
        counts: Mutex::new(BTreeMap::new()),
    });
    let router = Router::new().fallback(serve).with_state(tarpit.clone());
    let task = server::spawn(listener, router);
    *slot = Some((tarpit, task));
    Ok(TarpitInfo {
        port,
        dids: (0..hosts)
            .map(|i| format!("did:web:t{i}.localhost%3A{port}"))
            .collect(),
    })
}

pub(crate) async fn counts(shared: &Arc<Shared>) -> TarpitCounts {
    match &*shared.tarpit.lock().await {
        Some((tarpit, _)) => tarpit
            .counts
            .lock()
            .map(|counts| counts.clone())
            .unwrap_or_default(),
        None => TarpitCounts::new(),
    }
}

async fn serve(
    State(tarpit): State<Arc<Tarpit>>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(index) = tarpit.host_index(&host) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let xrpc = uri.path().starts_with("/xrpc/");
    if let Ok(mut counts) = tarpit.counts.lock() {
        let entry = counts.entry(host.clone()).or_default();
        if conn.first_request() {
            entry.connections += 1;
        }
        entry.requests += 1;
        if xrpc {
            entry.xrpc_requests += 1;
        }
    }
    if xrpc {
        return std::future::pending().await;
    }
    if uri.path() != "/.well-known/did.json" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let did = format!("did:web:t{index}.localhost%3A{}", tarpit.port);
    let key = keys::public(&keys::derive(&tarpit.seed, &format!("tarpit/{index}")));
    Json(json!({
        "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
        "id": did,
        "alsoKnownAs": [],
        "verificationMethod": [{
            "id": format!("{did}#atproto"),
            "type": "Multikey",
            "controller": did,
            "publicKeyMultibase": key.multikey(),
        }],
        "service": [{
            "id": "#atproto_pds",
            "type": "AtprotoPersonalDataServer",
            "serviceEndpoint": format!("http://{host}"),
        }],
    }))
    .into_response()
}
