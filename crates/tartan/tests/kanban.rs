//! Kanban boards over an org's issues, at three levels: the org, a repository and a project.
//! Cards follow Issues' events; moving a card updates the issue through a permitted call.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_ISSUES_PACKAGE` and `TARTAN_KANBAN_PACKAGE`
//! (see `scripts/e2e.sh`).

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

const ADMIN_TOKEN: &str = "kanban-test-admin-token";
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
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
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

    async fn ok(&self, path: &str, body: Value) -> Value {
        let (status, value) = self.post(path, body).await;
        assert!((200..300).contains(&status), "{path}: {status} {value}");
        value
    }

    /// Calls a Kanban tool on the org.
    async fn kanban(&self, tool: &str, args: Value) -> (u16, Value) {
        self.post(
            &format!("/-/api/orgs/acme/extensions/rawkode.kanban/tools/{tool}"),
            args,
        )
        .await
    }

    /// The board `id` once `done` holds for its view.
    async fn board_until(&self, id: &str, done: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let (status, view) = self.kanban("board", json!({"id": id})).await;
            assert_eq!(status, 200, "{view}");
            if done(&view) {
                return view;
            }
            assert!(start.elapsed() < DEADLINE, "board {id}: {view:#}");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn upload(package: &ExtensionPackage, manifest: &Value) -> Value {
    json!({
        "manifest": manifest,
        "component": base64::engine::general_purpose::STANDARD.encode(&package.component),
        "migrations": package.migrations.iter()
            .map(|(path, sql)| json!({"path": path, "sql": sql}))
            .collect::<Vec<_>>(),
    })
}

/// The cards in each column of a board view, as `repo#number`.
fn columns(view: &Value) -> Vec<(String, Vec<String>)> {
    view["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|column| {
            let cards = column["cards"]
                .as_array()
                .expect("cards")
                .iter()
                .map(|card| {
                    format!(
                        "{}#{}",
                        card["repo"].as_str().unwrap_or_default(),
                        card["number"]
                    )
                })
                .collect();
            (
                column["value"].as_str().unwrap_or_default().to_owned(),
                cards,
            )
        })
        .collect()
}

fn card_count(view: &Value) -> usize {
    columns(view).iter().map(|(_, cards)| cards.len()).sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the Issues and Kanban packages and a Rivet engine; run scripts/e2e.sh"]
async fn boards_for_the_org_a_repository_and_a_project() {
    let package = |name: &str| {
        let dir = std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
        ExtensionPackage::from_dir(dir.as_ref()).expect("a package")
    };
    let issues = package("TARTAN_ISSUES_PACKAGE");
    let kanban = package("TARTAN_KANBAN_PACKAGE");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let extensions = Arc::new(Extensions::new(
        ExtensionRuntime::new().expect("runtime"),
        Arc::new(object_store::memory::InMemory::new()),
    ));
    install_services(Services {
        forge_id: format!("kanban-test-{nanos}"),
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

    // An org with two repositories, each with Issues and a status field.
    api.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    let api_repo = api
        .ok("/-/api/orgs/acme/repos", json!({"slug": "api"}))
        .await;
    let web_repo = api
        .ok("/-/api/orgs/acme/repos", json!({"slug": "web"}))
        .await;
    let (api_id, web_id) = (
        api_repo["id"].as_str().expect("id").to_owned(),
        web_repo["id"].as_str().expect("id").to_owned(),
    );
    api.ok("/-/api/packages", upload(&issues, &issues.manifest.raw))
        .await;
    api.ok("/-/api/packages", upload(&kanban, &kanban.manifest.raw))
        .await;
    let status_field = json!({"fields": {"status": {
        "type": "select", "options": ["todo", "doing", "done"], "default": "todo"
    }}});
    for repo in ["api", "web"] {
        api.ok(
            &format!("/-/api/orgs/acme/repos/{repo}/extensions"),
            json!({"package": "rawkode.issues", "version": issues.manifest.version, "config": status_field}),
        )
        .await;
    }
    let issues_tool = |repo: &str, tool: &str| {
        format!("/-/api/orgs/acme/repos/{repo}/extensions/rawkode.issues/tools/{tool}")
    };

    // An issue filed before Kanban is installed still reaches its boards.
    api.ok(
        &issues_tool("api", "create"),
        json!({"title": "LSP crash", "project": "crates/lsp"}),
    )
    .await;
    api.ok(
        "/-/api/orgs/acme/extensions",
        json!({"package": "rawkode.kanban", "version": "0.1.0"}),
    )
    .await;
    api.ok(&issues_tool("api", "create"), json!({"title": "API docs"}))
        .await;
    api.ok(
        &issues_tool("web", "create"),
        json!({"title": "Web login", "fields": {"status": "doing"}}),
    )
    .await;

    // Three layers of boards over the same cards.
    let columns_spec = json!([
        {"value": "todo", "title": "To do"},
        {"value": "doing", "title": "Doing"},
        {"value": "done", "title": "Done"}
    ]);
    let boards = [
        json!({"id": "org", "name": "Everything", "field": "status", "columns": columns_spec,
               "scope": {"kind": "org"}}),
        json!({"id": "api", "name": "API", "field": "status", "columns": columns_spec,
               "scope": {"kind": "repo", "repo_id": api_id}}),
        json!({"id": "lsp", "name": "LSP", "field": "status", "columns": columns_spec,
               "scope": {"kind": "project", "repo_id": api_id, "project": "crates/lsp"}}),
        json!({"id": "state", "field": "state",
               "columns": [{"value": "open"}, {"value": "closed"}], "scope": {"kind": "org"}}),
    ];
    for board in boards {
        let (status, created) = api.kanban("create_board", board).await;
        assert_eq!(status, 200, "{created}");
    }
    let (status, _) = api
        .kanban("create_board", json!({"id": "Bad", "columns": []}))
        .await;
    assert_eq!(status, 400);
    let (_, listed) = api.kanban("list_boards", json!({})).await;
    assert_eq!(listed["boards"].as_array().map(Vec::len), Some(4));

    let org = api.board_until("org", |view| card_count(view) == 3).await;
    assert_eq!(
        columns(&org),
        vec![
            (
                "todo".to_owned(),
                vec!["acme/api#1".to_owned(), "acme/api#2".to_owned()]
            ),
            ("doing".to_owned(), vec!["acme/web#1".to_owned()]),
            ("done".to_owned(), vec![]),
        ]
    );
    let repo = api.board_until("api", |view| card_count(view) == 2).await;
    assert_eq!(card_count(&repo), 2);
    let project = api.board_until("lsp", |view| card_count(view) == 1).await;
    assert_eq!(columns(&project)[0].1, vec!["acme/api#1".to_owned()]);

    // Moving a card updates the issue in its own repository.
    let (status, moved) = api
        .kanban(
            "move",
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "done"}),
        )
        .await;
    assert_eq!(status, 200, "{moved}");
    let issue = api
        .ok(&issues_tool("web", "get"), json!({"number": 1}))
        .await;
    assert_eq!(issue["issue"]["fields"]["status"], "done", "{issue}");
    let org = api
        .board_until("org", |view| columns(view)[2].1.len() == 1)
        .await;
    assert_eq!(columns(&org)[2].1, vec!["acme/web#1".to_owned()]);

    // The board's UI action does the same, and a state board closes an issue.
    let (status, result) = api
        .post(
            "/-/api/orgs/acme/extensions/rawkode.kanban/actions/move",
            json!({"board": "api", "repo_id": api_id, "number": 2, "to": "doing"}),
        )
        .await;
    assert_eq!(status, 200, "{result}");
    let (status, _) = api
        .kanban(
            "move",
            json!({"board": "state", "repo_id": api_id, "number": 1, "to": "closed"}),
        )
        .await;
    assert_eq!(status, 200);
    let closed = api
        .ok(&issues_tool("api", "get"), json!({"number": 1}))
        .await;
    assert_eq!(closed["issue"]["state"], "closed");
    let (status, page) = api
        .post(
            "/-/api/orgs/acme/extensions/rawkode.kanban/render/board",
            json!({"board": "org"}),
        )
        .await;
    assert_eq!(status, 200, "{page}");
    assert!(page.to_string().contains("Web login"), "{page}");

    // Moves outside a board's columns or scope are refused.
    let (status, _) = api
        .kanban(
            "move",
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "nope"}),
        )
        .await;
    assert_eq!(status, 400);
    let (status, _) = api
        .kanban(
            "move",
            json!({"board": "lsp", "repo_id": web_id, "number": 1, "to": "todo"}),
        )
        .await;
    assert_eq!(status, 404);

    // The same board without permissions.calls may not touch issues.
    let mut locked = kanban.manifest.raw.clone();
    locked["id"] = json!("rawkode.kanban-locked");
    locked
        .as_object_mut()
        .expect("an object")
        .remove("permissions");
    api.ok("/-/api/packages", upload(&kanban, &locked)).await;
    api.ok(
        "/-/api/orgs/acme/extensions",
        json!({"package": "rawkode.kanban-locked", "version": "0.1.0"}),
    )
    .await;
    let locked_tool = "/-/api/orgs/acme/extensions/rawkode.kanban-locked/tools";
    api.ok(
        &format!("{locked_tool}/create_board"),
        json!({"id": "org", "field": "status", "columns": columns_spec}),
    )
    .await;
    let start = Instant::now();
    loop {
        let (_, view) = api
            .post(&format!("{locked_tool}/board"), json!({"id": "org"}))
            .await;
        if card_count(&view) == 3 {
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "the locked board's cards: {view}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let (status, refused) = api
        .post(
            &format!("{locked_tool}/move"),
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "todo"}),
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    let unchanged = api
        .ok(&issues_tool("web", "get"), json!({"number": 1}))
        .await;
    assert_eq!(unchanged["issue"]["fields"]["status"], "done");

    actors.shutdown().await;
}
