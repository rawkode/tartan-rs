//! The forge for end-to-end tests: the real Rivet engine and gateway, packages in memory, and a
//! Git host whose pushes are simulated: a pushed `tartan.cue` is stored per commit and announced
//! with a signed ref event for `main`.

// Each test binary uses part of this module.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use rivetkit::Registry;
use rivetkit::test::TestHandle;
use serde_json::{Value, json};
use sha2::Sha256;
use tartan::{GatewayState, router};
use tartan_extensions::{ExtensionPackage, ExtensionRuntime};
use tartan_forge::{Extensions, GitHost, Services, install_services, register_actors};

pub const ADMIN_TOKEN: &str = "test-admin-token";
pub const WEBHOOK_SECRET: &[u8] = b"test-webhook-secret";
pub const DEADLINE: Duration = Duration::from_secs(60);

/// A Git host that keeps each pushed commit's `tartan.cue`.
#[derive(Default)]
pub struct FakeGitHost {
    pub created: Mutex<Vec<String>>,
    /// `(owner/name, sha)` to the commit's `tartan.cue`, if it has one.
    files: Mutex<HashMap<(String, String), Option<String>>>,
}

#[async_trait]
impl GitHost for FakeGitHost {
    async fn create_repo(&self, owner: &str, name: &str) -> Result<()> {
        self.created
            .lock()
            .expect("never poisoned")
            .push(format!("{owner}/{name}"));
        Ok(())
    }

    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("http://git.test/{owner}/{name}.git")
    }

    async fn read_file(
        &self,
        owner: &str,
        name: &str,
        rev: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>> {
        if path != "tartan.cue" {
            return Ok(None);
        }
        let files = self.files.lock().expect("never poisoned");
        Ok(files
            .get(&(format!("{owner}/{name}"), rev.to_owned()))
            .cloned()
            .flatten()
            .map(String::into_bytes))
    }
}

/// A running forge.
pub struct Forge {
    pub base: String,
    pub http: reqwest::Client,
    pub git: Arc<FakeGitHost>,
    actors: Option<TestHandle>,
    seq: AtomicU64,
    heads: Mutex<HashMap<String, String>>,
}

impl Forge {
    /// Starts the engine, the actors and the gateway; `name` keeps this run's forge apart from
    /// earlier runs on the same engine.
    pub async fn start(name: &str) -> Self {
        // Warnings show with a failing test's output.
        let _ = tracing_subscriber::fmt()
            .with_env_filter("warn")
            .with_test_writer()
            .try_init();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_nanos();
        let git = Arc::new(FakeGitHost::default());
        let extensions = Arc::new(Extensions::new(
            ExtensionRuntime::new().expect("runtime"),
            Arc::new(object_store::memory::InMemory::new()),
        ));
        install_services(Services {
            forge_id: format!("{name}-{nanos}"),
            git_host: git.clone(),
            extensions: extensions.clone(),
        })
        .expect("installed once");
        let mut registry = Registry::new();
        register_actors(&mut registry);
        let actors = rivetkit::test::setup(registry)
            .await
            .expect("engine and registry");
        let state = GatewayState {
            client: actors.client().clone(),
            admin_token: Arc::from(ADMIN_TOKEN),
            webhook_secret: Arc::from(WEBHOOK_SECRET),
            extensions,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        Self {
            base,
            http: reqwest::Client::new(),
            git,
            actors: Some(actors),
            seq: AtomicU64::new(1),
            heads: Mutex::new(HashMap::new()),
        }
    }

    pub async fn stop(mut self) {
        if let Some(actors) = self.actors.take() {
            actors.shutdown().await;
        }
    }

    pub async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(ADMIN_TOKEN);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("the gateway answers");
        let status = response.status().as_u16();
        let text = response.text().await.expect("a body");
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.send(reqwest::Method::GET, path, None).await
    }

    /// POSTs and expects a 2xx.
    pub async fn ok(&self, path: &str, body: Value) -> Value {
        let (status, value) = self.post(path, body).await;
        assert!((200..300).contains(&status), "{path}: {status} {value}");
        value
    }

    /// Polls `read` until `done` holds for its result.
    pub async fn until(
        &self,
        what: &str,
        read: impl AsyncFn() -> Value,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let start = Instant::now();
        loop {
            let value = read().await;
            if done(&value) {
                return value;
            }
            assert!(start.elapsed() < DEADLINE, "{what}: {value:#}");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Publishes `package`, with `manifest` in place of its own.
    pub async fn publish(&self, package: &ExtensionPackage, manifest: &Value) -> Value {
        let upload = json!({
            "manifest": manifest,
            "component": base64::engine::general_purpose::STANDARD.encode(&package.component),
            "migrations": package.migrations.iter()
                .map(|(path, sql)| json!({"path": path, "sql": sql}))
                .collect::<Vec<_>>(),
        });
        self.ok("/-/api/packages", upload).await
    }

    /// Pushes a commit to `repo`'s `main` with `config` as its `tartan.cue` (none for `None`),
    /// and returns the commit.
    pub async fn push_config(&self, repo: &str, config: Option<&str>) -> String {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let sha = format!("{seq:040x}");
        self.git
            .files
            .lock()
            .expect("never poisoned")
            .insert((repo.to_owned(), sha.clone()), config.map(str::to_owned));
        let old = self
            .heads
            .lock()
            .expect("never poisoned")
            .insert(repo.to_owned(), sha.clone())
            .unwrap_or_else(|| "0".repeat(40));
        let event = json!([ref_event(repo, seq, "refs/heads/main", &old, &sha)]);
        assert_eq!(self.hook(&event).await, 204, "the push is recorded");
        sha
    }

    /// Pushes a commit to another branch of `repo`.
    pub async fn push_branch(&self, repo: &str, branch: &str, config: &str) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let sha = format!("{seq:040x}");
        self.git
            .files
            .lock()
            .expect("never poisoned")
            .insert((repo.to_owned(), sha.clone()), Some(config.to_owned()));
        let event = json!([ref_event(repo, seq, branch, &"0".repeat(40), &sha)]);
        assert_eq!(self.hook(&event).await, 204);
    }

    /// A walgit log sequence number not used before in this forge.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Posts ref events to the webhook, signed.
    pub async fn hook(&self, events: &Value) -> u16 {
        let body = serde_json::to_vec(events).expect("encode");
        let mut mac = Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET).expect("any key length");
        mac.update(&body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        self.http
            .post(format!("{}/-/hooks/git-host", self.base))
            .header("x-walgit-signature", signature)
            .body(body)
            .send()
            .await
            .expect("the gateway answers")
            .status()
            .as_u16()
    }

    /// The installations on `path` (`org` or `org/repo`), as `id@version`.
    pub async fn installed(&self, path: &str) -> Vec<String> {
        let route = match path.split_once('/') {
            Some((org, repo)) => format!("/-/api/orgs/{org}/repos/{repo}/extensions"),
            None => format!("/-/api/orgs/{path}/extensions"),
        };
        let (status, list) = self.get(&route).await;
        assert_eq!(status, 200, "{list}");
        list.as_array()
            .expect("a list")
            .iter()
            .map(|i| {
                format!(
                    "{}@{}",
                    i["extension_id"].as_str().unwrap_or_default(),
                    i["version"].as_str().unwrap_or_default()
                )
            })
            .collect()
    }

    /// Waits until `path`'s installations are `expected`.
    pub async fn until_installed(&self, path: &str, expected: &[&str]) {
        let start = Instant::now();
        loop {
            let installed = self.installed(path).await;
            if installed == expected {
                return;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "{path} has {installed:?}, not {expected:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The types of the events subscriber `name` has received.
    pub async fn received_types(&self, name: &str) -> Vec<String> {
        let (_, received) = self
            .get(&format!("/-/api/subscribers/{name}/events?limit=500"))
            .await;
        received
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|r| r["event"]["type"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The events subscriber `name` has received.
    pub async fn received(&self, name: &str) -> Vec<Value> {
        let (_, received) = self
            .get(&format!("/-/api/subscribers/{name}/events?limit=500"))
            .await;
        received
            .as_array()
            .map(|list| list.iter().map(|r| r["event"].clone()).collect())
            .unwrap_or_default()
    }
}

/// One walgit ref event.
pub fn ref_event(repo: &str, seq: u64, name: &str, old: &str, new: &str) -> Value {
    let zero = |s: &str| s.bytes().all(|b| b == b'0');
    let action = match (zero(old), zero(new)) {
        (true, _) => "create",
        (_, true) => "delete",
        _ => "update",
    };
    json!({
        "action": action,
        "ref_type": "branch",
        "ref_name": name,
        "old": old,
        "new": new,
        "pusher": "alice",
        "repo": repo,
        "_walgit": {"schema_version": 1, "seq": seq.to_string(), "entry_kind": "push"}
    })
}

/// The package directory named by environment variable `name`.
pub fn package(name: &str) -> ExtensionPackage {
    let dir = std::env::var(name).unwrap_or_else(|_| panic!("{name} names a package directory"));
    ExtensionPackage::from_dir(dir.as_ref()).expect("a package")
}
