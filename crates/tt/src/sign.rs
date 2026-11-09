//! Signing a Git payload (design D7, *Signing*).
//!
//! `tt` reads the payload (at most 16 MiB, always to the end of standard input), refuses unless
//! `gpg.format` is `x509` in Git's environment, chooses the account, builds the record with a
//! fresh salt, writes it alone in one `applyWrites` (or queues it), writes the six-line
//! signature to standard output and `BEGIN_SIGNING` then `SIG_CREATED` to the status
//! descriptor, and then drains the queue if its lock is free. It never prompts and never opens
//! a browser: Git shows its standard error only when signing fails.

use std::io::Read as _;

use anyhow::{Result, anyhow, bail};
use tartan_atproto::Mode;
use tartan_atproto::did::Did;
use tartan_atproto::signature::block::SignatureBlock;
use tartan_atproto::signature::record::{Salt, SignatureRecord, created_at_now};
use tartan_atproto::signature::tid::Tid;

use crate::config::{Config, Offline};
use crate::ctx::Ctx;
use crate::gpg::StatusWriter;
use crate::oauth::{self, RefreshFailure, SigningSession};
use crate::pds::{self, Create, WRITE_TIMEOUT, WriteOutcome};
use crate::queue::{self, Entry, Queue};
use crate::state;

/// The largest payload `tt` signs.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// The message for a repository not set up for `tt` as its x509 program.
pub const FORMAT_MESSAGE: &str =
    "tt is Git's x509 program; set gpg.format=x509 (run `tt sign --setup`)";

/// Reads standard input to its end, keeping at most [`MAX_PAYLOAD`] bytes. `None` when it was
/// longer.
pub fn read_payload() -> Option<Vec<u8>> {
    let mut payload = Vec::new();
    let mut stdin = std::io::stdin().lock();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut too_long = false;
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if payload.len() + read > MAX_PAYLOAD {
                    too_long = true;
                } else if !too_long {
                    payload.extend_from_slice(&chunk[..read]);
                }
            }
        }
    }
    (!too_long).then_some(payload)
}

/// Chooses the signing account for Git's key argument (design D16).
pub fn choose_account(ctx: &Ctx, config: &Config, key: &str) -> Result<String> {
    let key = key.trim();
    if key.starts_with("did:") {
        Did::parse(key, ctx.mode).map_err(|error| anyhow!("{key}: {error}"))?;
        return Ok(key.to_owned());
    }
    if let Some(did) = &config.file.default_signing_did {
        return Ok(did.clone());
    }
    let index = state::index(&ctx.dirs)?;
    let mut dids = index.signing.keys();
    match (dids.next(), dids.next()) {
        (Some(only), None) => Ok(only.clone()),
        (None, _) => bail!(
            "no signing session — run `tt sign --login` (and `tt sign --setup` to set \
             user.signingKey)"
        ),
        (Some(_), Some(_)) => bail!(
            "several signing sessions and no default signing DID; set user.signingKey to the DID \
             to sign as (`tt sign --setup --did <DID>`)"
        ),
    }
}

/// Git's sign call: returns the process exit code.
pub async fn git_sign(ctx: &Ctx, status_fd: i32, key: &str) -> i32 {
    let status = StatusWriter::new(status_fd);
    let payload = read_payload();
    match crate::git::config_get("gpg.format") {
        Ok(Some(format)) if format == "x509" => {}
        _ => {
            eprintln!("{FORMAT_MESSAGE}");
            state::record_failure(&ctx.dirs, None, FORMAT_MESSAGE);
            return 1;
        }
    }
    let Some(payload) = payload else {
        eprintln!("tt: the payload is larger than 16 MiB; tt does not sign it");
        return 1;
    };
    match sign(ctx, key, &payload).await {
        Ok(signed) => {
            print!("{}", signed.block.write());
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            status.line("BEGIN_SIGNING");
            status.line(&format!(
                "SIG_CREATED D 0 8 00 {} {}",
                signed.created_unix,
                signed.block.uri()
            ));
            if let Some(note) = signed.queued {
                state::record_failure(&ctx.dirs, Some(signed.block.did.as_str()), &note);
            } else {
                state::clear_failure(&ctx.dirs);
            }
            0
        }
        Err(error) => {
            let message = format!("tt: {error:#}");
            eprintln!("{message}");
            state::record_failure(&ctx.dirs, None, &message);
            1
        }
    }
}

/// A signature made.
pub struct Signed {
    /// The block.
    pub block: SignatureBlock,
    /// The signing time (Unix seconds).
    pub created_unix: i64,
    /// Why the record was queued, when it was.
    pub queued: Option<String>,
}

/// Signs `payload` as the account `key` chooses.
pub async fn sign(ctx: &Ctx, key: &str, payload: &[u8]) -> Result<Signed> {
    let config = Config::load(&ctx.dirs.config)?;
    let did = choose_account(ctx, &config, key)?;
    let session = SigningSession::load(ctx, &did)?.ok_or_else(|| {
        anyhow!("no signing session for {did} — run `tt sign --login --handle <handle or DID>`")
    })?;

    let salt = Salt::generate().map_err(|_| anyhow!("the operating system's RNG failed"))?;
    let created_at = created_at_now();
    let created_unix = jacquard_common::deps::chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|time| time.timestamp())
        .unwrap_or_default();
    let record = SignatureRecord::for_git(payload, &salt, created_at);
    let cid = record.cid();
    let rkey = Tid::now();
    let parsed_did = Did::parse(&did, mode_for(ctx))?;
    let block = SignatureBlock {
        did: parsed_did,
        rkey: rkey.clone(),
        cid,
        salt,
    };
    let entry = Entry {
        did: did.clone(),
        rkey: rkey.as_str().to_owned(),
        record: record.to_json(),
        cid: cid.to_string(),
        state: "unconfirmed".to_owned(),
        queued_at: state::now_ms(),
    };
    let queue = Queue::new(ctx, &did);
    let offline = config.file.sign.offline;
    let started = tokio::time::Instant::now();

    // A rate-limit or back-off window: queue at once, without contacting the PDS.
    if let Some((until, reason)) = queue.windows().open(state::now_ms()) {
        let note = format!(
            "the record for {} was queued without contacting the PDS ({reason}, until {})",
            block.uri(),
            format_time(until)
        );
        return queue_or_fail(&queue, &entry, offline, note).map(|queued| Signed {
            block,
            created_unix,
            queued: Some(queued),
        });
    }

    let session = match oauth::fresh_session(ctx, session).await {
        Ok(session) => session,
        Err(RefreshFailure::Authorization(message)) => bail!("{message}"),
        Err(RefreshFailure::Transient(message)) => {
            let _ = queue.back_off(&message);
            let note = format!("the record for {} was queued: {message}", block.uri());
            return queue_or_fail(&queue, &entry, offline, note).map(|queued| Signed {
                block,
                created_unix,
                queued: Some(queued),
            });
        }
    };
    let resolver = ctx.resolver()?;
    let create = Create {
        rkey: entry.rkey.clone(),
        value: entry.record.clone(),
    };
    let mut session = session;
    let mut outcome = pds::apply_writes(
        &resolver,
        &mut session,
        std::slice::from_ref(&create),
        WRITE_TIMEOUT,
    )
    .await;
    if let WriteOutcome::Unauthorized(_) = outcome {
        // The access token was refused before its stated expiry: refresh once and retry.
        session = match oauth::force_refresh(ctx, session).await {
            Ok(session) => session,
            Err(RefreshFailure::Authorization(message)) => bail!("{message}"),
            Err(RefreshFailure::Transient(message)) => {
                let _ = queue.back_off(&message);
                let note = format!("the record for {} was queued: {message}", block.uri());
                return queue_or_fail(&queue, &entry, offline, note).map(|queued| Signed {
                    block,
                    created_unix,
                    queued: Some(queued),
                });
            }
        };
        let remaining = WRITE_TIMEOUT.saturating_sub(started.elapsed());
        outcome = pds::apply_writes(
            &resolver,
            &mut session,
            std::slice::from_ref(&create),
            remaining,
        )
        .await;
    }
    let _ = session.save(ctx);
    let queued = match outcome {
        WriteOutcome::Written(results) => {
            let expected = (block.uri(), entry.cid.clone());
            if results.first() != Some(&expected) {
                bail!(
                    "the PDS stored the record as {:?}, not {} with CID {}; the signature would \
                     not verify",
                    results.first(),
                    expected.0,
                    expected.1
                );
            }
            None
        }
        WriteOutcome::RateLimited(reset) => {
            let _ = queue.rate_limited(reset);
            Some(format!(
                "the record for {} was queued: the PDS's rate limit",
                block.uri()
            ))
        }
        WriteOutcome::Unreachable(reason) => {
            let _ = queue.back_off(&reason);
            Some(format!(
                "the record for {} was queued: {reason}",
                block.uri()
            ))
        }
        WriteOutcome::ServerError(reason) => Some(format!(
            "the record for {} was queued: {reason}",
            block.uri()
        )),
        WriteOutcome::Unauthorized(reason) => {
            bail!("{reason}; the signing session was refused — run `tt sign --login`")
        }
        WriteOutcome::Refused(reason) => bail!("the PDS refused the signature record: {reason}"),
    };
    if let Some(note) = queued {
        return queue_or_fail(&queue, &entry, offline, note).map(|queued| Signed {
            block,
            created_unix,
            queued: Some(queued),
        });
    }

    // Written: drain what waits, with what remains of the time, if nobody else is.
    if let Ok(Some(lock)) = queue.try_lock()
        && queue.read(&lock).is_ok_and(|entries| !entries.is_empty())
    {
        let deadline = started + WRITE_TIMEOUT;
        let _ = queue::drain(ctx, &queue, &lock, Some(session), deadline).await;
    }
    Ok(Signed {
        block,
        created_unix,
        queued: None,
    })
}

fn queue_or_fail(queue: &Queue, entry: &Entry, offline: Offline, note: String) -> Result<String> {
    if offline == Offline::Fail {
        bail!("{note}; sign.offline is \"fail\", so signing failed");
    }
    let lock = queue.lock()?;
    queue.append(&lock, entry)?;
    Ok(note)
}

fn mode_for(ctx: &Ctx) -> Mode {
    ctx.mode
}

/// A Unix-millisecond time as `YYYY-MM-DD HH:MM UTC`.
pub fn format_time(ms: i64) -> String {
    jacquard_common::deps::chrono::DateTime::from_timestamp_millis(ms)
        .map(|time| time.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ms.to_string())
}
