//! What `tt` remembers that is not a secret, under `$XDG_STATE_HOME/tt`:
//!
//! - `sessions.json`, an index of the stored credentials (which forges and DIDs, their kinds and
//!   expiries), so `tt status` and account selection need not read the credential store, which
//!   cannot list its entries;
//! - `status.json`, the last signing failure and the records a drain reported, which `tt status`
//!   shows because Git hides what a signing program says unless signing fails.
//!
//! Both are rewritten under one exclusive lock, so concurrent `tt` processes never lose each
//! other's changes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::paths::{self, Dirs};

/// How long a public client's signing session lasts on the reference authorization server.
pub const SIGNING_SESSION_MS: i64 = 14 * 24 * 60 * 60 * 1000;

/// A forge credential, as indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeEntry {
    /// `cli` (a `tt login` session) or `token` (`tt login --with-token`).
    pub kind: String,
    /// The DID it acts as or for.
    pub did: String,
    /// The handle the forge showed at login.
    #[serde(default)]
    pub handle: Option<String>,
    /// When it expires (Unix milliseconds), when known.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// A signing session, as indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningEntry {
    /// The OAuth client id the session belongs to.
    pub client_id: String,
    /// The forge whose client metadata it uses (absent for the `http://localhost` client).
    #[serde(default)]
    pub forge: Option<String>,
    /// The authorization server.
    pub issuer: String,
    /// The PDS.
    pub pds: String,
    /// The handle at login.
    #[serde(default)]
    pub handle: Option<String>,
    /// When the session was created (Unix milliseconds).
    pub created_at: i64,
}

impl SigningEntry {
    /// When the session ends at the latest, on the reference server's two-week limit.
    pub fn ends_at(&self) -> i64 {
        self.created_at + SIGNING_SESSION_MS
    }
}

/// `sessions.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Forge credentials by origin.
    #[serde(default)]
    pub forges: BTreeMap<String, ForgeEntry>,
    /// Signing sessions by DID.
    #[serde(default)]
    pub signing: BTreeMap<String, SigningEntry>,
}

/// A signing failure, for `tt status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// The DID that was signing, when known.
    #[serde(default)]
    pub did: Option<String>,
    /// When (Unix milliseconds).
    pub at: i64,
    /// What failed and how to fix it.
    pub message: String,
}

/// A queued record a drain found replaced (another CID under its key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reported {
    /// The DID.
    pub did: String,
    /// The record key.
    pub rkey: String,
    /// The CID the signature names.
    pub cid: String,
    /// What the repository holds instead.
    pub found: String,
    /// When it was found (Unix milliseconds).
    pub at: i64,
}

/// `status.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The last signing failure.
    #[serde(default)]
    pub last_failure: Option<Failure>,
    /// Records reported by drains, newest last.
    #[serde(default)]
    pub reported: Vec<Reported>,
}

/// The current time in Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn update<T: Serialize + DeserializeOwned + Default, R>(
    dirs: &Dirs,
    file: &str,
    change: impl FnOnce(&mut T) -> R,
) -> Result<R> {
    let lock = paths::lock_file(&dirs.state.join("state.lock"))?;
    lock.lock().context("locking tt's state")?;
    let path = dirs.state.join(file);
    let mut value: T = read(&path)?;
    let result = change(&mut value);
    let bytes = serde_json::to_vec_pretty(&value).context("encoding tt's state")?;
    paths::write_private(&path, &bytes)?;
    Ok(result)
}

/// Reads `sessions.json`.
pub fn index(dirs: &Dirs) -> Result<Index> {
    read(&dirs.state.join("sessions.json"))
}

/// Changes `sessions.json`.
pub fn update_index<R>(dirs: &Dirs, change: impl FnOnce(&mut Index) -> R) -> Result<R> {
    update(dirs, "sessions.json", change)
}

/// Reads `status.json`.
pub fn status(dirs: &Dirs) -> Result<Status> {
    read(&dirs.state.join("status.json"))
}

/// Changes `status.json`.
pub fn update_status<R>(dirs: &Dirs, change: impl FnOnce(&mut Status) -> R) -> Result<R> {
    update(dirs, "status.json", change)
}

/// Records the last signing failure (best effort: signing reports its own failure anyway).
pub fn record_failure(dirs: &Dirs, did: Option<&str>, message: &str) {
    let _ = update_status(dirs, |status| {
        status.last_failure = Some(Failure {
            did: did.map(str::to_owned),
            at: now_ms(),
            message: message.to_owned(),
        });
    });
}

/// Clears the last signing failure after a signing that worked.
pub fn clear_failure(dirs: &Dirs) {
    if status(dirs).is_ok_and(|status| status.last_failure.is_some()) {
        let _ = update_status(dirs, |status| status.last_failure = None);
    }
}
