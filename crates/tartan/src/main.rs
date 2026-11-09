//! The Tartan forge: serves this process's share of the actors, the HTTP gateway and the
//! forge's CUE registry.
//!
//! Configuration comes from the environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `CUE_REGISTRY` | required: the forge's own registry (its gateway), which every evaluation resolves modules through |
//! | `TARTAN_CONFIG` | the deployment configuration: a CUE file or directory of package `forge` |
//! | `TARTAN_LISTEN` | the gateway's address, default `127.0.0.1:8080` |
//! | `TARTAN_FORGE_ID` | this forge's id within the Rivet namespace, default `forge` |
//! | `TARTAN_ADMIN_TOKEN` | the bearer token of the forge API |
//! | `TARTAN_WEBHOOK_SECRET` | the key the Git host signs its webhook with |
//! | `TARTAN_GIT_HOST_URL` | the Git host (walgit) base URL |
//! | `TARTAN_GIT_HOST_TOKEN` | optional: the bearer token for the Git host |
//! | `TARTAN_S3_ENDPOINT`, `TARTAN_S3_BUCKET` | the object store for the registry and extension packages |
//! | `TARTAN_S3_ACCESS_KEY`, `TARTAN_S3_SECRET_KEY` | its credentials |
//! | `TARTAN_S3_REGION` | optional, default `us-east-1` |
//! | `RIVET_ENDPOINT`, `RIVET_TOKEN`, `RIVET_NAMESPACE`, `RIVET_POOL` | the Rivet engine and the pool this process serves |

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::prefix::PrefixStore;
use rivetkit::client::{Client, ClientConfig};
use rivetkit::{Registry, ServeConfig};
use tartan::{GatewayState, RegistryStore, SyncTargets, router, sync_modules};
use tartan_config::deployment::{self, Deployment};
use tartan_extensions::ExtensionRuntime;
use tartan_forge::{Extensions, Services, WalgitClient, install_services, register_actors};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // The CUE evaluator reads CUE_REGISTRY once, from the environment the process starts with,
    // so it cannot be supplied later. Without it, repositories' imports would resolve against
    // the public registry instead of the modules this forge makes available.
    if std::env::var_os("CUE_REGISTRY").is_none_or(|value| value.is_empty()) {
        tracing::error!(
            "CUE_REGISTRY must name this forge's registry (its gateway, such as 127.0.0.1:8080+insecure) so configurations resolve modules through it"
        );
        anyhow::bail!("CUE_REGISTRY must be set");
    }
    let deployment = match std::env::var_os("TARTAN_CONFIG") {
        Some(path) => {
            let deployment = deployment::load(path.as_ref()).inspect_err(|error| {
                tracing::error!(%error, "the deployment configuration (TARTAN_CONFIG) is invalid");
            })?;
            tracing::info!(
                modules = deployment.modules.len(),
                "deployment configuration read"
            );
            deployment
        }
        None => {
            tracing::warn!("TARTAN_CONFIG is not set: no modules are made available");
            Deployment::default()
        }
    };

    let listen: SocketAddr = env_or("TARTAN_LISTEN", "127.0.0.1:8080")
        .parse()
        .context("TARTAN_LISTEN")?;
    let admin_token = required("TARTAN_ADMIN_TOKEN")?;
    let webhook_secret = required("TARTAN_WEBHOOK_SECRET")?;
    let git_host = WalgitClient::new(
        &required("TARTAN_GIT_HOST_URL")?,
        std::env::var("TARTAN_GIT_HOST_TOKEN").ok(),
    );
    let object_store = AmazonS3Builder::new()
        .with_endpoint(required("TARTAN_S3_ENDPOINT")?)
        .with_allow_http(true)
        .with_bucket_name(required("TARTAN_S3_BUCKET")?)
        .with_region(env_or("TARTAN_S3_REGION", "us-east-1"))
        .with_access_key_id(required("TARTAN_S3_ACCESS_KEY")?)
        .with_secret_access_key(required("TARTAN_S3_SECRET_KEY")?)
        .build()
        .context("the object store")?;
    let store: Arc<dyn ObjectStore> = Arc::new(PrefixStore::new(object_store, "tartan"));
    let registry = RegistryStore::new(store.clone());
    // The registry serves the listed modules it already holds from the moment it listens.
    registry.list(
        deployment
            .modules
            .iter()
            .map(|m| (m.path.as_str(), m.version.as_str())),
    );
    let extensions = Arc::new(Extensions::new(ExtensionRuntime::new()?, store));
    install_services(Services {
        forge_id: env_or("TARTAN_FORGE_ID", "forge"),
        git_host: Arc::new(git_host),
        extensions: extensions.clone(),
    })?;

    let serve_config = ServeConfig::from_env()?;
    let client = Client::new(
        ClientConfig::new(&serve_config.endpoint)
            .token_opt(serve_config.token.clone())
            .namespace(&serve_config.namespace)
            .pool_name(&serve_config.pool_name),
    );

    let shutdown = CancellationToken::new();
    let mut actor_registry = Registry::new();
    register_actors(&mut actor_registry);
    let actors = tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            actor_registry
                .serve_with_config(serve_config, shutdown)
                .await
        }
    });

    let state = GatewayState {
        client: client.clone(),
        admin_token: Arc::from(admin_token),
        webhook_secret: Arc::from(webhook_secret.into_bytes()),
        registry: registry.clone(),
    };
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    tracing::info!(%listen, "tartan gateway listening");

    // The registry serves what it already holds while the sync copies what is missing.
    // Repositories apply their configurations once the sync has finished.
    tokio::spawn(async move {
        let targets = SyncTargets {
            registry: &registry,
            client: &client,
            extensions: &extensions,
        };
        let report = sync_modules(&deployment, &targets).await;
        for problem in report.unreachable.iter().chain(&report.mismatched) {
            tracing::warn!(module = %problem.module, reason = %problem.reason, "module not synced");
        }
        for problem in &report.refused {
            tracing::warn!(module = %problem.module, reason = %problem.reason, "package not available");
        }
        tracing::info!(
            copied = ?report.copied,
            unchanged = ?report.unchanged,
            registered = ?report.registered,
            "modules synced"
        );
    });

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
