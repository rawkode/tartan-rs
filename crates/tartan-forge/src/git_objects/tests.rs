//! [`GitCache`] against repositories made with the `git` command and fetched over `file://`, and
//! [`MemoryObjects`] against the same histories.

use std::path::Path;
use std::process::Command;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Tests")
        .env("GIT_AUTHOR_EMAIL", "tests@example.com")
        .env("GIT_COMMITTER_NAME", "Tests")
        .env("GIT_COMMITTER_EMAIL", "tests@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

/// A source repository whose commits are made with increasing dates, so newest-first order is
/// defined.
struct Source {
    _dir: tempfile::TempDir,
    path: PathBuf,
    clock: std::cell::Cell<i64>,
}

impl Source {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("source");
        std::fs::create_dir_all(&path).expect("mkdir");
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
        Self {
            _dir: dir,
            path,
            clock: std::cell::Cell::new(1_700_000_000),
        }
    }

    fn url(&self) -> String {
        format!("file://{}", self.path.display())
    }

    /// A commit with `parents` (none for a root) and an empty tree.
    fn commit(&self, message: &str, parents: &[&str]) -> String {
        let tree = git(
            &self.path,
            &["hash-object", "-t", "tree", "-w", "/dev/null"],
        );
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        let time = self.clock.get() + 1;
        self.clock.set(time);
        let date = format!("{time} +0000");
        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.path)
            .env("GIT_AUTHOR_NAME", "Tests")
            .env("GIT_AUTHOR_EMAIL", "tests@example.com")
            .env("GIT_COMMITTER_NAME", "Tests")
            .env("GIT_COMMITTER_EMAIL", "tests@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .expect("git runs");
        assert!(output.status.success());
        let oid = String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned();
        // Keep every commit reachable from a ref, as a host serves it.
        git(
            &self.path,
            &["update-ref", &format!("refs/keep/{oid}"), &oid],
        );
        oid
    }

    /// A chain of `n` commits on top of `parent`, oldest first.
    fn chain(&self, n: usize, parent: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(n);
        for i in 0..n {
            let parents: Vec<&str> = match out.last() {
                Some(last) => vec![last.as_str()],
                None => parent.into_iter().collect(),
            };
            out.push(self.commit(&format!("c{i}"), &parents));
        }
        out
    }

    fn tag(&self, name: &str, target: &str) -> String {
        git(&self.path, &["tag", "-a", name, "-m", name, target]);
        git(&self.path, &["rev-parse", &format!("refs/tags/{name}")])
    }

    /// Copies every object into `memory`.
    fn copy_to(&self, memory: &MemoryObjects) {
        let listing = git(
            &self.path,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname) %(objecttype)",
            ],
        );
        for line in listing.lines() {
            let (oid, kind) = line.split_once(' ').expect("oid kind");
            let output = Command::new("git")
                .args(["cat-file", kind, oid])
                .current_dir(&self.path)
                .output()
                .expect("git runs");
            assert_eq!(memory.insert(kind, output.stdout), oid);
        }
    }
}

fn cache() -> (tempfile::TempDir, GitCache) {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = GitCache::new(dir.path().join("git"), GIT_CACHE_MAX_BYTES);
    (dir, cache)
}

fn never(_: &str) -> bool {
    false
}

fn oids(walk: &Walk) -> Vec<&str> {
    walk.listed.iter().map(|(oid, _)| oid.as_str()).collect()
}

fn read(source: &Source, cache: &GitCache, new: &str, hidden: &[&str]) -> Walk {
    let hidden: Vec<String> = hidden.iter().map(|h| (*h).to_owned()).collect();
    let mut walk = cache
        .list_new_objects("acme/api", &source.url(), None, new, &hidden, WALK_LIMIT)
        .expect("a walk");
    let wanted: Vec<String> = walk.listed.iter().map(|(oid, _)| oid.clone()).collect();
    walk.raw = cache
        .read_objects("acme/api", &wanted)
        .expect("the objects");
    let memory = MemoryObjects::default();
    source.copy_to(&memory);
    let in_memory = memory
        .read_new_objects(new, &hidden, WALK_LIMIT, &never)
        .expect("a walk in memory");
    assert_eq!(
        oids(&in_memory),
        oids(&walk),
        "the fake host lists the same"
    );
    assert_eq!(in_memory.truncated, walk.truncated);
    assert_eq!(in_memory.tip_not_commit, walk.tip_not_commit);
    walk
}

#[test]
fn fast_forward_lists_the_new_commits_newest_first() {
    let source = Source::new();
    let chain = source.chain(3, None);
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, &chain[2], &[&chain[0]]);
    assert_eq!(oids(&walk), vec![chain[2].as_str(), chain[1].as_str()]);
    assert!(!walk.truncated);
    assert_eq!(walk.raw.len(), 2);
    for raw in &walk.raw {
        assert_eq!(raw.kind, ObjectKind::Commit);
        assert!(raw.bytes.starts_with(b"tree "));
    }
}

#[test]
fn rewind_lists_only_the_tip() {
    let source = Source::new();
    let chain = source.chain(3, None);
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, &chain[0], &[&chain[2]]);
    assert_eq!(oids(&walk), vec![chain[0].as_str()]);
}

#[test]
fn a_new_branch_of_checked_commits_reads_no_bytes() {
    let source = Source::new();
    let chain = source.chain(3, None);
    let memory = MemoryObjects::default();
    source.copy_to(&memory);
    let checked: HashSet<String> = chain.iter().cloned().collect();
    let skip = move |oid: &str| checked.contains(oid);
    let walk = memory
        .read_new_objects(&chain[2], &[], WALK_LIMIT, &skip)
        .expect("a walk");
    assert_eq!(walk.listed.len(), 3);
    assert!(walk.raw.is_empty());
    let (_dir, cache) = cache();
    let listed = cache
        .list_new_objects("acme/api", &source.url(), None, &chain[2], &[], WALK_LIMIT)
        .expect("a walk");
    assert_eq!(listed.listed, walk.listed);
    assert!(listed.raw.is_empty(), "listing reads no bytes");
}

#[test]
fn a_new_ref_with_more_than_the_limit_is_truncated() {
    let source = Source::new();
    let chain = source.chain(WALK_LIMIT + 5, None);
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, chain.last().expect("a tip"), &[]);
    assert_eq!(walk.listed.len(), WALK_LIMIT);
    assert!(walk.truncated);
    assert_eq!(walk.listed[0].0, *chain.last().expect("a tip"));
}

#[test]
fn force_push_lists_the_new_line() {
    let source = Source::new();
    let base = source.chain(2, None);
    let old_line = source.chain(2, Some(&base[1]));
    let new_line = source.chain(2, Some(&base[1]));
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, &new_line[1], &[&old_line[1]]);
    assert_eq!(
        oids(&walk),
        vec![new_line[1].as_str(), new_line[0].as_str()]
    );
}

#[test]
fn tags_are_peeled_and_listed() {
    let source = Source::new();
    let chain = source.chain(2, None);
    let inner = source.tag("v1", &chain[1]);
    let outer = source.tag("v1-signed", &inner);
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, &outer, &[&chain[0]]);
    assert_eq!(
        walk.listed,
        vec![
            (outer.clone(), ObjectKind::Tag),
            (inner.clone(), ObjectKind::Tag),
            (chain[1].clone(), ObjectKind::Commit),
        ]
    );
    assert!(walk.tip_not_commit.is_none());
    assert_eq!(walk.raw[0].kind, ObjectKind::Tag);
}

#[test]
fn a_tag_of_a_tree_has_no_commit_tip() {
    let source = Source::new();
    let tree = git(
        &source.path,
        &["hash-object", "-t", "tree", "-w", "/dev/null"],
    );
    git(&source.path, &["update-ref", "refs/keep/tree", &tree]);
    let tag = source.tag("tree", &tree);
    let (_dir, cache) = cache();
    let walk = read(&source, &cache, &tag, &[]);
    assert_eq!(walk.listed, vec![(tag.clone(), ObjectKind::Tag)]);
    assert_eq!(walk.tip_not_commit.as_deref(), Some(tree.as_str()));
}

#[test]
fn an_object_no_longer_served_is_not_transient() {
    let source = Source::new();
    source.chain(1, None);
    let (_dir, cache) = cache();
    let gone = "1111111111111111111111111111111111111111";
    // Over `file://` the remote's reason is lost, so the fetch fails without saying why; the
    // walgit host asks walgit whether it still serves the object (`WalgitClient`).
    cache
        .list_new_objects("acme/api", &source.url(), None, gone, &[], WALK_LIMIT)
        .expect_err("not served");
    let memory = MemoryObjects::default();
    let error = memory
        .read_new_objects(gone, &[], WALK_LIMIT, &never)
        .expect_err("not served");
    assert!(error.downcast_ref::<ObjectNotServed>().is_some());
}

#[test]
fn is_ancestor_follows_history() {
    let source = Source::new();
    let chain = source.chain(3, None);
    let unrelated = source.chain(2, None);
    let (_dir, cache) = cache();
    let memory = MemoryObjects::default();
    source.copy_to(&memory);
    let check = |ancestor: &str, head: &str| {
        let fetched = cache
            .is_ancestor("acme/api", &source.url(), None, ancestor, head)
            .expect("an answer");
        assert_eq!(
            memory.is_ancestor(ancestor, head).expect("an answer"),
            fetched
        );
        fetched
    };
    assert!(check(&chain[0], &chain[2]), "an ancestor");
    assert!(check(&chain[2], &chain[2]), "the commit itself");
    assert!(!check(&chain[2], &chain[0]), "a descendant");
    assert!(!check(&unrelated[1], &chain[2]), "an unrelated history");
}

#[test]
fn the_cache_config_holds_no_token() {
    let source = Source::new();
    let chain = source.chain(2, None);
    let (dir, cache) = cache();
    cache
        .list_new_objects(
            "acme/api",
            &source.url(),
            Some("secret-git-host-token"),
            &chain[1],
            &[],
            WALK_LIMIT,
        )
        .expect("a walk");
    let config = std::fs::read_to_string(dir.path().join("git/acme/api.git/config"))
        .expect("the cache repository's config");
    assert!(!config.contains("secret-git-host-token"), "{config}");
    assert!(!config.contains("extraHeader"), "{config}");
}

#[test]
fn prunes_least_recently_used_repositories() {
    let source = Source::new();
    let chain = source.chain(1, None);
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = GitCache::new(dir.path().join("git"), 1);
    for key in ["acme/one", "acme/two"] {
        cache
            .list_new_objects(key, &source.url(), None, &chain[0], &[], WALK_LIMIT)
            .expect("a walk");
    }
    assert!(!dir.path().join("git/acme/one.git").exists(), "pruned");
    assert!(
        dir.path().join("git/acme/two.git").exists(),
        "kept: just used"
    );
}
