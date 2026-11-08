//! The Tartan forge: serves this process's share of the actors and the HTTP gateway.
//!
//! Configuration comes from the environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `TARTAN_LISTEN` | the gateway's address, default `127.0.0.1:8080` |
//! | `TARTAN_FORGE_ID` | this forge's id within the Rivet namespace, default `forge` |
//! | `TARTAN_ADMIN_TOKEN` | the bearer token of the forge API |
//! | `TARTAN_WEBHOOK_SECRET` | the key the Git host signs its webhook with |
//! | `TARTAN_GIT_HOST_URL` | the Git host (walgit) base URL |
//! | `TARTAN_GIT_HOST_TOKEN` | optional: the bearer token for the Git host |
//! | `RIVET_ENDPOINT`, `RIVET_TOKEN`, `RIVET_NAMESPACE`, `RIVET_POOL` | the Rivet engine and the pool this process serves |

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rivetkit::client::{Client, ClientConfig};
use rivetkit::{Registry, ServeConfig};
use tartan::{GatewayState, router};
use tartan_forge::{Services, WalgitClient, install_services, register_actors};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let listen: SocketAddr = env_or("TARTAN_LISTEN", "127.0.0.1:8080")
        .parse()
        .context("TARTAN_LISTEN")?;
    let admin_token = required("TARTAN_ADMIN_TOKEN")?;
    let webhook_secret = required("TARTAN_WEBHOOK_SECRET")?;
    let git_host = WalgitClient::new(
        &required("TARTAN_GIT_HOST_URL")?,
        std::env::var("TARTAN_GIT_HOST_TOKEN").ok(),
    );
    install_services(Services {
        forge_id: env_or("TARTAN_FORGE_ID", "forge"),
        git_host: Arc::new(git_host),
    })?;

    let serve_config = ServeConfig::from_env()?;
    let client = Client::new(
        ClientConfig::new(&serve_config.endpoint)
            .token_opt(serve_config.token.clone())
            .namespace(&serve_config.namespace)
            .pool_name(&serve_config.pool_name),
    );

    let shutdown = CancellationToken::new();
    let mut registry = Registry::new();
    register_actors(&mut registry);
    let actors = tokio::spawn({
        let shutdown = shutdown.clone();
        async move { registry.serve_with_config(serve_config, shutdown).await }
    });

    let state = GatewayState {
        client,
        admin_token: Arc::from(admin_token),
        webhook_secret: Arc::from(webhook_secret.into_bytes()),
    };
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    tracing::info!(%listen, "tartan gateway listening");
    let gateway = axum::serve(listener, router(state)).with_graceful_shutdown({
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        }
    });

    gateway.await.context("gateway")?;
    actors.await.context("actor host task")??;
    Ok(())
}

fn required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
