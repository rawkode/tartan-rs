//! `tt login`, `tt logout`, `tt status`, `tt token` and `tt identity resolve` (design D16).

use std::io::Read as _;

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tartan_atproto::did::Did;
use tartan_atproto::handle::{Handle, HandleVerdict};
use tartan_identity_api::{CLI_SESSION_PREFIX, MAX_DEVICE_CHARS, TOKEN_PREFIX};

use crate::cli::{
    ClientChoice, LoginArgs, LogoutArgs, StatusArgs, TokenArgs, TokenCommand, TokenCreateArgs,
};
use crate::config::{Config, Offered};
use crate::credentials::Store;
use crate::ctx::Ctx;
use crate::forge::{self, Forge, ForgeCredential, Me};
use crate::loopback::{self, Loopback};
use crate::oauth::{self, ClientSpec, SigningSession, Who};
use crate::queue::Queue;
use crate::sign::format_time;
use crate::state::{self, now_ms};

fn random_base64url(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| anyhow!("the operating system's RNG failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The device name the approval page shows: the host name, at most 64 characters.
fn device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_CHARS)
        .collect();
    if name.is_empty() {
        "this computer".to_owned()
    } else {
        name
    }
}

/// `tt login`.
pub async fn login(ctx: &Ctx, args: LoginArgs) -> Result<i32> {
    let mut config = Config::load(&ctx.dirs.config)?;
    let Some(origin) = forge::choose(ctx, args.forge.as_deref(), &config)? else {
        bail!(
            "no forge configured: pass `--forge <url>` (for example `tt login --forge https://forge.example`)"
        );
    };
    if args.with_token {
        return login_with_token(ctx, &mut config, &origin, args.set_default).await;
    }

    // 1. The loopback hand-off.
    let listener = Loopback::bind(args.port).await?;
    let state_value = random_base64url(32)?;
    let verifier = random_base64url(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let device = device_name();
    let url = format!(
        "{origin}/#/-/cli-login?port={}&state={state_value}&challenge={challenge}&device={}",
        listener.port(),
        percent(&device)
    );
    loopback::present(&url, &format!("approve tt on {device}"), args.no_open);
    let query = listener
        .wait("/forge", &loopback::page("tt is signed in to the forge."))
        .await?;
    drop(listener);
    if query.get("state") != Some(&state_value) {
        bail!("the forge's redirect carried the wrong state; nothing was stored");
    }
    if let Some(error) = query.get("error") {
        bail!("the sign-in was not approved ({error})");
    }
    let code = query
        .get("code")
        .ok_or_else(|| anyhow!("the forge's redirect carried no code"))?;

    // 2. The code exchange.
    let anonymous = Forge::with_credential(&origin, None)?;
    let session = anonymous
        .send(
            Method::POST,
            "/-/auth/cli/token",
            Some(&json!({"code": code, "code_verifier": verifier})),
        )
        .await
        .map_err(|error| anyhow!("exchanging the sign-in code: {error}"))?;
    let secret = session["session"]
        .as_str()
        .filter(|secret| secret.starts_with(CLI_SESSION_PREFIX))
        .ok_or_else(|| anyhow!("the forge answered no CLI session"))?
        .to_owned();
    let expires_at = session["expires_at"].as_i64();
    let credential = ForgeCredential {
        kind: "cli".to_owned(),
        secret,
    };
    let signed_in = Forge::with_credential(&origin, Some(credential.clone()))?;
    let me = signed_in
        .me()
        .await
        .map_err(|error| anyhow!("asking the forge who this is: {error}"))?;
    let did = me
        .did
        .clone()
        .ok_or_else(|| anyhow!("the forge does not recognise the new session"))?;
    let written = forge::store(ctx, &origin, &credential, &me, expires_at)?;
    println!(
        "Signed in to {origin} as {} ({did}).",
        me.handle
            .as_deref()
            .map_or_else(|| did.clone(), |handle| format!("@{handle}"))
    );
    eprintln!("The forge session is stored {}.", Store::describe(&written));
    report_default_forge(&mut config, &origin, args.set_default)?;

    // 3. First run: the signing login for the same DID.
    if args.no_signing {
        return Ok(0);
    }
    if SigningSession::load(ctx, &did)?.is_some() {
        println!("A signing session for {did} is already stored.");
        return Ok(0);
    }
    let client = client_spec(args.client, &origin);
    match signing_login(
        ctx,
        &mut config,
        Who::Did(did.clone()),
        client,
        args.port,
        args.no_open,
        args.set_default,
    )
    .await
    {
        Ok(()) => Ok(0),
        Err(error) => {
            eprintln!(
                "tt: signing is not set up: run `tt sign --login` ({error:#}). The forge session \
                 is kept."
            );
            Ok(1)
        }
    }
}

fn report_default_forge(config: &mut Config, origin: &str, set_default: bool) -> Result<()> {
    match config.offer_forge(origin, set_default)? {
        Offered::Set => println!("{origin} is the default forge."),
        Offered::Unchanged => {}
        Offered::Kept(default) => println!(
            "Signed in to {origin}; the default forge is still {default} (`tt login --forge \
             {origin} --set-default` to change it)."
        ),
    }
    Ok(())
}

async fn login_with_token(
    ctx: &Ctx,
    config: &mut Config,
    origin: &str,
    set_default: bool,
) -> Result<i32> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the access token from standard input")?;
    let secret = text.trim().to_owned();
    if !secret.starts_with(TOKEN_PREFIX) {
        bail!("standard input does not hold a {TOKEN_PREFIX} access token");
    }
    let credential = ForgeCredential {
        kind: "token".to_owned(),
        secret,
    };
    let me = Forge::with_credential(origin, Some(credential.clone()))?
        .me()
        .await
        .map_err(|error| anyhow!("checking the token: {error}"))?;
    if me.did.is_none() {
        bail!("the forge does not accept this token");
    }
    let written = forge::store(ctx, origin, &credential, &me, None)?;
    println!(
        "Signed in to {origin} with an access token for {}.",
        me.did.as_deref().unwrap_or_default()
    );
    eprintln!("The token is stored {}.", Store::describe(&written));
    report_default_forge(config, origin, set_default)?;
    Ok(0)
}

/// The client a signing login uses.
pub fn client_spec(choice: Option<ClientChoice>, forge: &str) -> ClientSpec {
    match choice {
        Some(ClientChoice::Localhost) => ClientSpec::Localhost,
        None => ClientSpec::Forge {
            forge: forge.to_owned(),
        },
    }
}

/// `tt sign --login`'s work: signs in, stores the session, applies the default signing DID
/// rule and never touches the default forge.
pub async fn signing_login(
    ctx: &Ctx,
    config: &mut Config,
    who: Who,
    client: ClientSpec,
    port: Option<u16>,
    no_open: bool,
    set_default: bool,
) -> Result<()> {
    let session = oauth::login(oauth::LoginRequest {
        ctx,
        who,
        client,
        scope: oauth::CREATE_SCOPE,
        port,
        no_open,
    })
    .await?;
    let written = session.save(ctx)?;
    let did = session.did.clone();
    println!(
        "Signing is set up for {did} (client {}); the session lasts about two weeks, until {}.",
        session.client.client_id(),
        format_time(session.created_at + state::SIGNING_SESSION_MS)
    );
    eprintln!(
        "The signing session is stored {}.",
        Store::describe(&written)
    );
    match config.offer_signing_did(&did, set_default)? {
        Offered::Set => println!("{did} is the default signing DID."),
        Offered::Unchanged => {}
        Offered::Kept(default) => {
            println!("The default signing DID is still {default} (`--set-default` to change it).")
        }
    }
    Ok(())
}

/// `tt logout`.
pub async fn logout(ctx: &Ctx, args: LogoutArgs) -> Result<i32> {
    let config = Config::load(&ctx.dirs.config)?;
    let Some(origin) = forge::choose(ctx, args.forge.as_deref(), &config)? else {
        bail!("no forge configured: pass `--forge <url>`");
    };
    let client = Forge::new(ctx, &origin)?;
    let mut code = 0;
    let index = state::index(&ctx.dirs)?;
    let did = index.forges.get(&origin).map(|entry| entry.did.clone());
    match client.credential() {
        None => println!("Not signed in to {origin}."),
        Some(credential) if credential.is_token() => {
            println!("Forgot the access token for {origin} (revoke it on the forge if it leaked).");
        }
        Some(_) => match client
            .send(Method::DELETE, "/-/api/sessions/current", None)
            .await
        {
            Ok(_) => println!("Ended the forge session on {origin}."),
            Err(error) if error.status == Some(reqwest::StatusCode::UNAUTHORIZED) => {
                println!("The forge session on {origin} had already ended.");
            }
            Err(error) => {
                eprintln!(
                    "tt: the forge session could not be ended ({error}); the local credential is deleted anyway"
                );
                code = 1;
            }
        },
    }
    forge::forget(ctx, &origin)?;

    // The signing session of the same DID.
    if let Some(did) = did.filter(|did| !did.is_empty())
        && let Some(session) = SigningSession::load(ctx, &did).ok().flatten()
    {
        match oauth::revoke_session(ctx, &session).await {
            Ok(true) => {
                println!("Revoked the signing session of {did} at its authorization server.")
            }
            Ok(false) => println!(
                "The authorization server of {did} offers no revocation; the signing session is forgotten."
            ),
            Err(error) => {
                eprintln!(
                    "tt: the signing session could not be revoked ({error:#}); it is deleted locally"
                );
                code = 1;
            }
        }
        oauth::forget(ctx, &did)?;
    }
    println!(
        "Local credentials are deleted. If a credential may be compromised, follow `tt sign revoke --help`."
    );
    Ok(code)
}

/// `tt status`.
pub async fn status(ctx: &Ctx, args: StatusArgs) -> Result<i32> {
    let config = Config::load(&ctx.dirs.config)?;
    let index = state::index(&ctx.dirs)?;
    let status = state::status(&ctx.dirs)?;
    let now = now_ms();
    let origin = forge::choose(ctx, args.forge.as_deref(), &config)?;
    let mut forge_json = Value::Null;
    let mut forge_did = None;
    let mut lines = Vec::new();
    match &origin {
        None => lines.push("Forge: none configured (`tt login --forge <url>`)".to_owned()),
        Some(origin) => {
            let client = Forge::new(ctx, origin)?;
            match client.credential() {
                None => lines.push(format!("Forge: {origin}, not signed in (`tt login`)")),
                Some(credential) => {
                    let kind = if credential.is_token() {
                        "access token"
                    } else {
                        "CLI session"
                    };
                    match client.me().await {
                        Ok(Me {
                            did: Some(did),
                            handle,
                            owner,
                            rights,
                            ..
                        }) => {
                            lines.push(format!(
                                "Forge: {origin}, signed in as {} ({did}) with a {kind}{}; rights: {}",
                                handle.as_deref().map_or_else(|| did.clone(), |h| format!("@{h}")),
                                if owner { ", an owner" } else { "" },
                                if rights.is_empty() { "none".to_owned() } else { rights.join(", ") }
                            ));
                            forge_json = json!({"url": origin, "did": did, "handle": handle, "kind": kind, "owner": owner, "rights": rights});
                            forge_did = Some(did);
                        }
                        Ok(_) => lines.push(format!(
                            "Forge: {origin}, the stored {kind} is no longer accepted (`tt login`)"
                        )),
                        Err(error) => {
                            let indexed = index.forges.get(origin);
                            lines.push(format!(
                                "Forge: {origin}, {kind} for {} ({error})",
                                indexed.map_or("unknown", |e| e.did.as_str())
                            ));
                            forge_did = indexed.map(|entry| entry.did.clone());
                        }
                    }
                }
            }
        }
    }
    if let Some(default) = &config.file.default_forge
        && origin.as_deref() != Some(default.as_str())
    {
        lines.push(format!("Default forge: {default}"));
    }

    let mut signing_json = Vec::new();
    if index.signing.is_empty() {
        lines.push("Signing: no signing session (`tt sign --login`)".to_owned());
    }
    for (did, entry) in &index.signing {
        let ends = entry.ends_at();
        let mut line = format!(
            "Signing: {did}{} via {} (client {}), session ends by {}",
            if config.file.default_signing_did.as_deref() == Some(did.as_str()) {
                " (default)"
            } else {
                ""
            },
            entry.issuer,
            entry.client_id,
            format_time(ends)
        );
        if ends - now < 2 * 24 * 60 * 60 * 1000 {
            line.push_str(" — fewer than two days remain: run `tt sign --login` soon");
        }
        lines.push(line);
        signing_json.push(json!({"did": did, "client_id": entry.client_id, "forge": entry.forge, "issuer": entry.issuer, "ends_at": ends, "default": config.file.default_signing_did.as_deref() == Some(did.as_str())}));
    }
    if let (Some(forge_did), Some(default)) = (&forge_did, &config.file.default_signing_did)
        && forge_did != default
    {
        lines.push(format!(
            "Warning: the forge session's DID {forge_did} is not the default signing DID {default}; covered refs judge the signing DID"
        ));
    }

    let mut queue_json = Vec::new();
    for did in Queue::queued_dids(ctx) {
        let queue = Queue::new(ctx, &did);
        let count = queue
            .read_unlocked()
            .map(|entries| entries.len())
            .unwrap_or_default();
        let window = queue.windows().open(now);
        let mut line = format!("Queued: {count} signature record(s) of {did} wait to be published");
        if let Some((until, reason)) = &window {
            line.push_str(&format!(
                " (paused until {}: {reason})",
                format_time(*until)
            ));
        }
        if count > 0 || window.is_some() {
            lines.push(line);
        }
        queue_json.push(json!({"did": did, "count": count, "paused_until": window.as_ref().map(|w| w.0), "reason": window.map(|w| w.1)}));
    }
    for reported in &status.reported {
        lines.push(format!(
            "Reported: the queued record at://{}/{}/{} was replaced ({}); its signature cannot verify",
            reported.did,
            tartan_atproto::signature::COLLECTION,
            reported.rkey,
            reported.found
        ));
    }
    if let Some(failure) = &status.last_failure {
        lines.push(format!(
            "Last signing failure ({}): {}",
            format_time(failure.at),
            failure.message
        ));
    }
    if args.json {
        let value = json!({
            "forge": forge_json,
            "default_forge": config.file.default_forge,
            "default_signing_did": config.file.default_signing_did,
            "signing": signing_json,
            "queue": queue_json,
            "reported": status.reported,
            "last_failure": status.last_failure,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        for line in lines {
            println!("{line}");
        }
    }
    Ok(0)
}

/// `tt token …`.
pub async fn token(ctx: &Ctx, args: TokenArgs) -> Result<i32> {
    let config = Config::load(&ctx.dirs.config)?;
    let Some(origin) = forge::choose(ctx, args.forge.as_deref(), &config)? else {
        bail!("no forge configured: run `tt login --forge <url>`");
    };
    let client = Forge::new(ctx, &origin)?;
    let credential = client.require_credential()?;
    forge::require_session(credential)?;
    match args.command {
        TokenCommand::Create(create) => {
            let created = client
                .send(Method::POST, "/-/api/tokens", Some(&token_request(&create)))
                .await?;
            print_token(&created);
        }
        TokenCommand::List => {
            let tokens = client.send(Method::GET, "/-/api/tokens", None).await?;
            print_tokens(&tokens);
        }
        TokenCommand::Revoke { id } => {
            client
                .send(
                    Method::DELETE,
                    &format!("/-/api/tokens/{}", percent(&id)),
                    None,
                )
                .await?;
            println!("Revoked token {id}.");
        }
    }
    Ok(0)
}

/// The JSON body of a token request.
pub fn token_request(create: &TokenCreateArgs) -> Value {
    json!({
        "name": create.name,
        "scopes": create.scopes,
        "repos": create.repos,
        "expires_in_days": create.expires_in_days,
    })
}

/// Prints a new token, its secret once.
pub fn print_token(created: &Value) {
    let token = &created["token"];
    println!(
        "Created token {} ({}) for {}, scopes {}, expires {}.",
        token["id"].as_str().unwrap_or_default(),
        token["name"].as_str().unwrap_or_default(),
        token["did"].as_str().unwrap_or_default(),
        token["scopes"],
        token["expires_at"]
            .as_i64()
            .map(format_time)
            .unwrap_or_default()
    );
    println!("Its secret, shown only now:");
    println!("{}", created["secret"].as_str().unwrap_or_default());
}

/// Prints a token list.
pub fn print_tokens(tokens: &Value) {
    let Some(list) = tokens.as_array() else {
        return;
    };
    if list.is_empty() {
        println!("No tokens.");
    }
    for token in list {
        let state = if token["revoked_at"].is_i64() {
            "revoked".to_owned()
        } else if token["expires_at"].as_i64().is_some_and(|at| at < now_ms()) {
            "expired".to_owned()
        } else {
            format!(
                "expires {}",
                token["expires_at"]
                    .as_i64()
                    .map(format_time)
                    .unwrap_or_default()
            )
        };
        println!(
            "{}  {}  {}  {}",
            token["id"].as_str().unwrap_or_default(),
            token["name"].as_str().unwrap_or_default(),
            token["scopes"],
            state
        );
    }
}

/// `tt identity resolve`.
pub async fn resolve(ctx: &Ctx, who: &str) -> Result<i32> {
    let resolver = ctx.resolver()?;
    let text = who
        .trim()
        .trim_start_matches("at://")
        .trim_start_matches('@');
    let did = if text.starts_with("did:") {
        Did::parse(text, ctx.mode).map_err(|error| anyhow!("{text}: {error}"))?
    } else {
        let handle = Handle::parse(&text.to_ascii_lowercase(), ctx.mode)
            .map_err(|_| anyhow!("{text:?} is neither a handle nor a DID"))?;
        resolver
            .resolve_verified_handle(&handle)
            .await
            .map_err(|error| anyhow!("@{}: {error}", handle.as_str()))?
    };
    let resolved = resolver
        .resolve_did(&did)
        .await
        .map_err(|error| anyhow!("{did}: {error}"))?;
    let Some(identity) = resolved.identity else {
        println!("DID:    {did}");
        println!("Status: deactivated (tombstoned)");
        return Ok(1);
    };
    let handle = match resolver.verify_handle(&identity).await {
        HandleVerdict::Verified(handle) => format!("@{} (verified)", handle.as_str()),
        HandleVerdict::Invalid(reason) => format!("handle.invalid ({reason})"),
        HandleVerdict::Unavailable(reason) => format!(
            "{} (not verified now: {reason})",
            identity.handle.as_deref().unwrap_or("none")
        ),
    };
    println!("DID:    {did}");
    println!("Handle: {handle}");
    println!("PDS:    {}", identity.pds.as_deref().unwrap_or("none"));
    Ok(0)
}
