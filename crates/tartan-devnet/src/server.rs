//! Listeners and routes.
//!
//! The devnet serves each connection itself rather than through `axum::serve`, so a handler can
//! close its connection without answering: that is how an offline service, a write whose
//! answer is lost, and the tarpit's held requests look to a client.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::Extension;
use axum::response::Response;
use axum::routing::{get, post};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::{Shared, control, jetstream, oauth, pds};

/// The connection a request arrived on.
#[derive(Clone)]
pub(crate) struct Conn(Arc<ConnInner>);

struct ConnInner {
    close: Notify,
    counted: AtomicBool,
}

impl Conn {
    /// Closes the connection without answering. The handler's future is dropped with it.
    pub(crate) async fn hang_up(&self) -> Response {
        self.0.close.notify_one();
        std::future::pending().await
    }

    /// True the first time it is called on this connection.
    pub(crate) fn first_request(&self) -> bool {
        !self.0.counted.swap(true, Ordering::SeqCst)
    }
}

/// Serves `router` on `listener` until the task is aborted.
pub(crate) fn spawn(listener: TcpListener, router: Router) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let conn = Conn(Arc::new(ConnInner {
                close: Notify::new(),
                counted: AtomicBool::new(false),
            }));
            let service = TowerToHyperService::new(router.clone().layer(Extension(conn.clone())));
            tokio::spawn(async move {
                let builder = auto::Builder::new(TokioExecutor::new());
                let connection =
                    builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
                tokio::select! {
                    _ = connection => {}
                    _ = conn.0.close.notified() => {}
                }
            });
        }
    })
}

/// The main host: PLC directory, PDS (host 0), authorization server, Jetstream and controls.
pub(crate) fn main_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/{did}", get(pds::plc_document))
        .route("/{did}/log/audit", get(pds::plc_audit_log))
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::metadata),
        )
        .route("/oauth/par", post(oauth::par))
        .route("/oauth/authorize", get(oauth::authorize))
        .route("/oauth/token", post(oauth::token))
        .route("/oauth/revoke", post(oauth::revoke))
        .route("/subscribe", get(jetstream::subscribe))
        .merge(control::router())
        .with_state(shared.clone())
        .merge(pds_router(shared, 0))
}

/// A PDS host's routes.
pub(crate) fn pds_router(shared: Arc<Shared>, host: usize) -> Router {
    pds::router().with_state(pds::Ctx { shared, host })
}
