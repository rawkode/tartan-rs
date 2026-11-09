//! The Tartan forge: serves this process's share of the actors, the HTTP gateway and the
//! forge's CUE registry.
//!
//! Configuration comes from the environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `CUE_REGISTRY` | required: the forge's own registry (its gateway), which every evaluation resolves modules through |
//! | `TARTAN_CONFIG` | required: the deployment configuration (package `forge`), listing `owners` |
//! | `TARTAN_PUBLIC_URL` | required: the browser-facing origin serving the web UI and proxying `/-/` and `/v2/` |
//! | `TARTAN_OAUTH_KEYS_FILE` | required: the web client's private ES256 JWKS, mode 0600 |
//! | `TARTAN_STATE_DIR` | local state; default `$XDG_STATE_HOME/tartan`, else `$HOME/.local/state/tartan` |
//! | `TARTAN_JETSTREAM_URL` | Jetstream's `/subscribe`; empty disables it |
//! | `TARTAN_TRUSTED_PROXIES` | comma-separated CIDRs whose `X-Forwarded-For` gives the client address |
//! | `TARTAN_LISTEN` | the gateway's address, default `127.0.0.1:8080` |
//! | `TARTAN_FORGE_ID` | this forge's id within the Rivet namespace, default `forge` |
//! | `TARTAN_DEV_IDENTITY`, `TARTAN_PLC_URL`, `TARTAN_DEV_HANDLE_RESOLVER` | `dev-identity` builds: the development identity network |
//! | `TARTAN_DEV_LOCALHOST_CLIENT` | `dev-identity` builds: sign in as ATProto's `http://localhost` client |
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
use tartan::settings::{self, Settings};
use tartan::{
    Access, AccessConfig, GatewayState, RegistryStore, SyncTargets, router, sync_modules,
};
use tartan_atproto::Mode;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tartan_atproto::resolver::{ResolverConfig, VerifyingResolver};
use tartan_config::deployment::{self, Deployment, Readers};
use tartan_extensions::ExtensionRuntime;
use tartan_forge::{
    Extensions, GIT_CACHE_MAX_BYTES, GitCache, Services, SystemClock, WalgitClient,
    install_services, register_actors,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    tartan::settings::refuse_arguments(&args).map_err(refuse)?;
    // The CUE evaluator reads CUE_REGISTRY once, from the environment the process starts with,
    // so it cannot be supplied later. Without it, repositories' imports would resolve against
    // the public registry instead of the modules this forge makes available.
    if std::env::var_os("CUE_REGISTRY").is_none_or(|value| value.is_empty()) {
        return Err(refuse(
            "CUE_REGISTRY must name this forge's registry (its gateway, such as \
             127.0.0.1:8080+insecure) so configurations resolve modules through it"
                .to_owned(),
        ));
    }
    let settings =
        settings::from_env(&|name| std::env::var(name).ok(), DEV_COMPILED).map_err(refuse)?;
    let config_path = std::env::var_os("TARTAN_CONFIG").unwrap_or_default();
    let deployment = deployment::load(config_path.as_ref()).inspect_err(|error| {
        tracing::error!(%error, "the deployment configuration (TARTAN_CONFIG) is invalid");
    })?;
    let resolver = identity_resolver(&settings)?;
    for owner in &deployment.owners {
        if owner.is_localhost() && !resolver.mode().is_development() {
            return Err(refuse(format!(
                "owner {owner} is a localhost DID, which only TARTAN_DEV_IDENTITY=1 accepts"
            )));
        }
    }
    tracing::info!(
        modules = deployment.modules.len(),
        owners = deployment.owners.len(),
        "deployment configuration read"
    );
    log_read_mode(&deployment.readers);
    log_owners(&resolver, &deployment);

    let listen: SocketAddr = env_or("TARTAN_LISTEN", "127.0.0.1:8080")
        .parse()
        .context("TARTAN_LISTEN")?;
    let webhook_secret = required("TARTAN_WEBHOOK_SECRET")?;
    let git_host = WalgitClient::new(
        &required("TARTAN_GIT_HOST_URL")?,
        std::env::var("TARTAN_GIT_HOST_TOKEN").ok(),
    )
    .with_cache(GitCache::new(
        settings.state_dir.join("git"),
        GIT_CACHE_MAX_BYTES,
    ));
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
    let extensions = Arc::new(Extensions::new(ExtensionRuntime::new()?, store.clone()));
    install_services(Services {
        forge_id: env_or("TARTAN_FORGE_ID", "forge"),
        git_host: Arc::new(git_host),
        extensions: extensions.clone(),
        clock: Arc::new(SystemClock),
        owners: deployment
            .owners
            .iter()
            .map(|owner| owner.as_str().to_owned())
            .collect(),
        readers: deployment.readers.clone(),
        resolver: resolver.clone(),
        identity_limits: tartan_forge::IdentityLimits::default(),
        objects: store,
        state_dir: settings.state_dir.clone(),
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
        access: Arc::new(Access::new(AccessConfig {
            public_url: settings.public_url.clone(),
            client: settings.client.clone(),
            resolver,
            owners: deployment
                .owners
                .iter()
                .map(|owner| owner.as_str().to_owned())
                .collect(),
            readers: deployment.readers.clone(),
            trusted_proxies: settings.trusted_proxies.clone(),
            clock: Arc::new(SystemClock),
        })),
        webhook_secret: Arc::from(webhook_secret.into_bytes()),
        registry: registry.clone(),
    };
    // Jetstream turns identity, account and signature-record events into re-checks (design
    // D10); an empty TARTAN_JETSTREAM_URL disables it, leaving the 6-hourly sweep.
    match &settings.jetstream_url {
        Some(url) => {
            tartan::jetstream::spawn(
                client.clone(),
                tartan::jetstream::JetstreamOptions::new(url),
                shutdown.clone(),
            );
        }
        None => tracing::info!("Jetstream is disabled (TARTAN_JETSTREAM_URL is empty)"),
    }

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    tracing::info!(%listen, public_url = settings.public_url.origin(), "tartan gateway listening");

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

    let gateway = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown({
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

/// Whether this build has the `dev-identity` feature.
const DEV_COMPILED: bool = cfg!(feature = "dev-identity");

/// Logs a startup refusal and turns it into the error `main` exits with.
fn refuse(message: String) -> anyhow::Error {
    tracing::error!("{message}");
    anyhow::anyhow!(message)
}

/// The identity resolver: `plc.directory` and DNS, or, in a `dev-identity` build run with
/// `TARTAN_DEV_IDENTITY=1`, the development PLC directory and handle resolver.
fn identity_resolver(settings: &Settings) -> Result<VerifyingResolver> {
    #[cfg(feature = "dev-identity")]
    if let Some((plc, handles)) = &settings.dev_resolver {
        let config = ResolverConfig::development(plc, handles);
        let fetch = FetchClient::new(FetchConfig::new(Mode::Development))?;
        return Ok(VerifyingResolver::new(fetch, config));
    }
    let _ = settings;
    let fetch = FetchClient::new(FetchConfig::new(Mode::Production))?;
    let config = ResolverConfig::production().context("the system DNS configuration")?;
    Ok(VerifyingResolver::new(fetch, config))
}

/// Logs who may read (design D13).
fn log_read_mode(readers: &Readers) {
    tracing::info!("{}", settings::read_mode_line(readers));
}

/// Logs each owner's DID with its handle, resolved in the background so a PLC directory or
/// PDS that is down never delays startup (design D13).
fn log_owners(
    resolver: &VerifyingResolver,
    deployment: &Deployment,
) -> tokio::task::JoinHandle<()> {
    let resolver = resolver.clone();
    let owners: Vec<String> = deployment
        .owners
        .iter()
        .map(|owner| owner.as_str().to_owned())
        .collect();
    tokio::spawn(async move {
        for owner in owners {
            let handle = match tartan_atproto::did::Did::parse(&owner, resolver.mode()) {
                Ok(did) => match resolver.resolve_did(&did).await {
                    Ok(resolved) => resolved
                        .identity
                        .and_then(|identity| identity.handle)
                        .unwrap_or_else(|| "unresolved".to_owned()),
                    Err(_) => "unresolved".to_owned(),
                },
                Err(_) => "unresolved".to_owned(),
            };
            tracing::info!(did = %owner, %handle, "owner");
        }
    })
}

fn required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} must be set"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Owners are resolved in the background: startup does not wait for an unreachable PLC
    /// directory.
    #[tokio::test]
    async fn owner_resolution_never_delays_startup() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        // A PLC directory that accepts connections and never answers.
        std::thread::spawn(move || {
            let _held: Vec<_> = listener.incoming().take(4).collect();
            std::thread::sleep(Duration::from_secs(60));
        });
        let plc = format!("http://{address}");
        let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("client");
        let resolver = VerifyingResolver::new(fetch, ResolverConfig::development(&plc, &plc));
        let deployment = Deployment {
            owners: vec!["did:plc:abcdefghijklmnopqrstuvwx".parse().expect("a DID")],
            ..Deployment::default()
        };
        let started = Instant::now();
        let logging = log_owners(&resolver, &deployment);
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(
            !logging.is_finished(),
            "the lookup is still waiting on the directory"
        );
        logging.abort();
    }
}
