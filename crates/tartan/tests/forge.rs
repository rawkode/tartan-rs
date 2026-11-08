//! The forge end to end over HTTP, with the real Rivet engine and a fake Git host.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use rivetkit::{EngineSpawnMode, Registry, ServeConfig};
use serde_json::{Value, json};
use sha2::Sha256;
use tartan::{GatewayState, router};
use tartan_forge::{Extensions, GitHost, Services, install_services, register_actors};
use tokio_util::sync::CancellationToken;

const ADMIN_TOKEN: &str = "test-admin-token";
const WEBHOOK_SECRET: &[u8] = b"test-webhook-secret";
/// Where `rivetkit::test::setup` runs the engine.
const ENGINE: &str = "http://127.0.0.1:6420";
const DEADLINE: Duration = Duration::from_secs(60);

#[derive(Default)]
struct FakeGitHost {
    created: Mutex<Vec<String>>,
}

#[async_trait]
impl GitHost for FakeGitHost {
    async fn create_repo(&self, owner: &str, name: &str) -> Result<()> {
        self.created
            .lock()
            .expect("the lock is never poisoned")
            .push(format!("{owner}/{name}"));
        Ok(())
    }

    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("http://git.test/{owner}/{name}.git")
    }
}

struct Forge {
    base: String,
    http: reqwest::Client,
}

impl Forge {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
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
        let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, value)
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    async fn put(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, None).await
    }

    async fn post_hook(&self, body: &[u8], signature: &str) -> u16 {
        self.http
            .post(format!("{}/-/hooks/git-host", self.base))
            .header("x-walgit-signature", signature)
            .body(body.to_vec())
            .send()
            .await
            .expect("the gateway answers")
            .status()
            .as_u16()
    }

    /// Every event subscriber `name` has received, once `done` holds for them.
    async fn received_until(&self, name: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let start = Instant::now();
        loop {
            let (status, body) = self
                .get(&format!("/-/api/subscribers/{name}/events?limit=500"))
                .await;
            assert_eq!(status, 200, "{body}");
            let events: Vec<Value> = body
                .as_array()
                .expect("a list")
                .iter()
                .map(|received| received["event"].clone())
                .collect();
            if done(&events) {
                return events;
            }
            assert!(
                start.elapsed() < DEADLINE,
                "subscriber {name} received only {events:#?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET).expect("any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn ref_event(repo: &str, seq: &str, name: &str, old: &str, new: &str) -> Value {
    json!({
        "action": if old.bytes().all(|b| b == b'0') { "create" } else { "update" },
        "ref_type": "branch",
        "ref_name": name,
        "old": old,
        "new": new,
        "pusher": "alice",
        "correlation_id": "c1",
        "repo": repo,
        "_walgit": {"schema_version": 1, "seq": seq, "entry_kind": "push", "request_id": "r1"}
    })
}

/// One engine and one gateway for every scenario, which run in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forge_end_to_end() {
    let git_host = Arc::new(FakeGitHost::default());
    // The test engine keeps its data between runs, so each run is a new forge.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let extensions = Arc::new(Extensions::new(
        tartan_extensions::ExtensionRuntime::new().expect("runtime"),
        Arc::new(object_store::memory::InMemory::new()),
    ));
    install_services(Services {
        extensions: extensions.clone(),
        forge_id: format!("test-{nanos}"),
        git_host: git_host.clone(),
    })
    .expect("installed once");

    let mut registry = Registry::new();
    register_actors(&mut registry);
    let actors = rivetkit::test::setup(registry)
        .await
        .expect("engine and registry");

    // A second pool on the same engine, standing in for dedicated machines.
    let dedicated_pool = format!("dedicated-{nanos}");
    let shutdown = CancellationToken::new();
    let mut dedicated = Registry::new();
    register_actors(&mut dedicated);
    let dedicated_config = ServeConfig {
        endpoint: ENGINE.to_owned(),
        token: Some("default".to_owned()),
        namespace: "default".to_owned(),
        pool_name: dedicated_pool.clone(),
        engine_spawn: EngineSpawnMode::Never,
        ..ServeConfig::default()
    };
    tokio::spawn(dedicated.serve_with_config(dedicated_config, shutdown.clone()));

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
    let forge = Forge {
        base,
        http: reqwest::Client::new(),
    };

    the_api_requires_the_admin_token(&forge).await;
    orgs_and_repositories_emit_events(&forge, &git_host).await;
    ref_events_are_recorded_once_and_filtered_by_pattern(&forge).await;
    placement_puts_actors_in_their_pool(&forge, &dedicated_pool).await;

    shutdown.cancel();
    actors.shutdown().await;
}

async fn the_api_requires_the_admin_token(forge: &Forge) {
    let status = forge
        .http
        .post(format!("{}/-/api/orgs", forge.base))
        .json(&json!({"slug": "acme"}))
        .send()
        .await
        .expect("answers")
        .status()
        .as_u16();
    assert_eq!(status, 401);
    let (status, _) = forge
        .post("/-/api/orgs", json!({"slug": "Not Valid"}))
        .await;
    assert_eq!(status, 400);
}

async fn orgs_and_repositories_emit_events(forge: &Forge, git_host: &FakeGitHost) {
    let (status, _) = forge
        .put("/-/api/subscribers/audit", json!({"pattern": ">"}))
        .await;
    assert_eq!(status, 204);
    let (status, _) = forge
        .put("/-/api/subscribers/refs", json!({"pattern": "ref.*"}))
        .await;
    assert_eq!(status, 204);

    let (status, org) = forge.post("/-/api/orgs", json!({"slug": "acme"})).await;
    assert_eq!(status, 201, "{org}");
    let (status, again) = forge.post("/-/api/orgs", json!({"slug": "acme"})).await;
    assert_eq!(status, 201);
    assert_eq!(org["id"], again["id"], "creating an org is idempotent");

    let (status, repo) = forge
        .post("/-/api/orgs/acme/repos", json!({"slug": "api"}))
        .await;
    assert_eq!(status, 201, "{repo}");
    assert_eq!(repo["clone_url"], "http://git.test/acme/api.git");
    assert!(
        git_host
            .created
            .lock()
            .expect("never poisoned")
            .contains(&"acme/api".to_owned())
    );

    let (status, _) = forge.get("/-/api/orgs/acme/repos/nope").await;
    assert_eq!(status, 404);

    let wanted = ["org.created", "org.ready", "repo.created", "repo.ready"];
    let events = forge
        .received_until("audit", |events| {
            let seen = types(events);
            wanted.iter().all(|kind| seen.iter().any(|t| t == kind))
        })
        .await;
    let org_source = format!("org:{}", org["id"].as_str().expect("id"));
    let repo_source = format!("repo:{}", repo["id"].as_str().expect("id"));
    for (kind, source) in [
        ("org.created", "forge"),
        ("org.ready", org_source.as_str()),
        ("repo.created", org_source.as_str()),
        ("repo.ready", repo_source.as_str()),
    ] {
        let matching: Vec<_> = events.iter().filter(|e| e["type"] == kind).collect();
        assert_eq!(matching.len(), 1, "exactly one {kind}: {events:#?}");
        assert_eq!(matching[0]["source"], source, "{kind}");
    }
}

async fn ref_events_are_recorded_once_and_filtered_by_pattern(forge: &Forge) {
    let zero = "0".repeat(40);
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let batch = json!([
        ref_event("acme/api", "1", "refs/heads/main", &zero, &a),
        ref_event("acme/api", "1", "refs/heads/dev", &zero, &a),
        ref_event("acme/api", "2", "refs/heads/main", &a, &b),
        ref_event("elsewhere/unknown", "7", "refs/heads/main", &zero, &a),
    ]);
    let body = serde_json::to_vec(&batch).expect("encode");

    assert_eq!(forge.post_hook(&body, "sha256=00").await, 401);
    assert_eq!(forge.post_hook(&body, &sign(&body)).await, 204);
    // The host delivers at least once; a repeat must not duplicate anything.
    assert_eq!(forge.post_hook(&body, &sign(&body)).await, 204);

    let refs = forge
        .received_until("refs", |events| events.len() >= 3)
        .await;
    assert_eq!(types(&refs), vec!["ref.updated"; 3], "{refs:#?}");
    let updates: Vec<(String, String)> = refs
        .iter()
        .map(|e| {
            (
                e["data"]["ref_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                e["data"]["new"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        updates,
        vec![
            ("refs/heads/main".to_owned(), a.clone()),
            ("refs/heads/dev".to_owned(), a.clone()),
            ("refs/heads/main".to_owned(), b.clone()),
        ]
    );

    let audit = forge
        .received_until("audit", |events| {
            types(events)
                .iter()
                .filter(|t| *t == "push.accepted")
                .count()
                >= 2
                && types(events).iter().filter(|t| *t == "ref.updated").count() >= 3
        })
        .await;
    // Give a duplicate delivery time to show up before counting.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let audit = forge
        .received_until("audit", |e| e.len() >= audit.len())
        .await;
    let pushes = types(&audit)
        .iter()
        .filter(|t| *t == "push.accepted")
        .count();
    let updates = types(&audit).iter().filter(|t| *t == "ref.updated").count();
    assert_eq!((pushes, updates), (2, 3), "{audit:#?}");

    let mut seen = BTreeSet::new();
    let mut last_seq_by_source = std::collections::BTreeMap::new();
    for event in &audit {
        let source = event["source"].as_str().expect("source").to_owned();
        let seq = event["seq"].as_u64().expect("seq");
        assert!(
            seen.insert((source.clone(), seq)),
            "duplicate {source}:{seq}"
        );
        let last = last_seq_by_source.insert(source.clone(), seq).unwrap_or(0);
        assert!(seq > last, "{source} out of order: {seq} after {last}");
    }
}

async fn placement_puts_actors_in_their_pool(forge: &Forge, dedicated_pool: &str) {
    let (status, body) = forge
        .put(
            "/-/api/placements",
            json!({"target": "org/big", "pool": dedicated_pool}),
        )
        .await;
    assert_eq!(status, 204, "{body}");
    let (status, body) = forge
        .put(
            "/-/api/placements",
            json!({"target": "nonsense", "pool": "x"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");

    let (status, org) = forge.post("/-/api/orgs", json!({"slug": "big"})).await;
    assert_eq!(status, 201, "{org}");
    assert_eq!(org["pool"], dedicated_pool);
    let (status, org_status) = forge.get("/-/api/orgs/big").await;
    assert_eq!(status, 200, "{org_status}");
    assert_eq!(
        org_status["pool"], dedicated_pool,
        "the org actor runs in its pool"
    );

    let (status, repo) = forge
        .post("/-/api/orgs/big/repos", json!({"slug": "monorepo"}))
        .await;
    assert_eq!(status, 201, "{repo}");
    let (status, view) = forge.get("/-/api/orgs/big/repos/monorepo").await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(
        view["hosted_in"], dedicated_pool,
        "its repositories follow the org"
    );

    let (status, acme) = forge.get("/-/api/orgs/acme").await;
    assert_eq!(status, 200);
    assert_ne!(
        acme["pool"], dedicated_pool,
        "other orgs stay in the default pool"
    );
}
