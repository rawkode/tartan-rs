//! `tt sign` from the command line: `--login`, `--setup`, `--flush`, `revoke` and `recheck`.
//! Git's own signing argv is handled in [`crate::sign`] and [`crate::verify`].

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::Method;
use serde_json::json;
use tartan_atproto::did::Did;
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::git::{SignatureClass, classify, extract_commit, extract_tag};

use crate::account;
use crate::cli::{RecheckArgs, RevokeArgs, SignArgs, SignCommand};
use crate::config::Config;
use crate::ctx::Ctx;
use crate::forge::{self, Forge};
use crate::oauth::{self, ClientSpec, SigningSession, Who};
use crate::pds::{self, WriteOutcome};
use crate::queue::{self, Queue};
use crate::setup::{self, SetupRequest};
use crate::sign::format_time;
use crate::state;

/// `tt sign …` without Git's argv.
pub async fn run(ctx: &Ctx, args: SignArgs) -> Result<i32> {
    match args.command {
        Some(SignCommand::Revoke(revoke)) => return revoke_signature(ctx, revoke).await,
        Some(SignCommand::Recheck(recheck)) => return recheck_signature(ctx, recheck).await,
        None => {}
    }
    if args.login {
        let mut config = Config::load(&ctx.dirs.config)?;
        login(ctx, &mut config, &args).await?;
        return Ok(0);
    }
    if args.setup && args.undo {
        setup::undo(args.global)?;
        return Ok(0);
    }
    if args.setup {
        let request = SetupRequest {
            global: args.global,
            did: args.did.clone(),
            program: args.program.clone(),
            no_tags: args.no_tags,
            verify_only: args.verify_only,
            hook: args.hook,
            hooks_path_ok: args.hooks_path_ok,
        };
        setup::setup(ctx, request, || async {
            let mut config = Config::load(&ctx.dirs.config)?;
            login(ctx, &mut config, &args).await
        })
        .await?;
        return Ok(0);
    }
    if args.flush {
        return flush(ctx, args.hook).await;
    }
    bail!("nothing to do: see `tt sign --help` (Git runs `tt` with its own arguments to sign)");
}

/// `tt sign --login`: returns the DID signed in.
async fn login(ctx: &Ctx, config: &mut Config, args: &SignArgs) -> Result<String> {
    let client = match args.client {
        Some(choice) => account::client_spec(Some(choice), ""),
        None => {
            let Some(forge) = forge::choose(ctx, args.forge.as_deref(), config)? else {
                bail!("no forge configured: run `tt login --forge <url>` or pass `--forge`");
            };
            ClientSpec::Forge { forge }
        }
    };
    let who = match &args.handle {
        Some(text) if text.trim().starts_with("did:") => Who::Did(text.trim().to_owned()),
        Some(text) => Who::Handle(text.clone()),
        None => default_who(ctx, config, args.forge.as_deref())?,
    };
    account::signing_login(
        ctx,
        config,
        who.clone(),
        client,
        args.port,
        args.no_open,
        args.set_default,
    )
    .await?;
    let config = Config::load(&ctx.dirs.config)?;
    let index = state::index(&ctx.dirs)?;
    // The DID just stored: the named one, else the newest session.
    Ok(match who {
        Who::Did(did) => did,
        Who::Handle(_) => index
            .signing
            .iter()
            .max_by_key(|(_, entry)| entry.created_at)
            .map(|(did, _)| did.clone())
            .or(config.file.default_signing_did)
            .unwrap_or_default(),
    })
}

/// Who `tt sign --login` signs in as without `--handle`: the forge session's DID, else the
/// default signing DID.
fn default_who(ctx: &Ctx, config: &Config, forge: Option<&str>) -> Result<Who> {
    let index = state::index(&ctx.dirs)?;
    if let Some(origin) = forge::choose(ctx, forge, config)?
        && let Some(entry) = index.forges.get(&origin)
        && !entry.did.is_empty()
    {
        return Ok(Who::Did(entry.did.clone()));
    }
    if let Some(did) = &config.file.default_signing_did {
        return Ok(Who::Did(did.clone()));
    }
    bail!("whom to sign in as? Pass `--handle <handle or DID>`");
}

/// `tt sign --flush [--hook]`.
async fn flush(ctx: &Ctx, hook: bool) -> Result<i32> {
    let dids = Queue::queued_dids(ctx);
    let mut remaining_total = 0;
    let deadline = tokio::time::Instant::now() + queue::FLUSH_TIME;
    for did in dids {
        let queue = Queue::new(ctx, &did);
        let lock = queue.lock()?;
        let session = SigningSession::load(ctx, &did).ok().flatten();
        let drained = queue::drain(ctx, &queue, &lock, session, deadline).await?;
        drop(lock);
        if drained.published > 0 {
            println!(
                "Published {} signature record(s) of {did}.",
                drained.published
            );
        }
        if drained.replaced > 0 {
            eprintln!(
                "tt: {} queued record(s) of {did} were replaced by other records; their signatures \
                 cannot verify (see `tt status`)",
                drained.replaced
            );
        }
        if drained.remaining > 0 {
            remaining_total += drained.remaining;
            let when = drained
                .retry_at
                .map(|at| format!("; retry after {}", format_time(at)))
                .unwrap_or_default();
            eprintln!(
                "tt: {} signature record(s) of {did} remain queued: {}{when}",
                drained.remaining,
                drained.reason.as_deref().unwrap_or("unknown reason")
            );
        }
    }
    warn_expiring(ctx);
    if remaining_total == 0 {
        if !hook {
            println!("The signing queue is empty.");
        }
        return Ok(0);
    }
    Ok(if hook { 0 } else { 1 })
}

/// Warns when a signing session has fewer than two days left.
fn warn_expiring(ctx: &Ctx) {
    let Ok(index) = state::index(&ctx.dirs) else {
        return;
    };
    let now = state::now_ms();
    for (did, entry) in &index.signing {
        if entry.ends_at() - now < 2 * 24 * 60 * 60 * 1000 {
            eprintln!(
                "tt: the signing session of {did} ends by {}; run `tt sign --login` before then",
                format_time(entry.ends_at())
            );
        }
    }
}

/// A record named by `tt sign revoke`.
struct Target {
    did: String,
    rkey: String,
    cid: Option<String>,
}

fn parse_target(ctx: &Ctx, target: &str) -> Result<Target> {
    if let Some(rest) = target.strip_prefix("at://") {
        let parts: Vec<&str> = rest.split('/').collect();
        let [did, collection, rkey] = parts[..] else {
            bail!("{target} is not a signature record URI");
        };
        if collection != COLLECTION {
            bail!("{target} is not in {COLLECTION}");
        }
        Did::parse(did, ctx.mode).map_err(|error| anyhow!("{did}: {error}"))?;
        return Ok(Target {
            did: did.to_owned(),
            rkey: rkey.to_owned(),
            cid: None,
        });
    }
    let kind = crate::git::run(&["cat-file", "-t", target]).with_context(|| {
        format!("{target} is neither an at:// URI nor an object in this repository")
    })?;
    let raw = std::process::Command::new("git")
        .args(["cat-file", kind.trim(), target])
        .output()
        .context("running git")?
        .stdout;
    let extracted = match kind.trim() {
        "commit" => extract_commit(&raw, tartan_atproto::signature::git::HashAlgorithm::Sha1)
            .map_err(|_| anyhow!("{target} carries more than one signature"))?,
        "tag" => extract_tag(&raw),
        other => bail!("{target} is a {other}, not a commit or tag"),
    };
    let signature = extracted
        .signature
        .ok_or_else(|| anyhow!("{target} is not signed"))?;
    match classify(&signature, ctx.mode) {
        SignatureClass::Tartan(block) => Ok(Target {
            did: block.did.as_str().to_owned(),
            rkey: block.rkey.as_str().to_owned(),
            cid: Some(block.cid.to_string()),
        }),
        _ => bail!("{target} does not carry a Tartan signature"),
    }
}

/// `tt sign revoke`.
async fn revoke_signature(ctx: &Ctx, args: RevokeArgs) -> Result<i32> {
    let target = parse_target(ctx, &args.target)?;
    let uri = format!("at://{}/{COLLECTION}/{}", target.did, target.rkey);

    // A record still queued was never published: drop it, no authorization needed.
    let queue = Queue::new(ctx, &target.did);
    {
        let lock = queue.lock()?;
        let entries = queue.read(&lock)?;
        if entries.iter().any(|entry| entry.rkey == target.rkey) {
            let kept: Vec<_> = entries
                .into_iter()
                .filter(|entry| entry.rkey != target.rkey)
                .collect();
            queue.rewrite(&lock, &kept)?;
            println!("Removed {uri} from the signing queue; it was never published.");
            return Ok(0);
        }
    }

    let config = Config::load(&ctx.dirs.config)?;
    let origin = forge::choose(ctx, args.forge.as_deref(), &config)?;
    let client = match args.client {
        Some(choice) => account::client_spec(Some(choice), ""),
        None => match &origin {
            Some(forge) => ClientSpec::Forge {
                forge: forge.clone(),
            },
            None => bail!("no forge configured: pass `--forge`, or `--client localhost`"),
        },
    };
    let mut session = oauth::login(oauth::LoginRequest {
        ctx,
        who: Who::Did(target.did.clone()),
        client,
        scope: oauth::DELETE_SCOPE,
        port: args.port,
        no_open: args.no_open,
    })
    .await?;
    let resolver = ctx.resolver()?;
    let cid = match target.cid.clone() {
        Some(cid) => Some(cid),
        None => match pds::get_record(&resolver, &session.pds, &target.did, &target.rkey).await {
            pds::Lookup::Present(cid) => Some(cid),
            _ => None,
        },
    };
    let deleted = pds::delete_record(&resolver, &mut session, &target.rkey).await;
    match oauth::revoke_session(ctx, &session).await {
        Ok(_) => {}
        Err(error) => eprintln!("tt: the one-off authorization could not be revoked: {error:#}"),
    }
    match deleted {
        WriteOutcome::Written(_) => println!("Deleted {uri}."),
        other => bail!("the record could not be deleted: {other:?}"),
    }

    let Some(origin) = origin else {
        return Ok(0);
    };
    let forge = Forge::new(ctx, &origin)?;
    let Some(credential) = forge.credential().cloned() else {
        println!("Not signed in to a forge; run `tt login` to withdraw it on a forge too.");
        return Ok(0);
    };
    let me = forge.me().await.ok();
    let own = me
        .as_ref()
        .and_then(|me| me.did.as_deref())
        .is_some_and(|did| did == target.did);
    let path = format!(
        "/-/api/people/{}/signatures/{}/refresh",
        target.did, target.rkey
    );
    let body = (own && !credential.is_token())
        .then(|| json!({"withdraw": true, "cid": cid.clone().unwrap_or_default()}));
    match forge.send(Method::POST, &path, body.as_ref()).await {
        Ok(_) if body.is_some() => println!("Withdrew the signature on {origin}."),
        Ok(_) => println!("Asked {origin} to re-prove the record."),
        Err(error) if body.is_none() && error.status == Some(reqwest::StatusCode::NOT_FOUND) => {
            println!("{origin} does not rely on that record; nothing to re-prove there.");
        }
        Err(error) => {
            eprintln!("tt: {origin} could not be told: {error}");
            return Ok(1);
        }
    }
    if own && credential.is_token() {
        println!(
            "Withdrawing a signature on the forge needs a session: run `tt login` and then \
             `tt sign revoke {}` again.",
            args.target
        );
    }
    Ok(0)
}

/// `tt sign recheck`.
async fn recheck_signature(ctx: &Ctx, args: RecheckArgs) -> Result<i32> {
    let target = parse_target(ctx, &args.commit)?;
    let sha = crate::git::run(&[
        "rev-parse",
        "--verify",
        &format!("{}^{{object}}", args.commit),
    ])?
    .trim()
    .to_owned();
    // Publish the record first if it is still queued.
    let queue = Queue::new(ctx, &target.did);
    {
        let lock = queue.lock()?;
        if queue
            .read(&lock)?
            .iter()
            .any(|entry| entry.rkey == target.rkey)
        {
            let session = SigningSession::load(ctx, &target.did)?;
            let deadline = tokio::time::Instant::now() + queue::FLUSH_TIME;
            let drained = queue::drain(ctx, &queue, &lock, session, deadline).await?;
            if queue
                .read(&lock)?
                .iter()
                .any(|entry| entry.rkey == target.rkey)
            {
                bail!(
                    "the commit's record is still queued ({}); publish it first with `tt sign \
                     --flush`",
                    drained.reason.as_deref().unwrap_or("unknown reason")
                );
            }
            println!("Published the commit's signature record.");
        }
    }
    let config = Config::load(&ctx.dirs.config)?;
    let Some(origin) = forge::choose(ctx, args.forge.as_deref(), &config)? else {
        bail!("no forge configured: run `tt login --forge <url>`");
    };
    let forge = Forge::new(ctx, &origin)?;
    forge.require_credential()?;
    let Some((org, repo)) = args.repo.split_once('/') else {
        bail!("--repo is <org>/<repo>");
    };
    let path = format!("/-/api/orgs/{org}/repos/{repo}/commits/{sha}/signature/recheck");
    match forge.send(Method::POST, &path, None).await {
        Ok(_) => {
            println!("{origin} is checking {sha} again.");
            Ok(0)
        }
        Err(error) if error.status == Some(reqwest::StatusCode::NOT_FOUND) => {
            eprintln!(
                "tt: this forge has no unverifiable result of yours for that commit ({sha} in \
                 {})",
                args.repo
            );
            Ok(1)
        }
        Err(error) if error.status == Some(reqwest::StatusCode::TOO_MANY_REQUESTS) => {
            eprintln!(
                "tt: too many re-checks; try again in {} seconds",
                error.retry_after.unwrap_or(60)
            );
            Ok(1)
        }
        Err(error) => Err(error.into()),
    }
}
