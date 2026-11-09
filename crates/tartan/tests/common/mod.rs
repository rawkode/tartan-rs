//! The forge for end-to-end tests: the real Rivet engine and gateway, the registry and packages
//! in memory, and a Git host whose pushes are simulated: a pushed `tartan.cue` is stored per
//! commit and announced with a signed ref event for `main`.
//!
//! Extension packages reach the forge as they do in a deployment: each is published as a CUE
//! module (`cue mod publish`, with the `cue` in `CUE_BIN`) to the source registry at
//! `TARTAN_MODULE_SOURCE` (a test-only CNCF distribution that `scripts/e2e.sh` starts), then the
//! forge syncs the modules published so far. The first-party modules (`tartan.rawkode.academy/types`,
//! Issues and Kanban) are built and published once by `scripts/e2e.sh`, which lists them in
//! `TARTAN_EXTENSION_MODULES`; tests add them to the sync. The gateway listens on the address in
//! `CUE_REGISTRY` when it is set, so the CUE evaluator in the test process resolves imports
//! against this forge's registry.

// Each test binary uses part of this module.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use rivetkit::Registry;
use rivetkit::client::Client;
use rivetkit::test::TestHandle;
use serde_json::{Value, json};
use sha2::Sha256;
use tartan::{GatewayState, RegistryStore, SyncReport, SyncTargets, router, sync_modules};
use tartan_config::DirEntry;
use tartan_config::deployment::{Deployment, ModuleVersion};
use tartan_extensions::{COMPONENT_FILE, ExtensionPackage, ExtensionRuntime};
use tartan_forge::{Extensions, GitHost, Services, install_services, register_actors};

pub const ADMIN_TOKEN: &str = "test-admin-token";
pub const WEBHOOK_SECRET: &[u8] = b"test-webhook-secret";
pub const DEADLINE: Duration = Duration::from_secs(60);

/// A Git host that keeps each pushed commit's files.
#[derive(Default)]
pub struct FakeGitHost {
    pub created: Mutex<Vec<String>>,
    /// `(owner/name, sha)` to the commit's files, by path.
    files: Mutex<HashMap<(String, String), BTreeMap<String, String>>>,
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

    async fn list_dir(
        &self,
        owner: &str,
        name: &str,
        rev: &str,
        path: &str,
    ) -> Result<Option<Vec<DirEntry>>> {
        let files = self.files.lock().expect("never poisoned");
        let Some(commit) = files.get(&(format!("{owner}/{name}"), rev.to_owned())) else {
            return Ok(None);
        };
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut entries: BTreeMap<String, bool> = BTreeMap::new();
        for file in commit.keys() {
            if let Some(rest) = file.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((dir, _)) => entries.insert(dir.to_owned(), true),
                    None => entries.insert(rest.to_owned(), false),
                };
            }
        }
        if entries.is_empty() && !path.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            entries
                .into_iter()
                .map(|(name, is_dir)| DirEntry { name, is_dir })
                .collect(),
        ))
    }

    async fn read_file(
        &self,
        owner: &str,
        name: &str,
        rev: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>> {
        let files = self.files.lock().expect("never poisoned");
        Ok(files
            .get(&(format!("{owner}/{name}"), rev.to_owned()))
            .and_then(|commit| commit.get(path))
            .map(|contents| contents.clone().into_bytes()))
    }
}

/// The `cue.mod/module.cue` of a repository whose configuration imports no other module.
pub const PLAIN_MODULE: &str = "module: \"example.com/acme\"\nlanguage: version: \"v0.14.1\"\n";

/// The prefix of the module paths test packages are published under.
pub const MODULE_PREFIX: &str = "tartan.test";

/// The module path a test package with extension id `id` is published under:
/// `tartan.test/<id's dot-separated parts>`, with `-` as `_`.
pub fn module_path(id: &str) -> String {
    format!("{MODULE_PREFIX}/{}", id.replace('.', "/").replace('-', "_"))
}

/// The CUE package name of the module for extension `id`: its path's last element.
pub fn module_package(id: &str) -> String {
    module_path(id)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The source registry test modules are published to, and the modules published so far.
#[derive(Default)]
pub struct ModuleSource {
    published: Mutex<Vec<ModuleVersion>>,
}

impl ModuleSource {
    /// The source registry's `host:port`.
    pub fn address() -> String {
        std::env::var("TARTAN_MODULE_SOURCE")
            .expect("TARTAN_MODULE_SOURCE names the source registry (see scripts/e2e.sh)")
    }

    /// Publishes `package`, with `manifest` in place of its own, as module version `version`
    /// (`v` and the manifest's version unless given), and remembers it for the next sync.
    pub async fn publish(
        &self,
        package: &ExtensionPackage,
        manifest: &Value,
        version: Option<&str>,
    ) -> ModuleVersion {
        let id = manifest["id"].as_str().expect("a manifest id");
        let version = match version {
            Some(version) => version.to_owned(),
            None => format!("v{}", manifest["version"].as_str().expect("a version")),
        };
        let module = ModuleVersion {
            path: module_path(id),
            version,
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let write = |path: &str, bytes: &[u8]| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(path, bytes).expect("write");
        };
        write(
            "cue.mod/module.cue",
            format!(
                "module: \"{}@{}\"\nlanguage: version: \"v0.14.1\"\nsource: kind: \"self\"\n",
                module.path,
                module.major()
            )
            .as_bytes(),
        );
        let package_name = module_package(id);
        let manifest_version = manifest["version"].as_str().unwrap_or_default();
        write(
            &format!("{package_name}.cue"),
            format!(
                "package {package_name}\n\n#Extension: {{\n\tid: \"{id}\"\n\tversion: \"{manifest_version}\"\n\tsettings: {{...}}\n}}\n"
            )
            .as_bytes(),
        );
        write(
            "tartan.json",
            &serde_json::to_vec_pretty(manifest).expect("encode"),
        );
        write(COMPONENT_FILE, &package.component);
        for (path, sql) in &package.migrations {
            write(path, sql.as_bytes());
        }
        cue_mod_publish(dir.path(), &module.version).await;
        let mut published = self.published.lock().expect("never poisoned");
        if !published.contains(&module) {
            published.push(module.clone());
        }
        module
    }

    /// Adds the first-party modules `scripts/e2e.sh` published (see [`extension_modules`]) to
    /// the modules the next sync lists, and returns them.
    pub fn add_extension_modules(&self) -> Vec<ModuleVersion> {
        let modules = extension_modules();
        let mut published = self.published.lock().expect("never poisoned");
        for module in &modules {
            if !published.contains(module) {
                published.push(module.clone());
            }
        }
        modules
    }

    /// A deployment listing every module published so far, from the source registry: the test
    /// packages under [`MODULE_PREFIX`] and the first-party modules under their own paths.
    pub fn deployment(&self) -> Deployment {
        let source = format!("{}+insecure", Self::address());
        Deployment {
            modules: self.published.lock().expect("never poisoned").clone(),
            sources: [MODULE_PREFIX, "tartan.rawkode.academy"]
                .into_iter()
                .map(|prefix| (prefix.to_owned(), source.parse().expect("a source")))
                .collect(),
        }
    }
}

/// The first-party modules (`tartan.rawkode.academy/types`, Issues' and Kanban's) that `scripts/e2e.sh`
/// built and published to the source registry, from `TARTAN_EXTENSION_MODULES`: a
/// space-separated list of `path@vX.Y.Z`.
pub fn extension_modules() -> Vec<ModuleVersion> {
    let list = std::env::var("TARTAN_EXTENSION_MODULES").expect(
        "TARTAN_EXTENSION_MODULES lists the published first-party modules (see scripts/e2e.sh)",
    );
    list.split_whitespace()
        .map(|module| {
            let (path, version) = module
                .split_once('@')
                .unwrap_or_else(|| panic!("{module} is not path@version"));
            ModuleVersion {
                path: path.to_owned(),
                version: version.to_owned(),
            }
        })
        .collect()
}

/// The `cue.mod/module.cue` of a repository whose configuration depends on every first-party
/// module `scripts/e2e.sh` published.
pub fn repository_module() -> String {
    let deps: String = extension_modules()
        .iter()
        .map(|module| {
            format!(
                "\t\"{}@{}\": v: \"{}\"\n",
                module.path,
                module.major(),
                module.version
            )
        })
        .collect();
    format!(
        "module: \"example.com/acme-api\"\nlanguage: version: \"v0.14.1\"\ndeps: {{\n{deps}}}\n"
    )
}

/// Runs `cue mod publish <version>` in `dir` against the source registry.
pub async fn cue_mod_publish(dir: &Path, version: &str) {
    let cue = std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_owned());
    let output = tokio::process::Command::new(&cue)
        .args(["mod", "publish", version])
        .current_dir(dir)
        .env(
            "CUE_REGISTRY",
            format!("{}+insecure", ModuleSource::address()),
        )
        .output()
        .await
        .unwrap_or_else(|error| panic!("run {cue}: {error}"));
    assert!(
        output.status.success(),
        "cue mod publish {version}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The gateway's listener: the address in `CUE_REGISTRY` when it is set, so the CUE evaluator
/// in this process resolves modules against this forge's registry, otherwise any free port.
pub async fn gateway_listener() -> tokio::net::TcpListener {
    let address = match std::env::var("CUE_REGISTRY") {
        Ok(registry) if !registry.is_empty() => {
            let location = registry.split('+').next().unwrap_or_default();
            location.split('/').next().unwrap_or_default().to_owned()
        }
        _ => "127.0.0.1:0".to_owned(),
    };
    tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|error| panic!("bind the gateway to {address}: {error}"))
}

/// Syncs `deployment`'s modules into a forge's registry, as the forge does at startup.
pub async fn sync(
    deployment: &Deployment,
    registry: &RegistryStore,
    client: &Client,
    extensions: &Extensions,
) -> SyncReport {
    let targets = SyncTargets {
        registry,
        client,
        extensions,
    };
    sync_modules(deployment, &targets).await
}

/// A running forge.
pub struct Forge {
    pub base: String,
    pub http: reqwest::Client,
    pub git: Arc<FakeGitHost>,
    pub registry: RegistryStore,
    pub extensions: Arc<Extensions>,
    pub modules: ModuleSource,
    client: Client,
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
        let store: Arc<dyn object_store::ObjectStore> =
            Arc::new(object_store::memory::InMemory::new());
        let registry = RegistryStore::new(store.clone());
        let extensions = Arc::new(Extensions::new(
            ExtensionRuntime::new().expect("runtime"),
            store,
        ));
        install_services(Services {
            forge_id: format!("{name}-{nanos}"),
            git_host: git.clone(),
            extensions: extensions.clone(),
        })
        .expect("installed once");
        let mut actor_registry = Registry::new();
        register_actors(&mut actor_registry);
        let actors = rivetkit::test::setup(actor_registry)
            .await
            .expect("engine and registry");
        let client = actors.client().clone();
        let state = GatewayState {
            client: client.clone(),
            admin_token: Arc::from(ADMIN_TOKEN),
            webhook_secret: Arc::from(WEBHOOK_SECRET),
            registry: registry.clone(),
        };
        let listener = gateway_listener().await;
        let base = format!("http://{}", listener.local_addr().expect("address"));
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        // A forge starts with its deployment's modules synced; this one lists none yet.
        sync(&Deployment::default(), &registry, &client, &extensions).await;
        Self {
            base,
            http: reqwest::Client::new(),
            git,
            registry,
            extensions,
            modules: ModuleSource::default(),
            client,
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

    /// Publishes `package` as a module, with `manifest` in place of its own, and syncs every
    /// module published so far into the forge, as a restart with them listed would.
    pub async fn publish(&self, package: &ExtensionPackage, manifest: &Value) -> SyncReport {
        self.modules.publish(package, manifest, None).await;
        self.sync().await
    }

    /// Lists the first-party modules `scripts/e2e.sh` published and syncs them into the forge,
    /// as a deployment listing them would at startup.
    pub async fn sync_extension_modules(&self) -> SyncReport {
        self.modules.add_extension_modules();
        self.sync().await
    }

    /// Syncs every module published so far.
    pub async fn sync(&self) -> SyncReport {
        self.sync_deployment(&self.modules.deployment()).await
    }

    /// Syncs `deployment`'s modules.
    pub async fn sync_deployment(&self, deployment: &Deployment) -> SyncReport {
        sync(deployment, &self.registry, &self.client, &self.extensions).await
    }

    /// Pushes a commit to `repo`'s `main` with `config` as its `tartan.cue` (no file for `None`),
    /// in a module without dependencies ([`PLAIN_MODULE`]), and returns the commit.
    pub async fn push_config(&self, repo: &str, config: Option<&str>) -> String {
        let files: Vec<(&str, &str)> = config
            .map(|c| vec![("cue.mod/module.cue", PLAIN_MODULE), ("tartan.cue", c)])
            .unwrap_or_default();
        self.push_files(repo, &files).await
    }

    /// Pushes a commit to `repo`'s `main` holding exactly `files`, and returns the commit.
    pub async fn push_files(&self, repo: &str, files: &[(&str, &str)]) -> String {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let sha = format!("{seq:040x}");
        let commit = files
            .iter()
            .map(|(path, contents)| ((*path).to_owned(), (*contents).to_owned()))
            .collect();
        self.git
            .files
            .lock()
            .expect("never poisoned")
            .insert((repo.to_owned(), sha.clone()), commit);
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
        self.git.files.lock().expect("never poisoned").insert(
            (repo.to_owned(), sha.clone()),
            BTreeMap::from([
                ("cue.mod/module.cue".to_owned(), PLAIN_MODULE.to_owned()),
                ("tartan.cue".to_owned(), config.to_owned()),
            ]),
        );
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

    /// The installations on repository `path` (`org/repo`), as `id@version`.
    pub async fn installed(&self, path: &str) -> Vec<String> {
        let (org, repo) = path.split_once('/').expect("org/repo");
        let route = format!("/-/api/orgs/{org}/repos/{repo}/extensions");
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
