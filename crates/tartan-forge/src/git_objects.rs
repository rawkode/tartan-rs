//! Reading the objects a ref update introduced (design D8, *Reading objects*).
//!
//! [`GitCache`] fetches from the Git host over smart HTTP with gix into a bare cache repository
//! per Git host repository, under `TARTAN_STATE_DIR/git`, and walks it there. The cache is a
//! disposable optimisation: it is pruned by least-recently-used repository above a size bound
//! and rebuilt on demand. The Git host token reaches gix as an in-memory `http.extraHeader` for
//! each fetch; it is never written to the cache repository's configuration.
//!
//! Every object read is hashed again from its bytes with collision-detecting SHA-1 (gix 0.90's
//! `sha1` feature, gix-hash's `sha1dc` backend), and an object whose bytes do not hash to its id,
//! or whose hashing detects a collision attempt, is refused.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// The most commits one update lists for judging.
pub const WALK_LIMIT: usize = 1000;

/// The kind of a judged object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    /// A commit.
    Commit,
    /// An annotated tag.
    Tag,
}

impl ObjectKind {
    /// The kind as stored.
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Commit => "commit",
            ObjectKind::Tag => "tag",
        }
    }

    /// Parses a stored kind.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "commit" => Some(ObjectKind::Commit),
            "tag" => Some(ObjectKind::Tag),
            _ => None,
        }
    }
}

/// A listed object's content, without the `<kind> <size>\0` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawObject {
    /// The object id (hex).
    pub oid: String,
    /// Its kind.
    pub kind: ObjectKind,
    /// Its content.
    pub bytes: Vec<u8>,
}

/// What a ref update introduced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Walk {
    /// The objects to judge, newest first: the annotated tags `new` names (outermost first),
    /// the commit they peel to (the tip), then every commit in `old..new` up to the limit.
    pub listed: Vec<(String, ObjectKind)>,
    /// The content of each listed object that `skip` did not exclude.
    pub raw: Vec<RawObject>,
    /// Whether the walk stopped at the limit.
    pub truncated: bool,
    /// When `new` names a tag whose peeled target is not a commit (a tree or blob): that
    /// object's id. Such a tip is never compliant on a covered ref.
    pub tip_not_commit: Option<String>,
}

/// The update's `new` is no longer served by the Git host (force-pushed away before the check
/// ran). It is not transient: the check gives up at once.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("object {oid} is no longer on the host")]
pub struct ObjectNotServed {
    /// The object.
    pub oid: String,
}

/// Bare cache repositories under one directory.
#[derive(Debug)]
pub struct GitCache {
    root: PathBuf,
    max_bytes: u64,
}

/// The default bound on the cache's size before least-recently-used repositories are removed.
pub const GIT_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// One lock per cache repository, so two fetches into one repository never interleave.
fn repository_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.entry(path.to_owned()).or_default().clone()
}

fn parse_oid(hex: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_hex(hex.as_bytes())
        .map_err(|error| anyhow!("{hex:?} is not an object id: {error}"))
}

/// A gix error with its sources, so the remote's own message (such as `not our ref`) is kept.
fn chain(error: impl std::error::Error) -> anyhow::Error {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    anyhow!(text)
}

/// Whether a fetch error says the host does not serve the object.
fn not_served(error: &str) -> bool {
    error.contains("not our ref")
        || error.contains("unadvertised object")
        || error.contains("no such remote ref")
        || error.contains("not found")
}

impl GitCache {
    /// A cache under `root`, bounded at `max_bytes`.
    pub fn new(root: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            root: root.into(),
            max_bytes,
        }
    }

    /// The cache repository for `key` (such as `owner/name`).
    fn path(&self, key: &str) -> Result<PathBuf> {
        let mut path = self.root.clone();
        for part in key.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                bail!("{key:?} is not a repository key");
            }
            path.push(part);
        }
        path.set_extension("git");
        Ok(path)
    }

    fn open(&self, path: &Path) -> Result<gix::Repository> {
        let repo = if path.join("HEAD").exists() {
            gix::open(path).with_context(|| format!("open {}", path.display()))?
        } else {
            std::fs::create_dir_all(path)?;
            gix::init_bare(path).with_context(|| format!("init {}", path.display()))?
        };
        // Mark the repository as used, for least-recently-used pruning.
        let _ = std::fs::write(path.join("tartan-used"), b"");
        Ok(repo)
    }

    /// Fetches `oids` from `url` unless every one of them is present already.
    fn ensure(
        &self,
        repo: &mut gix::Repository,
        url: &str,
        token: Option<&str>,
        oids: &[gix::ObjectId],
    ) -> Result<()> {
        let missing: Vec<gix::ObjectId> = oids
            .iter()
            .filter(|oid| !repo.has_object(**oid))
            .copied()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        if let Some(token) = token {
            // In memory only: the snapshot is committed to this handle, never to disk.
            let mut config = repo.config_snapshot_mut();
            config
                .set_raw_value(
                    "http.extraHeader",
                    format!("Authorization: Bearer {token}").as_str(),
                )
                .map_err(|error| anyhow!("set the Git host credential: {error}"))?;
            config
                .commit()
                .map_err(|error| anyhow!("apply the Git host credential: {error}"))?;
        }
        let refspecs: Vec<String> = missing
            .iter()
            .map(|oid| format!("{oid}:refs/tartan/fetched/{oid}"))
            .collect();
        let fetched = (|| -> Result<()> {
            let remote = repo
                .remote_at(url)
                .map_err(chain)?
                .with_fetch_tags(gix::remote::fetch::Tags::None)
                .with_refspecs(
                    refspecs.iter().map(String::as_str),
                    gix::remote::Direction::Fetch,
                )
                .map_err(chain)?;
            let connection = remote
                .connect(gix::remote::Direction::Fetch)
                .map_err(chain)?;
            connection
                .prepare_fetch(gix::progress::Discard, Default::default())
                .map_err(chain)?
                .receive(gix::progress::Discard, &AtomicBool::new(false))
                .map_err(chain)?;
            Ok(())
        })();
        if let Err(error) = fetched {
            let text = format!("{error:#}");
            if not_served(&text)
                && let Some(oid) = missing.first()
            {
                return Err(anyhow::Error::new(ObjectNotServed {
                    oid: oid.to_string(),
                })
                .context(text));
            }
            return Err(error.context(format!("fetch from {}", redact(url))));
        }
        for oid in &missing {
            if !repo.has_object(*oid) {
                return Err(ObjectNotServed {
                    oid: oid.to_string(),
                }
                .into());
            }
        }
        Ok(())
    }

    /// Lists what the update `hidden..new` of the repository at `url` introduced (design D8,
    /// *Two sets per update*), leaving [`Walk::raw`] empty: [`Self::read_objects`] reads the
    /// listed objects wanted. Blocking: call it from a blocking task.
    pub fn list_new_objects(
        &self,
        key: &str,
        url: &str,
        token: Option<&str>,
        new: &str,
        hidden: &[String],
        limit: usize,
    ) -> Result<Walk> {
        let path = self.path(key)?;
        let lock = repository_lock(&path);
        let walk = {
            let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut repo = self.open(&path)?;
            let new_id = parse_oid(new)?;
            self.ensure(&mut repo, url, token, &[new_id])?;
            // The hidden tips are fetched when they can be; one the host no longer serves hides
            // nothing, so the walk only lists more.
            let mut hide = Vec::new();
            for old in hidden {
                let old_id = parse_oid(old)?;
                if old_id.is_null() {
                    continue;
                }
                match self.ensure(&mut repo, url, token, &[old_id]) {
                    Ok(()) => hide.push(old_id),
                    Err(error) if error.downcast_ref::<ObjectNotServed>().is_some() => {}
                    Err(error) => return Err(error),
                }
            }
            list(&repo, new_id, &hide, limit)?
        };
        self.prune(&path);
        Ok(walk)
    }

    /// Reads `oids` (each a listed commit or tag) from the cache repository for `key`, checking
    /// each against its id. Blocking: call it from a blocking task.
    pub fn read_objects(&self, key: &str, oids: &[String]) -> Result<Vec<RawObject>> {
        let path = self.path(key)?;
        let lock = repository_lock(&path);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let repo = gix::open(&path).with_context(|| format!("open {}", path.display()))?;
        oids.iter()
            .map(|oid| {
                let (kind, bytes) = read_checked(&repo, parse_oid(oid)?)?;
                let kind = match kind {
                    gix::object::Kind::Commit => ObjectKind::Commit,
                    gix::object::Kind::Tag => ObjectKind::Tag,
                    other => bail!("object {oid} is a {other}, not a commit or tag"),
                };
                Ok(RawObject {
                    oid: oid.clone(),
                    kind,
                    bytes,
                })
            })
            .collect()
    }

    /// Whether `ancestor` is `head` or one of its ancestors in the repository at `url`.
    /// Blocking: call it from a blocking task.
    pub fn is_ancestor(
        &self,
        key: &str,
        url: &str,
        token: Option<&str>,
        ancestor: &str,
        head: &str,
    ) -> Result<bool> {
        let path = self.path(key)?;
        let lock = repository_lock(&path);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut repo = self.open(&path)?;
        let (ancestor, head) = (parse_oid(ancestor)?, parse_oid(head)?);
        self.ensure(&mut repo, url, token, &[head])?;
        if ancestor == head {
            return Ok(true);
        }
        walk_contains(&repo, head, ancestor)
    }

    /// Removes least-recently-used cache repositories, other than `keep`, while the cache is
    /// larger than its bound.
    fn prune(&self, keep: &Path) {
        let mut repos = Vec::new();
        collect_repositories(&self.root, &mut repos);
        let mut sized: Vec<(PathBuf, u64, std::time::SystemTime)> = repos
            .into_iter()
            .map(|path| {
                let used = std::fs::metadata(path.join("tartan-used"))
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let size = dir_size(&path);
                (path, size, used)
            })
            .collect();
        let mut total: u64 = sized.iter().map(|(_, size, _)| size).sum();
        if total <= self.max_bytes {
            return;
        }
        sized.sort_by_key(|(_, _, used)| *used);
        for (path, size, _) in sized {
            if total <= self.max_bytes {
                break;
            }
            if path == keep {
                continue;
            }
            let lock = repository_lock(&path);
            let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
            if std::fs::remove_dir_all(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}

/// The URL without credentials, for error messages.
fn redact(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => url.to_owned(),
    }
}

fn collect_repositories(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.extension().is_some_and(|e| e == "git") && path.join("HEAD").exists() {
            out.push(path);
        } else {
            collect_repositories(&path, out);
        }
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Reads `id`'s kind and content, checking that the content hashes to `id` with
/// collision-detecting SHA-1.
fn read_checked(repo: &gix::Repository, id: gix::ObjectId) -> Result<(gix::object::Kind, Vec<u8>)> {
    let object = repo
        .find_object(id)
        .map_err(|error| anyhow!("read {id}: {error}"))?;
    let kind = object.kind;
    let data = object.data.clone();
    let computed = gix::objs::compute_hash(id.kind(), kind, &data)
        .map_err(|error| anyhow!("object {id} failed collision-detecting hashing: {error}"))?;
    if computed != id {
        bail!("object {id} does not hash to its id");
    }
    Ok((kind, data))
}

/// The target of a raw annotated tag.
fn tag_target(data: &[u8]) -> Result<gix::ObjectId> {
    let tag = gix::objs::TagRef::from_bytes(data, gix::hash::Kind::Sha1)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(tag.target())
}

/// The commit `id` names, peeling annotated tags; `None` when it is not a commit underneath.
fn peel_to_commit(repo: &gix::Repository, id: gix::ObjectId) -> Result<Option<gix::ObjectId>> {
    let mut current = id;
    for _ in 0..=64 {
        let (kind, data) = read_checked(repo, current)?;
        match kind {
            gix::object::Kind::Commit => return Ok(Some(current)),
            gix::object::Kind::Tag => current = tag_target(&data)?,
            _ => return Ok(None),
        }
    }
    bail!("tag {id} nests more than 64 tags")
}

fn list(
    repo: &gix::Repository,
    new: gix::ObjectId,
    hidden: &[gix::ObjectId],
    limit: usize,
) -> Result<Walk> {
    let mut walk = Walk::default();
    // Peel `new` through nested tags; each tag on the way is judged.
    let mut current = new;
    let tip = loop {
        let (kind, data) = read_checked(repo, current)?;
        match kind {
            gix::object::Kind::Tag => {
                walk.listed.push((current.to_string(), ObjectKind::Tag));
                current = tag_target(&data)?;
                if walk.listed.len() > 64 {
                    bail!("tag {new} nests more than 64 tags");
                }
            }
            gix::object::Kind::Commit => break Some(current),
            _ => {
                walk.tip_not_commit = Some(current.to_string());
                break None;
            }
        }
    };
    if let Some(tip) = tip {
        walk.listed.push((tip.to_string(), ObjectKind::Commit));
        let mut seen: HashSet<gix::ObjectId> = HashSet::from([tip]);
        // The rev-walk hides only commits, so each hidden tip is peeled through its tags, as
        // git does for `^old`; one that is not a commit underneath hides nothing.
        let mut peeled = Vec::with_capacity(hidden.len());
        for id in hidden {
            if let Some(commit) = peel_to_commit(repo, *id)? {
                peeled.push(commit);
            }
        }
        let commits = repo
            .rev_walk([tip])
            .with_hidden(peeled)
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .all()
            .map_err(|error| anyhow!("walk from {tip}: {error}"))?;
        let mut counted = 1usize;
        for info in commits {
            let info = info.map_err(|error| anyhow!("walk from {tip}: {error}"))?;
            if !seen.insert(info.id) {
                continue;
            }
            if counted >= limit {
                walk.truncated = true;
                break;
            }
            counted += 1;
            walk.listed.push((info.id.to_string(), ObjectKind::Commit));
        }
    }
    Ok(walk)
}

/// Whether `target` is reachable from `head`.
fn walk_contains(
    repo: &gix::Repository,
    head: gix::ObjectId,
    target: gix::ObjectId,
) -> Result<bool> {
    let commits = repo
        .rev_walk([head])
        .all()
        .map_err(|error| anyhow!("walk from {head}: {error}"))?;
    for info in commits {
        let info = info.map_err(|error| anyhow!("walk from {head}: {error}"))?;
        if info.id == target {
            return Ok(true);
        }
    }
    Ok(false)
}

/// An in-memory object graph that answers [`Walk`]s the same way [`GitCache`] does, for fakes
/// of the Git host in tests. Commits and tags are stored with their raw content; the walk reads
/// parents from the content.
#[derive(Debug, Default)]
pub struct MemoryObjects {
    objects: Mutex<HashMap<String, (String, Vec<u8>)>>,
}

impl MemoryObjects {
    /// Stores an object (`kind` is `commit`, `tag`, `tree` or `blob`) and returns its SHA-1 id.
    pub fn insert(&self, kind: &str, bytes: Vec<u8>) -> String {
        let object_kind = match kind {
            "commit" => gix::object::Kind::Commit,
            "tag" => gix::object::Kind::Tag,
            "tree" => gix::object::Kind::Tree,
            _ => gix::object::Kind::Blob,
        };
        let oid = gix::objs::compute_hash(gix::hash::Kind::Sha1, object_kind, &bytes)
            .map(|id| id.to_string())
            .unwrap_or_default();
        self.objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(oid.clone(), (kind.to_owned(), bytes));
        oid
    }

    /// Forgets an object, as a host does once nothing references it.
    pub fn remove(&self, oid: &str) {
        self.objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(oid);
    }

    /// Whether the object is stored.
    pub fn contains(&self, oid: &str) -> bool {
        self.objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(oid)
    }

    fn get(&self, oid: &str) -> Option<(String, Vec<u8>)> {
        self.objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(oid)
            .cloned()
    }

    fn parents(&self, oid: &str) -> Vec<String> {
        let Some((kind, bytes)) = self.get(oid) else {
            return Vec::new();
        };
        if kind != "commit" {
            return Vec::new();
        }
        gix::objs::CommitRef::from_bytes(&bytes, gix::hash::Kind::Sha1)
            .map(|commit| commit.parents().map(|p| p.to_string()).collect())
            .unwrap_or_default()
    }

    fn committed_at(&self, oid: &str) -> i64 {
        self.get(oid)
            .and_then(|(_, bytes)| {
                gix::objs::CommitRef::from_bytes(&bytes, gix::hash::Kind::Sha1)
                    .ok()
                    .and_then(|c| c.committer().ok().map(|s| s.seconds()))
            })
            .unwrap_or_default()
    }

    /// Lists and reads `hidden..new`, as [`GitCache::read_new_objects`] does.
    pub fn read_new_objects(
        &self,
        new: &str,
        hidden: &[String],
        limit: usize,
        skip: &(dyn Fn(&str) -> bool + Send + Sync),
    ) -> Result<Walk> {
        let mut walk = Walk::default();
        let mut current = new.to_owned();
        let tip = loop {
            let Some((kind, bytes)) = self.get(&current) else {
                return Err(ObjectNotServed { oid: current }.into());
            };
            match kind.as_str() {
                "tag" => {
                    walk.listed.push((current.clone(), ObjectKind::Tag));
                    current = tag_target(&bytes)?.to_string();
                }
                "commit" => break Some(current),
                _ => {
                    walk.tip_not_commit = Some(current);
                    break None;
                }
            }
        };
        if let Some(tip) = tip {
            let mut hidden_set = HashSet::new();
            // Hidden tips are peeled through tags, as the cache's walk does.
            let mut stack: Vec<String> = Vec::new();
            for hidden_tip in hidden {
                let mut current = hidden_tip.clone();
                while let Some((kind, bytes)) = self.get(&current) {
                    match kind.as_str() {
                        "tag" => current = tag_target(&bytes)?.to_string(),
                        "commit" => {
                            stack.push(current);
                            break;
                        }
                        _ => break,
                    }
                }
            }
            while let Some(oid) = stack.pop() {
                if hidden_set.insert(oid.clone()) {
                    stack.extend(self.parents(&oid));
                }
            }
            walk.listed.push((tip.clone(), ObjectKind::Commit));
            let mut seen = HashSet::from([tip.clone()]);
            let mut queue: Vec<String> = self.parents(&tip);
            let mut found = Vec::new();
            while let Some(oid) = queue.pop() {
                if hidden_set.contains(&oid) || !seen.insert(oid.clone()) || !self.contains(&oid) {
                    continue;
                }
                queue.extend(self.parents(&oid));
                found.push(oid);
            }
            found.sort_by_key(|oid| std::cmp::Reverse(self.committed_at(oid)));
            for oid in found {
                if walk.listed.len()
                    - walk
                        .listed
                        .iter()
                        .filter(|(_, k)| *k == ObjectKind::Tag)
                        .count()
                    >= limit
                {
                    walk.truncated = true;
                    break;
                }
                walk.listed.push((oid, ObjectKind::Commit));
            }
        }
        for (oid, kind) in &walk.listed {
            if skip(oid) {
                continue;
            }
            if let Some((_, bytes)) = self.get(oid) {
                walk.raw.push(RawObject {
                    oid: oid.clone(),
                    kind: *kind,
                    bytes,
                });
            }
        }
        Ok(walk)
    }

    /// Whether `ancestor` is `head` or one of its ancestors.
    pub fn is_ancestor(&self, ancestor: &str, head: &str) -> Result<bool> {
        if !self.contains(head) {
            return Err(ObjectNotServed {
                oid: head.to_owned(),
            }
            .into());
        }
        let mut seen = HashSet::new();
        let mut stack = vec![head.to_owned()];
        while let Some(oid) = stack.pop() {
            if oid == ancestor {
                return Ok(true);
            }
            if seen.insert(oid.clone()) {
                stack.extend(self.parents(&oid));
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
