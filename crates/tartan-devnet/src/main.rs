//! The `tartan-devnet` binary: serves the local ATProto network.
//!
//! - `tartan-devnet [--listen 127.0.0.1:9400] [--seed dev]` serves until interrupted and prints
//!   the environment the forge and `tt` need to use it.
//! - `tartan-devnet did <handle> [--seed dev]` prints a seeded person's DID.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use tartan_devnet::{Devnet, DevnetConfig, seeded_did};

#[derive(Parser)]
#[command(name = "tartan-devnet", about = "A local ATProto network for Tartan")]
struct Cli {
    /// The address to listen on.
    #[arg(long, default_value = "127.0.0.1:9400", global = true)]
    listen: SocketAddr,
    /// The seed people's keys and DIDs are derived from.
    #[arg(long, default_value = "dev", global = true)]
    seed: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the DID of a seeded person.
    Did {
        /// The handle, for example `alice.test`.
        handle: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Did { handle }) = cli.command {
        let did = seeded_did(&cli.seed, &handle).ok_or_else(|| {
            anyhow::anyhow!(
                "{handle} is not a seeded person (one of {})",
                tartan_devnet::SEEDED_HANDLES.join(", ")
            )
        })?;
        println!("{did}");
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let devnet = Devnet::start(DevnetConfig {
        listen: cli.listen,
        seed: cli.seed,
    })
    .await?;
    println!("tartan-devnet listening on {}", devnet.url());
    println!("  TARTAN_PLC_URL={}", devnet.url());
    println!("  TARTAN_DEV_HANDLE_RESOLVER={}", devnet.url());
    println!("  TARTAN_JETSTREAM_URL={}", devnet.jetstream_url());
    println!("  migration PDS host: {}", devnet.migration_url().await);
    for person in devnet.people().await {
        println!("  @{:<12} {}", person.handle, person.did);
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}
