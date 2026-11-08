//! Kanban boards over an org's issues, defined in `tartan.cue`: the org's boards in its
//! `tartan` repository, and a repository's and a project's boards contributed by that
//! repository. Cards follow Issues' events; moving a card updates the issue through a permitted
//! call.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_ISSUES_PACKAGE` and `TARTAN_KANBAN_PACKAGE`
//! (see `scripts/e2e.sh`).

mod common;

use common::{Forge, package};
use serde_json::{Value, json};

const COLUMNS: &str = r#"[{value: "todo", title: "To do"}, {value: "doing", title: "Doing"}, {value: "done", title: "Done"}]"#;

fn issues(version: &str) -> String {
    format!(
        r#"extensions: "rawkode.issues": {{
	version: "{version}"
	settings: fields: status: {{type: "select", options: ["todo", "doing", "done"], default: "todo"}}
}}
"#
    )
}

/// The API repository: Issues, and the boards it contributes to the org's Kanban.
fn api_config(issues_version: &str, lsp_name: &str) -> String {
    format!(
        r#"package tartan

{issues}
extensions: "rawkode.kanban": settings: boards: {{
	api: {{name: "API", field: "status", columns: {COLUMNS}}}
	lsp: {{name: "{lsp_name}", field: "status", project: "crates/lsp", columns: {COLUMNS}}}
}}
"#,
        issues = issues(issues_version)
    )
}

/// The org's configuration: Kanban with its org boards, and the same component without the
/// permission to call Issues.
fn org_config(kanban_version: &str) -> String {
    format!(
        r#"package tartan

let statuses = {COLUMNS}

extensions: "rawkode.kanban": {{
	version: "{kanban_version}"
	settings: boards: {{
		org: {{name: "Everything", field: "status", "columns": statuses}}
		web: {{field: "status", "columns": statuses, scope: {{kind: "repo", repo: "acme/web"}}}}
		state: {{field: "state", columns: [{{value: "open"}}, {{value: "closed"}}]}}
		bad: {{field: "status", columns: []}}
	}}
}}
extensions: "rawkode.kanban-locked": {{
	version: "{kanban_version}"
	settings: boards: org: {{field: "status", "columns": statuses}}
}}
"#
    )
}

/// The cards in each column of a board view, as `repo#number`.
fn columns(view: &Value) -> Vec<(String, Vec<String>)> {
    view["columns"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|column| {
                    let cards = column["cards"]
                        .as_array()
                        .map(|cards| {
                            cards
                                .iter()
                                .map(|card| {
                                    format!(
                                        "{}#{}",
                                        card["repo"].as_str().unwrap_or_default(),
                                        card["number"]
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (
                        column["value"].as_str().unwrap_or_default().to_owned(),
                        cards,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn card_count(view: &Value) -> usize {
    columns(view).iter().map(|(_, cards)| cards.len()).sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "migrated in integration"]
async fn boards_for_the_org_a_repository_and_a_project_from_configuration() {
    let issues_package = package("TARTAN_ISSUES_PACKAGE");
    let kanban_package = package("TARTAN_KANBAN_PACKAGE");
    let (issues_version, kanban_version) = (
        issues_package.manifest.version.clone(),
        kanban_package.manifest.version.clone(),
    );
    let forge = Forge::start("kanban-test").await;

    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    let api_repo = forge
        .ok("/-/api/orgs/acme/repos", json!({"slug": "api"}))
        .await;
    let web_repo = forge
        .ok("/-/api/orgs/acme/repos", json!({"slug": "web"}))
        .await;
    forge
        .ok("/-/api/orgs/acme/repos", json!({"slug": "tartan"}))
        .await;
    let (api_id, web_id) = (
        api_repo["id"].as_str().expect("id").to_owned(),
        web_repo["id"].as_str().expect("id").to_owned(),
    );
    forge
        .publish(&issues_package, &issues_package.manifest.raw)
        .await;
    forge
        .publish(&kanban_package, &kanban_package.manifest.raw)
        .await;
    let mut locked = kanban_package.manifest.raw.clone();
    locked["id"] = json!("rawkode.kanban-locked");
    locked
        .as_object_mut()
        .expect("an object")
        .remove("permissions");
    forge.publish(&kanban_package, &locked).await;

    // Issues on both repositories, from their configuration.
    forge
        .push_config("acme/api", Some(&api_config(&issues_version, "LSP")))
        .await;
    forge
        .push_config(
            "acme/web",
            Some(&format!("package tartan\n\n{}", issues(&issues_version))),
        )
        .await;
    let installed = format!("rawkode.issues@{issues_version}");
    forge.until_installed("acme/api", &[&installed]).await;
    forge.until_installed("acme/web", &[&installed]).await;
    let issues_tool = |repo: &str, tool: &str| {
        format!("/-/api/orgs/acme/repos/{repo}/extensions/rawkode.issues/tools/{tool}")
    };

    // An issue filed before Kanban is installed still reaches its boards.
    forge
        .ok(
            &issues_tool("api", "create"),
            json!({"title": "LSP crash", "project": "crates/lsp"}),
        )
        .await;
    forge
        .push_config("acme/tartan", Some(&org_config(&kanban_version)))
        .await;
    forge
        .until_installed(
            "acme",
            &[
                &format!("rawkode.kanban@{kanban_version}"),
                &format!("rawkode.kanban-locked@{kanban_version}"),
            ],
        )
        .await;
    forge
        .ok(&issues_tool("api", "create"), json!({"title": "API docs"}))
        .await;
    forge
        .ok(
            &issues_tool("web", "create"),
            json!({"title": "Web login", "fields": {"status": "doing"}}),
        )
        .await;

    let kanban = |tool: &str| format!("/-/api/orgs/acme/extensions/rawkode.kanban/tools/{tool}");
    let listed = forge.ok(&kanban("list_boards"), json!({})).await;
    let ids: Vec<&str> = listed["boards"]
        .as_array()
        .expect("boards")
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["org", "state", "web", "acme/api/api", "acme/api/lsp"],
        "{listed}"
    );
    assert_eq!(listed["invalid"][0]["id"], "bad", "{listed}");

    let board_until = async |id: &str, done: &dyn Fn(&Value) -> bool| {
        forge
            .until(
                &format!("board {id}"),
                async || forge.ok(&kanban("board"), json!({"id": id})).await,
                done,
            )
            .await
    };
    let org = board_until("org", &|view| card_count(view) == 3).await;
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
    assert_eq!(
        card_count(&board_until("web", &|view| card_count(view) == 1).await),
        1
    );
    let repo = board_until("acme/api/api", &|view| card_count(view) == 2).await;
    assert_eq!(card_count(&repo), 2);
    let project = board_until("acme/api/lsp", &|view| card_count(view) == 1).await;
    assert_eq!(columns(&project)[0].1, vec!["acme/api#1".to_owned()]);
    assert_eq!(project["board"]["name"], "LSP");

    // A repository's new contribution reconfigures the org's Kanban.
    forge
        .push_config(
            "acme/api",
            Some(&api_config(&issues_version, "Language server")),
        )
        .await;
    board_until("acme/api/lsp", &|view| {
        view["board"]["name"] == "Language server"
    })
    .await;

    // Moving a card updates the issue in its own repository.
    forge
        .ok(
            &kanban("move"),
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "done"}),
        )
        .await;
    let issue = forge
        .ok(&issues_tool("web", "get"), json!({"number": 1}))
        .await;
    assert_eq!(issue["issue"]["fields"]["status"], "done", "{issue}");
    let org = board_until("org", &|view| columns(view)[2].1.len() == 1).await;
    assert_eq!(columns(&org)[2].1, vec!["acme/web#1".to_owned()]);

    // The board's UI action does the same, and a state board closes an issue.
    forge
        .ok(
            "/-/api/orgs/acme/extensions/rawkode.kanban/actions/move",
            json!({"board": "acme/api/api", "repo_id": api_id, "number": 2, "to": "doing"}),
        )
        .await;
    forge
        .ok(
            &kanban("move"),
            json!({"board": "state", "repo_id": api_id, "number": 1, "to": "closed"}),
        )
        .await;
    let closed = forge
        .ok(&issues_tool("api", "get"), json!({"number": 1}))
        .await;
    assert_eq!(closed["issue"]["state"], "closed");
    let page = forge
        .ok(
            "/-/api/orgs/acme/extensions/rawkode.kanban/render/board",
            json!({"board": "org"}),
        )
        .await;
    assert!(page.to_string().contains("Web login"), "{page}");

    // Moves outside a board's columns or scope are refused.
    let (status, _) = forge
        .post(
            &kanban("move"),
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "nope"}),
        )
        .await;
    assert_eq!(status, 400);
    let (status, _) = forge
        .post(
            &kanban("move"),
            json!({"board": "acme/api/lsp", "repo_id": web_id, "number": 1, "to": "todo"}),
        )
        .await;
    assert_eq!(status, 404);

    // The same component without permissions.calls may not touch issues.
    let locked_tool = "/-/api/orgs/acme/extensions/rawkode.kanban-locked/tools";
    forge
        .until(
            "the locked board's cards",
            async || {
                forge
                    .post(&format!("{locked_tool}/board"), json!({"id": "org"}))
                    .await
                    .1
            },
            |view| card_count(view) == 3,
        )
        .await;
    let (status, refused) = forge
        .post(
            &format!("{locked_tool}/move"),
            json!({"board": "org", "repo_id": web_id, "number": 1, "to": "todo"}),
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    let unchanged = forge
        .ok(&issues_tool("web", "get"), json!({"number": 1}))
        .await;
    assert_eq!(unchanged["issue"]["fields"]["status"], "done");

    forge.stop().await;
}
