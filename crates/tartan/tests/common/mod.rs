//! The forge for end-to-end tests: the real Rivet engine and gateway, the registry and packages
//! in memory, and a Git host whose pushes are simulated: a pushed `tartan.cue` is stored per
//! commit and announced with a signed ref event for `main`.
//!
//! Identity runs against an in-process `tartan-devnet` (PLC directory, PDS and authorization
//! server). Its `alice.test` is the forge's owner, and the harness acts as her through an
//! access token minted by the identity actor's operator path, as `tt admin token create` does.
//! The gateway's public URL is its own origin, since no web server runs.
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
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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
use tartan::settings::{PublicUrl, PublicUrlRules, WebClient};
use tartan::{
    Access, AccessConfig, GatewayState, RegistryStore, SyncReport, SyncTargets, router,
    sync_modules,
};
use tartan_atproto::Mode;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tartan_atproto::resolver::{ResolverConfig, VerifyingResolver};
use tartan_config::DirEntry;
use tartan_config::deployment::{Deployment, ModuleVersion, Readers};
use tartan_devnet::{Devnet, DevnetConfig};
use tartan_extensions::{COMPONENT_FILE, ExtensionPackage, ExtensionRuntime};
use tartan_forge::{
    Clock, Extensions, GitHost, Services, identity_handle, install_services, register_actors,
};
use tartan_identity_api::{AdminCreateToken, TokenRequest, TokenScope};

pub const WEBHOOK_SECRET: &[u8] = b"test-webhook-secret";
pub const DEADLINE: Duration = Duration::from_secs(60);

/// The forge's clock in tests (design D12, *Time in tests*): wall-clock time plus an offset the
/// test advances. Only this harness has it; the `tartan` binary always runs on real time.
#[derive(Debug, Default)]
pub struct TestClock {
    offset_ms: AtomicI64,
}

impl TestClock {
    /// Moves the forge's time forward by `by`.
    pub fn advance(&self, by: Duration) {
        let by =
            i64::try_from(by.as_millis()).expect("a test advances by less than 292 million years");
        self.offset_ms.fetch_add(by, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_millis();
        i64::try_from(wall).expect("fits") + self.offset_ms()
    }

    fn offset_ms(&self) -> i64 {
        self.offset_ms.load(Ordering::SeqCst)
    }
}

/// An identity resolver with production rules (public HTTPS, `plc.directory`), for tests that
/// reach no identity network.
pub fn offline_resolver() -> VerifyingResolver {
    let fetch = FetchClient::new(FetchConfig::new(Mode::Production)).expect("a fetch client");
    let config = ResolverConfig::production().expect("the system DNS configuration");
    VerifyingResolver::new(fetch, config)
}

/// The forge's identity side in tests: a devnet, a resolver over it, the web client's key and
/// the owner's DID.
pub struct IdentityWorld {
    pub devnet: Arc<Devnet>,
    pub resolver: VerifyingResolver,
    pub keyset: jacquard_oauth::keyset::Keyset,
    pub owner: String,
}

impl IdentityWorld {
    /// Starts a devnet with the seed people; `alice.test` is the owner.
    pub async fn start() -> Self {
        let devnet = Devnet::start(DevnetConfig::default())
            .await
            .expect("the devnet starts");
        let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("a fetch client");
        let resolver = VerifyingResolver::new(
            fetch,
            ResolverConfig::development(devnet.url(), devnet.url()),
        );
        let keyset =
            tartan::settings::parse_keyset(&test_jwks()).expect("the test client key is valid");
        let owner = devnet.did("alice.test").await.expect("alice");
        Self {
            devnet: Arc::new(devnet),
            resolver,
            keyset,
            owner,
        }
    }

    /// The gateway's access state for a gateway at `base`.
    pub fn access(&self, base: &str, readers: Readers, clock: Arc<dyn Clock>) -> Arc<Access> {
        self.access_with(base, readers, clock, Vec::new())
    }

    /// The gateway's access state, trusting `proxies`' `X-Forwarded-For`.
    pub fn access_with(
        &self,
        base: &str,
        readers: Readers,
        clock: Arc<dyn Clock>,
        proxies: Vec<ipnet::IpNet>,
    ) -> Arc<Access> {
        self.access_client(
            base,
            readers,
            clock,
            proxies,
            WebClient::Confidential(self.keyset.clone()),
        )
    }

    /// The gateway's access state with `client` as its web sign-in client.
    pub fn access_client(
        &self,
        base: &str,
        readers: Readers,
        clock: Arc<dyn Clock>,
        proxies: Vec<ipnet::IpNet>,
        client: WebClient,
    ) -> Arc<Access> {
        Arc::new(Access::new(AccessConfig {
            public_url: PublicUrl::parse(base, PublicUrlRules::Development).expect("public url"),
            client,
            resolver: self.resolver.clone(),
            owners: vec![self.owner.clone()],
            readers,
            trusted_proxies: proxies,
            clock,
        }))
    }
}

/// A private ES256 JWKS for the test forge's web client.
pub fn test_jwks() -> String {
    let secret = p256::SecretKey::from_slice(&[42_u8; 32]).expect("a key");
    let mut jwk = serde_json::to_value(jose_jwk::Jwk {
        key: jose_jwk::Key::from(&jose_jwk::crypto::Key::from(secret)),
        prm: Default::default(),
    })
    .expect("jwk");
    jwk["kid"] = json!("test-1");
    json!({ "keys": [jwk] }).to_string()
}

/// Mints an access token for `did` through the identity actor's operator path.
pub async fn admin_token(client: &Client, did: &str, scopes: Vec<TokenScope>) -> String {
    identity_handle(client)
        .expect("the identity actor")
        .send(AdminCreateToken {
            did: did.to_owned(),
            request: TokenRequest {
                name: "tests".to_owned(),
                scopes,
                repos: None,
                expires_in_days: 30,
            },
        })
        .await
        .expect("the identity actor answers")
        .expect("an owner's token")
        .secret
}

/// Every token scope.
pub fn all_scopes() -> Vec<TokenScope> {
    vec![TokenScope::Read, TokenScope::Write, TokenScope::Admin]
}

/// Serves `router` on `listener` with each request's peer address, as the binary does.
pub fn serve(listener: tokio::net::TcpListener, router: axum::Router) {
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
}

/// The `name=value` pairs of a response's `Set-Cookie` headers.
pub fn set_cookies(response: &reqwest::Response) -> HashMap<String, String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// A browser for sign-in tests: no redirects followed, no proxy, cookies kept by hand.
pub fn browser() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a client")
}

/// The first half of a sign-in: the login answer.
#[derive(Debug)]
pub struct Started {
    pub status: u16,
    pub location: String,
    pub cookies: HashMap<String, String>,
    pub retry_after: Option<String>,
}

/// Posts the sign-in form for `handle` as a browser at `base` would.
pub async fn start_sign_in(
    base: &str,
    handle: &str,
    return_to: &str,
    forwarded_for: Option<&str>,
) -> Started {
    let mut request = browser()
        .post(format!("{base}/-/auth/login"))
        .header("origin", base)
        .form(&[("handle", handle), ("return_to", return_to)]);
    if let Some(forwarded) = forwarded_for {
        request = request.header("x-forwarded-for", forwarded);
    }
    let response = request.send().await.expect("login answers");
    Started {
        status: response.status().as_u16(),
        location: response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        cookies: set_cookies(&response),
        retry_after: response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    }
}

/// Follows the devnet's auto-approving authorize page and answers the callback URL.
pub async fn approve(location: &str) -> String {
    let approved = browser()
        .get(location)
        .send()
        .await
        .expect("authorize answers");
    assert_eq!(
        approved.status(),
        303,
        "authorize: {:?}",
        approved.text().await
    );
    approved.headers()["location"]
        .to_str()
        .expect("location")
        .to_owned()
}

/// What a callback answered.
#[derive(Debug)]
pub struct Finished {
    /// The callback's status.
    pub status: u16,
    /// Where it sent the browser.
    pub location: String,
    /// The cookies it set (an empty value clears a cookie).
    pub cookies: HashMap<String, String>,
}

impl Finished {
    /// The session cookie's value.
    pub fn session(&self) -> String {
        self.cookies
            .get("tartan_session")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| panic!("no session: {self:?}"))
            .clone()
    }

    /// The query of the sign-in page the browser was sent to, decoded.
    pub fn problem(&self) -> HashMap<String, String> {
        problem_query(&self.location)
    }
}

/// The decoded query of a `/#/-/sign-in?…` location.
pub fn problem_query(location: &str) -> HashMap<String, String> {
    let query = location
        .split_once("/#/-/sign-in?")
        .map(|(_, q)| q)
        .unwrap_or_default();
    reqwest::Url::parse(&format!("http://x/?{query}"))
        .expect("a query")
        .query_pairs()
        .into_owned()
        .collect()
}

/// Calls the callback URL with `cookie` as the `Cookie` header.
pub async fn finish(callback: &str, cookie: &str) -> Finished {
    let response = browser()
        .get(callback)
        .header("cookie", cookie)
        .send()
        .await
        .expect("callback answers");
    Finished {
        status: response.status().as_u16(),
        location: response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
        cookies: set_cookies(&response),
    }
}

/// Signs `handle` in through the whole OAuth flow; `cookie` is anything else the browser holds.
pub async fn sign_in(base: &str, handle: &str, return_to: &str, cookie: Option<&str>) -> Finished {
    let started = start_sign_in(base, handle, return_to, None).await;
    assert_eq!(started.status, 303, "login: {started:?}");
    let pre_auth = started
        .cookies
        .get("tartan_oauth")
        .unwrap_or_else(|| panic!("no pre-auth cookie: {started:?}"))
        .clone();
    let callback = approve(&started.location).await;
    let mut cookies = format!("tartan_oauth={pre_auth}");
    if let Some(cookie) = cookie {
        cookies.push_str("; ");
        cookies.push_str(cookie);
    }
    finish(&callback, &cookies).await
}

/// `text` percent-encoded as the devnet and the forge write query values.
pub fn percent(text: &str) -> String {
    tartan::auth::percent_encode(text)
}

/// The `host:port` of a URL, as outcome details name a server.
pub fn host(url: &str) -> String {
    let url = reqwest::Url::parse(url).expect("a URL");
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_owned(),
    }
}

/// Git's empty tree.
pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The content of a commit with `parents`, made at `time` (Unix seconds), unsigned.
pub fn commit_bytes(parents: &[&str], message: &str, time: i64) -> Vec<u8> {
    let mut text = format!("tree {EMPTY_TREE}\n");
    for parent in parents {
        text.push_str(&format!("parent {parent}\n"));
    }
    text.push_str(&format!(
        "author Tests <tests@example.com> {time} +0000\ncommitter Tests <tests@example.com> {time} +0000\n\n{message}\n"
    ));
    text.into_bytes()
}

/// A Git host that keeps each pushed commit's files and every object pushed, per repository.
#[derive(Default)]
pub struct FakeGitHost {
    pub created: Mutex<Vec<String>>,
    /// `(owner/name, sha)` to the commit's files, by path.
    files: Mutex<HashMap<(String, String), BTreeMap<String, String>>>,
    objects: Mutex<HashMap<String, Arc<tartan_forge::MemoryObjects>>>,
    /// While set, reading objects fails as an unreachable host does.
    pub failing: std::sync::atomic::AtomicBool,
    /// How many times objects were read.
    pub reads: AtomicU64,
}

impl FakeGitHost {
    /// The objects of `repo` (`owner/name`).
    pub fn objects(&self, repo: &str) -> Arc<tartan_forge::MemoryObjects> {
        self.objects
            .lock()
            .expect("never poisoned")
            .entry(repo.to_owned())
            .or_default()
            .clone()
    }

    /// Stores an object in `repo` and returns its id.
    pub fn insert(&self, repo: &str, kind: &str, bytes: Vec<u8>) -> String {
        self.objects(repo).insert(kind, bytes)
    }

    /// Stores `files` as the files of commit `sha` in `repo`.
    pub fn set_files(&self, repo: &str, sha: &str, files: BTreeMap<String, String>) {
        self.files
            .lock()
            .expect("never poisoned")
            .insert((repo.to_owned(), sha.to_owned()), files);
    }
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

    async fn read_new_objects(
        &self,
        owner: &str,
        name: &str,
        new: &str,
        hidden: &[String],
        limit: usize,
        skip: &(dyn for<'s> Fn(&'s str) -> bool + Send + Sync),
    ) -> Result<tartan_forge::Walk> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.failing.load(Ordering::SeqCst) {
            anyhow::bail!("git.test: connection refused");
        }
        self.objects(&format!("{owner}/{name}"))
            .read_new_objects(new, hidden, limit, skip)
    }

    async fn is_ancestor(
        &self,
        owner: &str,
        name: &str,
        ancestor: &str,
        head: &str,
    ) -> Result<bool> {
        if self.failing.load(Ordering::SeqCst) {
            anyhow::bail!("git.test: connection refused");
        }
        self.objects(&format!("{owner}/{name}"))
            .is_ancestor(ancestor, head)
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
            ..Deployment::default()
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
    pub clock: Arc<TestClock>,
    pub identity: IdentityWorld,
    /// The owner's access token, with every scope.
    pub token: String,
    pub client: Client,
    actors: Option<TestHandle>,
    seq: AtomicU64,
    heads: Mutex<HashMap<String, String>>,
}

impl Forge {
    /// Starts the engine, the actors and the gateway; `name` keeps this run's forge apart from
    /// earlier runs on the same engine.
    pub async fn start(name: &str) -> Self {
        Self::start_with(
            name,
            Readers::default(),
            tartan_forge::IdentityLimits::default(),
        )
        .await
    }

    /// Starts a forge whose deployment admits `readers` and whose identity actor enforces
    /// `identity_limits` (tests lower caps so they can be reached).
    pub async fn start_with(
        name: &str,
        readers: Readers,
        identity_limits: tartan_forge::IdentityLimits,
    ) -> Self {
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
            store.clone(),
        ));
        let clock = Arc::new(TestClock::default());
        let identity = IdentityWorld::start().await;
        install_services(Services {
            forge_id: format!("{name}-{nanos}"),
            git_host: git.clone(),
            extensions: extensions.clone(),
            clock: clock.clone(),
            owners: vec![identity.owner.clone()],
            readers: readers.clone(),
            resolver: identity.resolver.clone(),
            identity_limits,
            objects: store.clone(),
            state_dir: std::env::temp_dir().join(format!("tartan-test-{name}-{nanos}")),
        })
        .expect("installed once");
        let mut actor_registry = Registry::new();
        register_actors(&mut actor_registry);
        let actors = rivetkit::test::setup(actor_registry)
            .await
            .expect("engine and registry");
        let client = actors.client().clone();
        let listener = gateway_listener().await;
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let state = GatewayState {
            client: client.clone(),
            access: identity.access(&base, readers, clock.clone()),
            webhook_secret: Arc::from(WEBHOOK_SECRET),
            registry: registry.clone(),
        };
        serve(listener, router(state));
        let token = admin_token(&client, &identity.owner, all_scopes()).await;
        // A forge starts with its deployment's modules synced; this one lists none yet.
        sync(&Deployment::default(), &registry, &client, &extensions).await;
        Self {
            base,
            http: reqwest::Client::new(),
            git,
            registry,
            extensions,
            modules: ModuleSource::default(),
            clock,
            identity,
            token,
            client,
            actors: Some(actors),
            seq: AtomicU64::new(1),
            heads: Mutex::new(HashMap::new()),
        }
    }

    /// Starts another gateway over the same actors, with its own `readers` and trusted proxies,
    /// and answers its origin. Rights are computed in the gateway, so each gateway shows the
    /// forge as its read mode would.
    pub async fn gateway(&self, readers: Readers, proxies: Vec<ipnet::IpNet>) -> String {
        let keyset = self.identity.keyset.clone();
        self.gateway_with(readers, proxies, move |_| WebClient::Confidential(keyset))
            .await
    }

    /// Starts another gateway whose web sign-in client `client` builds from its origin.
    pub async fn gateway_with(
        &self,
        readers: Readers,
        proxies: Vec<ipnet::IpNet>,
        client: impl FnOnce(&str) -> WebClient,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a gateway");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let state = GatewayState {
            client: self.client.clone(),
            access: self.identity.access_client(
                &base,
                readers,
                self.clock.clone(),
                proxies,
                client(&base),
            ),
            webhook_secret: Arc::from(WEBHOOK_SECRET),
            registry: self.registry.clone(),
        };
        serve(listener, router(state));
        base
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
            .bearer_auth(&self.token);
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
        let old = self
            .heads
            .lock()
            .expect("never poisoned")
            .get(repo)
            .cloned();
        let parents: Vec<&str> = old.iter().map(String::as_str).collect();
        let sha = self.git.insert(
            repo,
            "commit",
            commit_bytes(&parents, &format!("push {seq}"), commit_time(seq)),
        );
        self.git.set_files(
            repo,
            &sha,
            files
                .iter()
                .map(|(path, contents)| ((*path).to_owned(), (*contents).to_owned()))
                .collect(),
        );
        self.heads
            .lock()
            .expect("never poisoned")
            .insert(repo.to_owned(), sha.clone());
        let old = old.unwrap_or_else(|| "0".repeat(40));
        let event = json!([ref_event(repo, seq, "refs/heads/main", &old, &sha)]);
        assert_eq!(self.hook(&event).await, 204, "the push is recorded");
        sha
    }

    /// Pushes a new branch `branch` of `repo` at a root commit holding `config` as its
    /// `tartan.cue`.
    pub async fn push_branch(&self, repo: &str, branch: &str, config: &str) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let sha = self.git.insert(
            repo,
            "commit",
            commit_bytes(&[], &format!("push {seq}"), commit_time(seq)),
        );
        self.git.set_files(
            repo,
            &sha,
            BTreeMap::from([
                ("cue.mod/module.cue".to_owned(), PLAIN_MODULE.to_owned()),
                ("tartan.cue".to_owned(), config.to_owned()),
            ]),
        );
        let event = json!([ref_event(repo, seq, branch, &"0".repeat(40), &sha)]);
        assert_eq!(self.hook(&event).await, 204);
    }

    /// Records a push of `ref_name` in `repo` from `old` to `new`, objects already stored, and
    /// makes `new` `main`'s head when `ref_name` is `main`.
    pub async fn push_ref(&self, repo: &str, ref_name: &str, old: &str, new: &str) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        if ref_name == "refs/heads/main" {
            self.heads
                .lock()
                .expect("never poisoned")
                .insert(repo.to_owned(), new.to_owned());
        }
        let event = json!([ref_event(repo, seq, ref_name, old, new)]);
        assert_eq!(self.hook(&event).await, 204, "the push is recorded");
    }

    /// `main`'s head in `repo`, if pushed.
    pub fn head(&self, repo: &str) -> Option<String> {
        self.heads
            .lock()
            .expect("never poisoned")
            .get(repo)
            .cloned()
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

/// The commit time of the commit made for push `seq`, increasing with it.
pub fn commit_time(seq: u64) -> i64 {
    1_700_000_000 + i64::try_from(seq).expect("small")
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
