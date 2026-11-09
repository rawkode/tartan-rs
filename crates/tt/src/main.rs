//! `tt`, the Tartan command-line interface (design D16).
//!
//! `tt` signs in to forges, manages access tokens, resolves identities, signs and verifies Git
//! objects as Git's `gpg.x509.program`, and holds the operator commands. When Git runs it to
//! sign or verify (`tt --status-fd=…`, or `tt sign --status-fd=…`), its argument list is
//! recognised and parsed by hand before clap, because Git's argv is not a `tt` command line.

mod account;
mod admin;
mod cli;
mod config;
mod credentials;
mod ctx;
mod forge;
mod git;
mod gpg;
mod loopback;
mod oauth;
mod paths;
mod pds;
mod queue;
mod setup;
mod sign;
mod signcmd;
mod state;
mod verify;

use clap::Parser as _;

use crate::cli::{Cli, Command, IdentityCommand};
use crate::ctx::Ctx;
use crate::gpg::GpgCall;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("tt: cannot start: {error}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(run(args));
    // Leave without waiting for background tasks (an idle connection pool, for example).
    runtime.shutdown_timeout(std::time::Duration::from_millis(100));
    std::process::exit(code);
}

async fn run(args: Vec<String>) -> i32 {
    // Git's signing program interface, with or without a leading `sign`.
    let gpg_args = match args.first().map(String::as_str) {
        Some("sign") if gpg::is_gpg_argv(&args[1..]) => Some(&args[1..]),
        _ if gpg::is_gpg_argv(&args) => Some(&args[..]),
        _ => None,
    };
    if let Some(gpg_args) = gpg_args {
        return git_program(gpg_args).await;
    }
    let cli = Cli::parse_from(std::iter::once("tt".to_owned()).chain(args));
    if let Command::Admin(admin) = cli.command {
        return report(admin::run(admin.command).await);
    }
    let ctx = match Ctx::from_env() {
        Ok(ctx) => ctx,
        Err(error) => {
            eprintln!("tt: {error:#}");
            return 1;
        }
    };
    let result = match cli.command {
        Command::Login(login) => account::login(&ctx, login).await,
        Command::Logout(logout) => account::logout(&ctx, logout).await,
        Command::Status(status) => account::status(&ctx, status).await,
        Command::Token(token) => account::token(&ctx, token).await,
        Command::Identity(identity) => match identity.command {
            IdentityCommand::Resolve { who } => account::resolve(&ctx, &who).await,
        },
        Command::Sign(sign) => signcmd::run(&ctx, *sign).await,
        Command::Admin(_) => Ok(0),
    };
    report(result)
}

fn report(result: anyhow::Result<i32>) -> i32 {
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tt: {error:#}");
            1
        }
    }
}

/// Git running `tt` as its signing program. Standard input is always read to its end, even
/// when `tt` is about to fail, so Git never meets a closed pipe.
async fn git_program(args: &[String]) -> i32 {
    let call = match gpg::parse(args) {
        Ok(call) => call,
        Err(message) => {
            let _ = sign::read_payload();
            eprintln!("tt: {message} (tt understands only Git's x509 signing and verify calls)");
            return 2;
        }
    };
    if let GpgCall::OpenPgpVerify { status_fd } = call {
        return verify::openpgp_verify(status_fd);
    }
    let ctx = match Ctx::from_env() {
        Ok(ctx) => ctx,
        Err(error) => {
            let _ = sign::read_payload();
            eprintln!("tt: {error:#}");
            return 1;
        }
    };
    match call {
        GpgCall::Sign { status_fd, key } => sign::git_sign(&ctx, status_fd, &key).await,
        GpgCall::Verify { status_fd, file } => verify::git_verify(&ctx, status_fd, &file).await,
        GpgCall::OpenPgpVerify { .. } => 1,
    }
}
