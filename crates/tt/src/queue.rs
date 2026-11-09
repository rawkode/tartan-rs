//! The signing queue (design D7, *Offline and rate limits*).
//!
//! A record that cannot be published now is appended to `$XDG_STATE_HOME/tt/queue/<did>.jsonl`
//! (mode 0600) and its signature is still returned, since its CID is computed locally. Every
//! queued record is *unconfirmed* until a read says otherwise: a write that timed out may have
//! been committed, and a plain create of a key that exists fails the whole `applyWrites`. So a
//! drain first reconciles each record with `getRecord`, then sends the absent ones in calls of
//! at most 200 creates, then rewrites the file.
//!
//! `<did>.state.json` holds the rate-limit window (until the last 429's `ratelimit-reset`) and
//! the back-off window (two minutes after a connection failure or timeout); while either is
//! open, signing queues at once and no drain runs, so signing never waits inside Git.
//!
//! Every queue operation holds `<did>.lock` from reading the queue to rewriting it. Lock order:
//! the queue lock may be held while the refresh lock is taken, never the reverse.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tartan_atproto::resolver::VerifyingResolver;

use crate::ctx::Ctx;
use crate::oauth::{self, RefreshFailure, SigningSession};
use crate::paths;
use crate::pds::{self, Create, Lookup, WriteOutcome};
use crate::state::{self, Reported};

/// The most creates one `applyWrites` call carries (the reference PDS's limit).
pub const MAX_WRITES: usize = 200;
/// How long the back-off window stays open after a connection failure or timeout.
pub const BACKOFF_MS: i64 = 2 * 60 * 1000;

/// A queued record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The repository.
    pub did: String,
    /// The record key.
    pub rkey: String,
    /// The record as `applyWrites` sends it.
    pub record: Value,
    /// The record's CID, which the signature names.
    pub cid: String,
    /// Always `unconfirmed`: no read has shown it published.
    pub state: String,
    /// When it was queued (Unix milliseconds).
    pub queued_at: i64,
}

/// `<did>.state.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Windows {
    /// Until when the PDS's rate limit holds (Unix milliseconds).
    #[serde(default)]
    pub rate_limited_until: Option<i64>,
    /// Until when the back-off after a connection failure holds (Unix milliseconds).
    #[serde(default)]
    pub backoff_until: Option<i64>,
    /// Why the last window opened.
    #[serde(default)]
    pub reason: Option<String>,
}

impl Windows {
    /// The open window's end and reason, if one is open at `now`.
    pub fn open(&self, now: i64) -> Option<(i64, String)> {
        let reason = self.reason.clone().unwrap_or_default();
        match (self.rate_limited_until, self.backoff_until) {
            (Some(rate), Some(backoff)) if rate > now || backoff > now => {
                Some((rate.max(backoff), reason))
            }
            (Some(until), _) | (_, Some(until)) if until > now => Some((until, reason)),
            _ => None,
        }
    }
}

/// One DID's queue files.
pub struct Queue {
    did: String,
    dir: PathBuf,
    stem: String,
}

/// A held queue lock.
pub struct QueueLock {
    _file: File,
}

impl Queue {
    /// The queue of `did`.
    pub fn new(ctx: &Ctx, did: &str) -> Self {
        Queue {
            did: did.to_owned(),
            dir: ctx.dirs.queue(),
            stem: paths::file_name(did),
        }
    }

    /// The DIDs that have a queue file.
    pub fn queued_dids(ctx: &Ctx) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(ctx.dirs.queue()) else {
            return Vec::new();
        };
        let mut dids: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".jsonl")?;
                paths::from_file_name(stem)
            })
            .collect();
        dids.sort();
        dids
    }

    fn file(&self, suffix: &str) -> PathBuf {
        self.dir.join(format!("{}.{suffix}", self.stem))
    }

    /// Takes the queue lock, waiting for it.
    pub fn lock(&self) -> Result<QueueLock> {
        let file = paths::lock_file(&self.file("lock"))?;
        file.lock().context("locking the signing queue")?;
        Ok(QueueLock { _file: file })
    }

    /// Takes the queue lock only if it is free now.
    pub fn try_lock(&self) -> Result<Option<QueueLock>> {
        let file = paths::lock_file(&self.file("lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(QueueLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(error).context("locking the signing queue")
            }
        }
    }

    /// The queued records (under the lock).
    pub fn read(&self, _lock: &QueueLock) -> Result<Vec<Entry>> {
        self.read_unlocked()
    }

    /// The queued records, for display.
    pub fn read_unlocked(&self) -> Result<Vec<Entry>> {
        let path = self.file("jsonl");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.with_context(|| format!("reading {}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            // A line that does not parse (a torn write) is skipped rather than blocking every
            // later drain.
            if let Ok(entry) = serde_json::from_str::<Entry>(&line) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Appends `entry` (under the lock).
    pub fn append(&self, _lock: &QueueLock, entry: &Entry) -> Result<()> {
        paths::private_dir(&self.dir)?;
        let mut file = paths::open_private(&self.file("jsonl"), false)?;
        let mut line = serde_json::to_vec(entry).context("encoding a queued record")?;
        line.push(b'\n');
        file.write_all(&line)
            .context("appending to the signing queue")?;
        file.sync_all().ok();
        Ok(())
    }

    /// Rewrites the queue with `entries` (under the lock); an empty queue removes the file.
    pub fn rewrite(&self, _lock: &QueueLock, entries: &[Entry]) -> Result<()> {
        let path = self.file("jsonl");
        if entries.is_empty() {
            return match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
            };
        }
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend(serde_json::to_vec(entry).context("encoding a queued record")?);
            bytes.push(b'\n');
        }
        paths::write_private(&path, &bytes)
    }

    /// The rate-limit and back-off windows.
    pub fn windows(&self) -> Windows {
        std::fs::read(self.file("state.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_windows(&self, windows: &Windows) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(windows).context("encoding the windows")?;
        paths::write_private(&self.file("state.json"), &bytes)
    }

    /// Opens the rate-limit window until `reset` (Unix seconds; an hour when not given).
    pub fn rate_limited(&self, reset: Option<i64>) -> Result<()> {
        let now = state::now_ms();
        let until = reset.map_or(now + 60 * 60 * 1000, |seconds| seconds.saturating_mul(1000));
        let mut windows = self.windows();
        windows.rate_limited_until = Some(until.max(now + 1000));
        windows.reason = Some("the PDS's rate limit".to_owned());
        self.save_windows(&windows)
    }

    /// Opens the two-minute back-off window.
    pub fn back_off(&self, reason: &str) -> Result<()> {
        let mut windows = self.windows();
        windows.backoff_until = Some(state::now_ms() + BACKOFF_MS);
        windows.reason = Some(reason.to_owned());
        self.save_windows(&windows)
    }

    /// The DID.
    pub fn did(&self) -> &str {
        &self.did
    }
}

/// What a drain left.
#[derive(Debug, Clone, Default)]
pub struct Drained {
    /// Records published or found published.
    pub published: usize,
    /// Records dropped because another record holds their key.
    pub replaced: usize,
    /// Records still queued.
    pub remaining: usize,
    /// Why records remain.
    pub reason: Option<String>,
    /// When retrying is next worth it (Unix milliseconds).
    pub retry_at: Option<i64>,
    /// Whether the remaining records need `tt sign --login` first.
    pub needs_login: bool,
}

/// Drains `did`'s queue under `lock`, within `deadline`.
pub async fn drain(
    ctx: &Ctx,
    queue: &Queue,
    lock: &QueueLock,
    session: Option<SigningSession>,
    deadline: tokio::time::Instant,
) -> Result<Drained> {
    let mut entries = queue.read(lock)?;
    let mut report = Drained::default();
    if entries.is_empty() {
        return Ok(report);
    }
    let now = state::now_ms();
    if let Some((until, reason)) = queue.windows().open(now) {
        report.remaining = entries.len();
        report.reason = Some(reason);
        report.retry_at = Some(until);
        return Ok(report);
    }
    let resolver = ctx.resolver()?;
    let pds = match &session {
        Some(session) => Some(session.pds.clone()),
        None => resolve_pds(&resolver, queue.did()).await,
    };
    let Some(pds) = pds else {
        report.remaining = entries.len();
        report.reason = Some(format!("the PDS of {} could not be found", queue.did()));
        return Ok(report);
    };

    // 1. Reconcile.
    let mut to_send = Vec::new();
    let mut kept = Vec::new();
    let mut read_failure = None;
    for entry in entries.drain(..) {
        if tokio::time::Instant::now() >= deadline {
            kept.push(entry);
            continue;
        }
        match pds::get_record(&resolver, &pds, &entry.did, &entry.rkey).await {
            Lookup::Present(cid) if cid == entry.cid => report.published += 1,
            Lookup::Present(found) => {
                report.replaced += 1;
                eprintln!(
                    "tt: the queued record at://{}/{}/{} was replaced by another record ({found}); \
                     the signature that names it can no longer verify",
                    entry.did,
                    tartan_atproto::signature::COLLECTION,
                    entry.rkey
                );
                let reported = Reported {
                    did: entry.did.clone(),
                    rkey: entry.rkey.clone(),
                    cid: entry.cid.clone(),
                    found,
                    at: state::now_ms(),
                };
                let _ = state::update_status(&ctx.dirs, |status| status.reported.push(reported));
            }
            Lookup::Absent => to_send.push(entry),
            Lookup::Failed(reason) => {
                read_failure = Some(reason);
                kept.push(entry);
            }
        }
    }

    // 2. Send the absent ones.
    let mut session = session;
    if !to_send.is_empty() {
        let outcome = send(ctx, &resolver, queue, &mut session, &mut to_send, deadline).await;
        report.published += outcome.published;
        if let Some(reason) = outcome.reason {
            report.reason = Some(reason);
        }
        report.retry_at = outcome.retry_at;
        report.needs_login = outcome.needs_login;
        kept.extend(to_send);
    }
    if report.reason.is_none()
        && let Some(reason) = read_failure
    {
        report.reason = Some(format!("a queued record could not be read back: {reason}"));
    }

    // 3. Rewrite. Records appended meanwhile cannot exist: appending takes the same lock.
    kept.sort_by_key(|entry| entry.queued_at);
    report.remaining = kept.len();
    queue.rewrite(lock, &kept)?;
    Ok(report)
}

struct Sent {
    published: usize,
    reason: Option<String>,
    retry_at: Option<i64>,
    needs_login: bool,
}

/// Sends `pending` in calls of at most [`MAX_WRITES`]; what is left in `pending` stays queued.
async fn send(
    ctx: &Ctx,
    resolver: &VerifyingResolver,
    queue: &Queue,
    session: &mut Option<SigningSession>,
    pending: &mut Vec<Entry>,
    deadline: tokio::time::Instant,
) -> Sent {
    let mut sent = Sent {
        published: 0,
        reason: None,
        retry_at: None,
        needs_login: false,
    };
    let Some(current) = session.take() else {
        sent.reason = Some(format!(
            "no signing session for {}; run `tt sign --login`",
            queue.did()
        ));
        sent.needs_login = true;
        return sent;
    };
    let mut current = match oauth::fresh_session(ctx, current).await {
        Ok(fresh) => fresh,
        Err(RefreshFailure::Authorization(message)) => {
            sent.reason = Some(message);
            sent.needs_login = true;
            return sent;
        }
        Err(RefreshFailure::Transient(message)) => {
            let _ = queue.back_off(&message);
            sent.retry_at = Some(state::now_ms() + BACKOFF_MS);
            sent.reason = Some(message);
            return sent;
        }
    };
    let mut retried = false;
    while !pending.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            sent.reason = Some("time ran out".to_owned());
            break;
        }
        let count = pending.len().min(MAX_WRITES);
        let creates: Vec<Create> = pending[..count]
            .iter()
            .map(|entry| Create {
                rkey: entry.rkey.clone(),
                value: entry.record.clone(),
            })
            .collect();
        let outcome = pds::apply_writes(resolver, &mut current, &creates, remaining).await;
        let _ = current.save(ctx);
        match outcome {
            WriteOutcome::Written(_) => {
                pending.drain(..count);
                sent.published += count;
            }
            WriteOutcome::RateLimited(reset) => {
                let _ = queue.rate_limited(reset);
                sent.retry_at = queue.windows().rate_limited_until;
                sent.reason = Some("the PDS's rate limit".to_owned());
                break;
            }
            WriteOutcome::Unreachable(reason) => {
                let _ = queue.back_off(&reason);
                sent.retry_at = Some(state::now_ms() + BACKOFF_MS);
                sent.reason = Some(reason);
                break;
            }
            WriteOutcome::ServerError(reason) => {
                sent.reason = Some(reason);
                break;
            }
            WriteOutcome::Unauthorized(reason) => {
                sent.reason = Some(format!("{reason}; run `tt sign --login`"));
                sent.needs_login = true;
                break;
            }
            WriteOutcome::Refused(reason) => {
                if retried {
                    sent.reason = Some(format!("the PDS refused queued records: {reason}"));
                    break;
                }
                retried = true;
                // Reconcile this batch again: one of its records may exist after all.
                let mut still_absent = Vec::new();
                for entry in pending.drain(..count) {
                    match pds::get_record(resolver, &current.pds, &entry.did, &entry.rkey).await {
                        Lookup::Present(cid) if cid == entry.cid => sent.published += 1,
                        Lookup::Present(_) => {}
                        _ => still_absent.push(entry),
                    }
                }
                still_absent.append(pending);
                *pending = still_absent;
            }
        }
    }
    *session = Some(current);
    sent
}

async fn resolve_pds(resolver: &VerifyingResolver, did: &str) -> Option<String> {
    let parsed = tartan_atproto::did::Did::parse(did, resolver.mode()).ok()?;
    resolver.resolve_did(&parsed).await.ok()?.identity?.pds
}

/// How long a `--flush` or hook drain may take.
pub const FLUSH_TIME: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_report_the_later_end() {
        let windows = Windows {
            rate_limited_until: Some(5_000),
            backoff_until: Some(3_000),
            reason: Some("x".into()),
        };
        assert_eq!(windows.open(1_000), Some((5_000, "x".into())));
        assert_eq!(windows.open(6_000), None);
        let backoff = Windows {
            backoff_until: Some(3_000),
            ..Windows::default()
        };
        assert_eq!(backoff.open(1_000).map(|w| w.0), Some(3_000));
        assert_eq!(Windows::default().open(0), None);
    }
}
