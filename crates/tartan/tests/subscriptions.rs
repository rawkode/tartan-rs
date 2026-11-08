//! An installed extension receives its repository's events through `on-event`, and what it
//! emits reaches the forge's subscribers.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH` and `TARTAN_FIXTURE_PACKAGE`, the package of
//! `extensions/fixture` (see `scripts/e2e.sh`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use rivetkit::Registry;
use serde_json::{Value, json};
use sha2::Sha256;
use tartan::{GatewayState, router};
use tartan_extensions::{ExtensionPackage, ExtensionRuntime};
use tartan_forge::{Extensions, GitHost, Services, install_services, register_actors};

const ADMIN_TOKEN: &str = "subscriptions-test-admin-token";
const WEBHOOK_SECRET: &[u8] = b"subscriptions-test-webhook-secret";
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
    async fn send(&self, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(ADMIN_TOKEN)
            .json(&body)
            .send()
            .await
            .expect("the gateway answers");
        let status = response.status().as_u16();
        let text = response.text().await.expect("a body");
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, body).await
    }

    async fn hook(&self, events: &Value) -> u16 {
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

    /// Polls `read` until `done` holds for its result.
    async fn until(
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
}

fn ref_event(seq: &str, name: &str, old: &str, new: &str) -> Value {
    json!({
        "action": if old.bytes().all(|b| b == b'0') { "create" } else { "update" },
        "ref_type": "branch",
        "ref_name": name,
        "old": old,
        "new": new,
        "pusher": "alice",
        "repo": "acme/api",
        "_walgit": {"schema_version": 1, "seq": seq, "entry_kind": "push"}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs TARTAN_FIXTURE_PACKAGE and a Rivet engine; run scripts/e2e.sh"]
async fn an_extension_receives_its_repository_events_and_emits_its_own() {
    let dir = std::env::var("TARTAN_FIXTURE_PACKAGE").expect("TARTAN_FIXTURE_PACKAGE");
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
        forge_id: format!("subscriptions-test-{nanos}"),
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
        webhook_secret: Arc::from(WEBHOOK_SECRET),
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
            "/-/api/subscribers/emitted",
            json!({"pattern": "x.>"}),
        )
        .await;
    assert_eq!(status, 204);
    api.post("/-/api/orgs", json!({"slug": "acme"})).await;
    api.post("/-/api/orgs/acme/repos", json!({"slug": "api"}))
        .await;
    api.post("/-/api/orgs/acme/repos", json!({"slug": "other"}))
        .await;
    let upload = json!({
        "manifest": package.manifest.raw,
        "component": base64::engine::general_purpose::STANDARD.encode(&package.component),
        "migrations": package.migrations.iter()
            .map(|(path, sql)| json!({"path": path, "sql": sql}))
            .collect::<Vec<_>>(),
    });
    let (status, published) = api.post("/-/api/packages", upload).await;
    assert_eq!(status, 201, "{published}");
    let (status, installed) = api
        .post(
            "/-/api/orgs/acme/repos/api/extensions",
            json!({"package": "tartan.fixture", "version": "0.1.0"}),
        )
        .await;
    assert_eq!(status, 201, "{installed}");

    let zero = "0".repeat(40);
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let batch = json!([
        ref_event("1", "refs/heads/main", &zero, &a),
        ref_event("2", "refs/heads/fail", &zero, &a),
        ref_event("3", "refs/heads/main", &a, &b),
    ]);
    assert_eq!(api.hook(&batch).await, 204);
    assert_eq!(
        api.hook(&batch).await,
        204,
        "a repeated delivery is accepted"
    );
    // Another repository's events never reach this installation.
    let elsewhere = json!([{
        "action": "create", "ref_type": "branch", "ref_name": "refs/heads/main",
        "old": zero, "new": a, "pusher": "bob", "repo": "acme/other",
        "_walgit": {"schema_version": 1, "seq": "1", "entry_kind": "push"}
    }]);
    assert_eq!(api.hook(&elsewhere).await, 204);

    // The extension recorded the two ref updates it could handle, once each; the failing one
    // became a dead letter without blocking the update after it.
    let seen = api
        .until(
            "the fixture's seen tool",
            async || {
                api.post(
                    "/-/api/orgs/acme/repos/api/extensions/tartan.fixture/tools/seen",
                    json!({}),
                )
                .await
                .1
            },
            |value| value["seen"].as_array().is_some_and(|seen| seen.len() >= 2),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, settled) = api
        .post(
            "/-/api/orgs/acme/repos/api/extensions/tartan.fixture/tools/seen",
            json!({}),
        )
        .await;
    assert_eq!(settled, seen, "nothing more arrived");
    let seen = seen["seen"].as_array().expect("a list");
    assert_eq!(seen.len(), 2, "{seen:#?}");
    let repo_source = seen[0]["source"].as_str().expect("a source");
    assert!(repo_source.starts_with("repo:"), "{repo_source}");
    assert!(
        seen.iter()
            .all(|s| s["type"] == "ref.updated" && s["source"] == repo_source)
    );

    // What it emitted reached the forge's subscriber.
    let emitted = api
        .until(
            "the emitted events",
            async || {
                api.send(
                    reqwest::Method::GET,
                    "/-/api/subscribers/emitted/events?limit=500",
                    Value::Null,
                )
                .await
                .1
            },
            |value| value.as_array().is_some_and(|events| events.len() >= 2),
        )
        .await;
    let emitted = emitted.as_array().expect("a list");
    assert_eq!(emitted.len(), 2, "{emitted:#?}");
    for received in emitted {
        assert_eq!(received["event"]["type"], "x.tartan.fixture.seen");
        assert_eq!(received["event"]["data"]["data"]["type"], "ref.updated");
    }

    actors.shutdown().await;
}
