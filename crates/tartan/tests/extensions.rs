//! Extensions over HTTP: publish the Issues package, install it on a repository and use it,
//! with the real Rivet engine.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH` and `TARTAN_ISSUES_PACKAGE` (see `scripts/e2e.sh`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use rivetkit::Registry;
use serde_json::{Value, json};
use tartan::{GatewayState, router};
use tartan_extensions::{ExtensionPackage, ExtensionRuntime};
use tartan_forge::{Extensions, GitHost, Services, install_services, register_actors};

const ADMIN_TOKEN: &str = "extensions-test-admin-token";
const DEADLINE: Duration = Duration::from_secs(60);

struct NoGitHost;

#[async_trait]
impl GitHost for NoGitHost {
    async fn create_repo(&self, _owner: &str, _name: &str) -> Result<()> {
        Ok(())
    }

    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("http://git.test/{owner}/{name}.git")
    }
}

struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    async fn send(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
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

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        self.send(reqwest::Method::GET, path, None).await
    }
}

fn upload(package: &ExtensionPackage) -> Value {
    json!({
        "manifest": package.manifest.raw,
        "component": base64::engine::general_purpose::STANDARD.encode(&package.component),
        "migrations": package
            .migrations
            .iter()
            .map(|(path, sql)| json!({"path": path, "sql": sql}))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs TARTAN_ISSUES_PACKAGE and a Rivet engine; run scripts/e2e.sh"]
async fn issues_installs_on_a_repository_and_answers_over_http() {
    let dir = std::env::var("TARTAN_ISSUES_PACKAGE").expect("TARTAN_ISSUES_PACKAGE");
    let package = ExtensionPackage::from_dir(dir.as_ref()).expect("a package");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let extensions = Arc::new(Extensions::new(
        ExtensionRuntime::new().expect("runtime"),
        Arc::new(object_store::memory::InMemory::new()),
    ));
    install_services(Services {
        forge_id: format!("extensions-test-{nanos}"),
        git_host: Arc::new(NoGitHost),
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
        webhook_secret: Arc::from(&b"unused"[..]),
        extensions,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move { axum::serve(listener, router(state)).await });
    let api = Api {
        base,
        http: reqwest::Client::new(),
    };

    let (status, _) = api
        .send(
            reqwest::Method::PUT,
            "/-/api/subscribers/audit",
            Some(json!({"pattern": ">"})),
        )
        .await;
    assert_eq!(status, 204);
    api.post("/-/api/orgs", json!({"slug": "acme"})).await;
    let (status, _) = api
        .post("/-/api/orgs/acme/repos", json!({"slug": "api"}))
        .await;
    assert_eq!(status, 201);

    // Publish: once, again (the same package), then refuse other contents under its version.
    let (status, published) = api.post("/-/api/packages", upload(&package)).await;
    assert_eq!(status, 201, "{published}");
    assert_eq!(published["id"], "rawkode.issues");
    let (status, _) = api.post("/-/api/packages", upload(&package)).await;
    assert_eq!(status, 200);
    let mut changed = package.clone();
    changed.migrations[0].1.push_str("\n-- changed\n");
    let (status, _) = api.post("/-/api/packages", upload(&changed)).await;
    assert_eq!(status, 409);
    let mut broken = package.clone();
    broken.component = b"not a component".to_vec();
    broken.manifest.raw["version"] = json!("9.9.9");
    let (status, _) = api.post("/-/api/packages", upload(&broken)).await;
    assert_eq!(status, 400);

    // Install on the repository.
    let version = package.manifest.version.clone();
    let install = json!({"package": "rawkode.issues", "version": version});
    let (status, installed) = api
        .post("/-/api/orgs/acme/repos/api/extensions", install.clone())
        .await;
    assert_eq!(status, 201, "{installed}");
    let (_, again) = api
        .post("/-/api/orgs/acme/repos/api/extensions", install)
        .await;
    assert_eq!(installed["installation_id"], again["installation_id"]);
    let (status, list) = api.get("/-/api/orgs/acme/repos/api/extensions").await;
    assert_eq!(status, 200);
    assert_eq!(list.as_array().map(Vec::len), Some(1));

    // Use it: its data lives in the installation actor's SQLite.
    let tools = "/-/api/orgs/acme/repos/api/extensions/rawkode.issues/tools";
    let (status, created) = api
        .post(
            &format!("{tools}/create"),
            json!({"title": "Over HTTP", "body": "Hello"}),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["issue"]["number"], 1);
    let (_, second) = api
        .post(&format!("{tools}/create"), json!({"title": "Second"}))
        .await;
    assert_eq!(second["issue"]["number"], 2);
    let (status, listed) = api.post(&format!("{tools}/list"), json!({})).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["open"], 2);
    let (status, refused) = api.post(&format!("{tools}/create"), json!({})).await;
    assert_eq!(
        status, 400,
        "an invalid issue is the extension's refusal: {refused}"
    );
    assert_eq!(refused["kind"], "invalid");
    let (status, _) = api.post(&format!("{tools}/nope"), json!({})).await;
    assert_eq!(status, 404);

    let (status, page) = api
        .post(
            "/-/api/orgs/acme/repos/api/extensions/rawkode.issues/render/issues",
            json!({}),
        )
        .await;
    assert_eq!(status, 200, "{page}");
    assert!(page.to_string().contains("Over HTTP"), "{page}");

    let (status, _) = api
        .post(
            "/-/api/orgs/acme/repos/other/extensions/rawkode.issues/tools/list",
            json!({}),
        )
        .await;
    assert_eq!(status, 404);

    // The forge recorded both as events.
    let start = Instant::now();
    loop {
        let (_, received) = api.get("/-/api/subscribers/audit/events?limit=500").await;
        let kinds: Vec<String> = received
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|r| r["event"]["type"].as_str().map(str::to_owned))
            .collect();
        if kinds.iter().any(|k| k == "package.published")
            && kinds.iter().any(|k| k == "extension.installed")
        {
            break;
        }
        assert!(start.elapsed() < DEADLINE, "events so far: {kinds:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    actors.shutdown().await;
}
