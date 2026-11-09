//! Kanban boards over a repository's issues, configured in its committed CUE: the
//! repository's module depends on `tartan.rawkode.academy/types`, Issues' module and Kanban's module, which
//! the forge serves from its own registry, and its `tartan.cue` installs both extensions through
//! their `#Extension`s, with boards whose columns, lanes and groups are Issues' properties. Cards
//! follow Issues' events; moving a card updates the issue through Kanban's permitted call.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_EXTENSION_MODULES`, `TARTAN_MODULE_SOURCE`,
//! `CUE_REGISTRY` and `CUE_BIN` (see `scripts/e2e.sh`).

mod common;

use std::time::Duration;

use common::{Forge, repository_module};
use serde_json::{Value, json};

/// Issues' entry: the repository's typed properties.
const ISSUES: &str = r#"
extensions: "issues.tartan.rawkode.academy": issues.#Extension & {settings: properties: {
	status: types.#Select & {
		options: ["todo", "doing", "done"]
		labels: {todo: "To do", doing: "In progress", done: "Done"}
		default: "todo"
	}
	priority: types.#Select & {options: ["p1", "p2", "p3"], labels: p1: "Urgent"}
	labels:   types.#MultiSelect & {options: ["bug", "docs"]}
	owner:    types.#Person
	estimate: types.#Number & {min: 0}
}}
"#;

/// Kanban's entry: a board with lanes by priority and groups by project, a project's board
/// with some status columns, lanes by a multi-select and groups by a person, and a state board.
const KANBAN: &str = r#"
let props = extensions."issues.tartan.rawkode.academy".settings.properties

extensions: "kanban.tartan.rawkode.academy": kanban.#Extension & {
	#issues: extensions."issues.tartan.rawkode.academy"
	settings: boards: {
		triage: {name: "Triage", columns: props.status, lanes: props.priority, group: "project"}
		lsp: {
			name:    "Language server"
			project: "crates/lsp"
			columns: {property: props.status, values: ["todo", "done"]}
			lanes:   props.labels
			group:   props.owner
		}
		open: {columns: "state", lanes: "project"}
	}
}
"#;

/// The repository's configuration: its module, and `tartan.cue` with `entries`.
fn config(entries: &str) -> Vec<(String, String)> {
    vec![
        ("cue.mod/module.cue".to_owned(), repository_module()),
        (
            "tartan.cue".to_owned(),
            format!(
                "package tartan\n\nimport (\n\t\"tartan.rawkode.academy/types\"\n\t\"tartan.rawkode.academy/issues\"\n\t\"tartan.rawkode.academy/kanban\"\n)\n\n// Keeps every import used whichever entries are written.\n_uses: [types.#Property, issues.#Extension, kanban.#Extension]\n{entries}"
            ),
        ),
    ]
}

async fn push(forge: &Forge, repo: &str, files: &[(String, String)]) {
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    forge.push_files(repo, &files).await;
}

/// Waits for a `config.refused` event whose problems mention `problem`. Refusals are matched
/// by their problem, not counted: actors an earlier test process left on the same engine can
/// be woken by this one and report to its subscribers too.
async fn refusal(forge: &Forge, problem: &str) {
    forge
        .until(
            &format!("a refused configuration: {problem}"),
            async || Value::from(forge.received("config").await),
            |events| {
                events.as_array().is_some_and(|e| {
                    e.iter().any(|e| {
                        e["type"] == "config.refused"
                            && e["data"]["problems"].to_string().contains(problem)
                    })
                })
            },
        )
        .await;
}

/// A board view as `lane / column / group: #numbers`, one entry per non-empty group, with `-`
/// for a null value.
fn layout(view: &Value) -> Vec<String> {
    let text = |value: &Value| value.as_str().unwrap_or("-").to_owned();
    let mut out = Vec::new();
    for lane in view["lanes"].as_array().into_iter().flatten() {
        for cell in lane["cells"].as_array().into_iter().flatten() {
            for group in cell["groups"].as_array().into_iter().flatten() {
                let cards: Vec<String> = group["cards"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|card| format!("#{}", card["number"]))
                    .collect();
                if !cards.is_empty() {
                    out.push(format!(
                        "{} / {} / {}: {}",
                        text(&lane["value"]),
                        text(&cell["column"]),
                        text(&group["value"]),
                        cards.join(" ")
                    ));
                }
            }
        }
    }
    out
}

fn card_count(view: &Value) -> usize {
    view["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|lane| lane["cells"].as_array().into_iter().flatten())
        .flat_map(|cell| cell["groups"].as_array().into_iter().flatten())
        .map(|group| group["cards"].as_array().map_or(0, Vec::len))
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the first-party modules, the module registries and a Rivet engine; run scripts/e2e.sh"]
async fn boards_from_a_repository_configuration_importing_the_modules() {
    let forge = Forge::start("kanban-test").await;
    let (status, _) = forge
        .put("/-/api/subscribers/config", json!({"pattern": "config.*"}))
        .await;
    assert_eq!(status, 204);
    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    for repo in ["api", "web"] {
        forge
            .ok("/-/api/orgs/acme/repos", json!({"slug": repo}))
            .await;
    }

    let report = forge.sync_extension_modules().await;
    let mut registered = report.registered.clone();
    registered.sort();
    assert_eq!(
        registered,
        vec![
            "issues.tartan.rawkode.academy@0.7.0",
            "kanban.tartan.rawkode.academy@0.4.0"
        ],
        "{report:?}"
    );

    // Kanban without the Issues it requires is refused, and installs nothing.
    push(
        &forge,
        "acme/web",
        &config(
            "extensions: \"kanban.tartan.rawkode.academy\": kanban.#Extension & {settings: boards: open: columns: \"state\"}\n",
        ),
    )
    .await;
    refusal(
        &forge,
        "kanban.tartan.rawkode.academy requires issues.tartan.rawkode.academy",
    )
    .await;
    assert!(forge.installed("acme/web").await.is_empty());

    // Issues alone first: an issue filed before Kanban is installed still reaches its boards.
    push(&forge, "acme/api", &config(ISSUES)).await;
    forge
        .until_installed("acme/api", &["issues.tartan.rawkode.academy@0.7.0"])
        .await;
    let issues_tool = |tool: &str| {
        format!("/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/{tool}")
    };
    let created = forge
        .ok(
            &issues_tool("create"),
            json!({
                "title": "LSP crash",
                "project": "crates/lsp",
                "properties": {"priority": "p1", "labels": ["bug"], "owner": forge.identity.owner}
            }),
        )
        .await;
    assert_eq!(
        created["issue"]["properties"]["status"], "todo",
        "{created}"
    );
    // The owner's token is an agent acting for her DID (spec: extension-runtime).
    let author = &created["issue"]["author"];
    assert_eq!(author["kind"], "agent", "{created}");
    assert!(
        author["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("token:")),
        "{created}"
    );
    assert_eq!(
        author["onBehalfOf"],
        json!(forge.identity.owner),
        "{created}"
    );

    push(&forge, "acme/api", &config(&format!("{ISSUES}{KANBAN}"))).await;
    forge
        .until_installed(
            "acme/api",
            &[
                "issues.tartan.rawkode.academy@0.7.0",
                "kanban.tartan.rawkode.academy@0.4.0",
            ],
        )
        .await;
    for (title, properties) in [
        (
            "API docs",
            json!({"status": "doing", "priority": "p2", "labels": ["docs", "bug"]}),
        ),
        ("Release notes", json!({"status": "done", "estimate": 3})),
    ] {
        forge
            .ok(
                &issues_tool("create"),
                json!({"title": title, "properties": properties}),
            )
            .await;
    }

    let kanban = |tool: &str| {
        format!("/-/api/orgs/acme/repos/api/extensions/kanban.tartan.rawkode.academy/tools/{tool}")
    };
    let listed = forge.ok(&kanban("list_boards"), json!({})).await;
    assert_eq!(
        listed,
        json!({
            "boards": [
                {"id": "lsp", "name": "Language server", "project": "crates/lsp"},
                {"id": "open", "name": "open", "project": null},
                {"id": "triage", "name": "Triage", "project": null},
            ],
            "invalid": [],
        })
    );

    let board_until = async |id: &str, done: &dyn Fn(&Value) -> bool| {
        forge
            .until(
                &format!("board {id}"),
                async || forge.ok(&kanban("board"), json!({"board": id})).await,
                done,
            )
            .await
    };

    // Lanes by priority, every option and then "None"; columns titled by the labels; groups
    // by project.
    let triage = board_until("triage", &|view| card_count(view) == 3).await;
    assert_eq!(
        triage["board"]["columns"],
        json!([
            {"value": "todo", "title": "To do"},
            {"value": "doing", "title": "In progress"},
            {"value": "done", "title": "Done"},
        ])
    );
    assert_eq!(triage["board"]["lanes"], json!({"by": "priority"}));
    assert_eq!(triage["board"]["group"], json!({"by": "project"}));
    let lanes: Vec<(Value, Value)> = triage["lanes"]
        .as_array()
        .expect("lanes")
        .iter()
        .map(|lane| (lane["value"].clone(), lane["title"].clone()))
        .collect();
    assert_eq!(
        lanes,
        vec![
            (json!("p1"), json!("Urgent")),
            (json!("p2"), json!("p2")),
            (json!("p3"), json!("p3")),
            (Value::Null, json!("None")),
        ]
    );
    assert_eq!(
        layout(&triage),
        vec![
            "p1 / todo / crates/lsp: #1",
            "p2 / doing / -: #2",
            "- / done / -: #3",
        ]
    );

    // A project's board: only its issues, some columns, lanes by a multi-select, groups by a
    // person.
    let lsp = board_until("lsp", &|view| card_count(view) == 1).await;
    assert_eq!(
        lsp["board"]["columns"],
        json!([{"value": "todo", "title": "To do"}, {"value": "done", "title": "Done"}])
    );
    // A person group is keyed by the DID and names it, so the web UI labels it by handle.
    let alice = &forge.identity.owner;
    assert_eq!(layout(&lsp), vec![format!("bug / todo / {alice}: #1")]);
    assert_eq!(
        lsp["lanes"][0]["cells"][0]["groups"][0]["person"],
        json!(alice),
        "{lsp}"
    );

    // A state board with lanes by project.
    let open = board_until("open", &|view| card_count(view) == 3).await;
    assert_eq!(
        layout(&open),
        vec!["crates/lsp / open / -: #1", "- / open / -: #2 #3"]
    );

    // Moving a card sets the column's property through Issues' update.
    let moved = forge
        .ok(
            &kanban("move"),
            json!({"board": "triage", "number": 1, "column": "done"}),
        )
        .await;
    assert_eq!(moved["card"]["properties"]["status"], "done", "{moved}");
    let issue = forge.ok(&issues_tool("get"), json!({"number": 1})).await;
    assert_eq!(issue["issue"]["properties"]["status"], "done", "{issue}");
    assert_eq!(issue["issue"]["version"], 2, "{issue}");
    let triage = board_until("triage", &|view| {
        layout(view).contains(&"p1 / done / crates/lsp: #1".to_owned())
    })
    .await;
    assert_eq!(card_count(&triage), 3);

    // On a state board, moving a card closes the issue.
    forge
        .ok(
            &kanban("move"),
            json!({"board": "open", "number": 3, "column": "closed"}),
        )
        .await;
    let closed = forge.ok(&issues_tool("get"), json!({"number": 3})).await;
    assert_eq!(closed["issue"]["state"], "closed", "{closed}");

    // Moves to no column of the board, or of an issue outside the board's project, are refused.
    let (status, body) = forge
        .post(
            &kanban("move"),
            json!({"board": "triage", "number": 2, "column": "blocked"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = forge
        .post(
            &kanban("move"),
            json!({"board": "lsp", "number": 2, "column": "done"}),
        )
        .await;
    assert_eq!(status, 404, "{body}");
    let untouched = forge.ok(&issues_tool("get"), json!({"number": 2})).await;
    assert_eq!(untouched["issue"]["properties"]["status"], "doing");

    // A board whose columns are not one of Issues' select properties fails evaluation, so the
    // configuration is refused and the boards stay as they were.
    let lookalike = KANBAN.replace(
        "columns: props.status, lanes",
        "columns: types.#Select & {options: [\"todo\", \"review\", \"done\"]}, lanes",
    );
    assert_ne!(lookalike, KANBAN);
    push(&forge, "acme/api", &config(&format!("{ISSUES}{lookalike}"))).await;
    refusal(&forge, "boards.triage.columns").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let listed_again = forge.ok(&kanban("list_boards"), json!({})).await;
    assert_eq!(listed_again, listed);

    // What `cue export` refuses as incomplete is refused too: a board without columns, and a
    // select property without options.
    let no_columns = KANBAN.replace(
        "open: {columns: \"state\", lanes: \"project\"}",
        "open: {columns: \"state\", lanes: \"project\"}\n\t\tbare: name: \"No columns\"",
    );
    assert_ne!(no_columns, KANBAN);
    push(
        &forge,
        "acme/api",
        &config(&format!("{ISSUES}{no_columns}")),
    )
    .await;
    refusal(&forge, "boards.bare.columns is not concrete").await;
    let no_options = ISSUES.replace(
        "estimate: types.#Number & {min: 0}",
        "estimate: types.#Number & {min: 0}\n\tblank: types.#Select",
    );
    assert_ne!(no_options, ISSUES);
    push(
        &forge,
        "acme/api",
        &config(&format!("{no_options}{KANBAN}")),
    )
    .await;
    refusal(&forge, "properties.blank.options").await;

    // CUE cannot tell that #issues is the repository's Issues entry, so Kanban checks each board
    // against the properties the installed Issues lists: a board over a property Issues does not
    // have is invalid, and the other boards still work.
    let unbound = r#"
extensions: "kanban.tartan.rawkode.academy": kanban.#Extension & {
	#issues: issues.#Extension & {settings: properties: phase: types.#Select & {options: ["a", "b"]}}
	settings: boards: {
		phases: {columns: #issues.settings.properties.phase}
		open: {columns: "state", lanes: "project"}
	}
}
"#;
    push(&forge, "acme/api", &config(&format!("{ISSUES}{unbound}"))).await;
    let boards = forge
        .until(
            "the board over another Issues entry is invalid",
            async || forge.ok(&kanban("list_boards"), json!({})).await,
            |listed| listed["invalid"].as_array().is_some_and(|i| !i.is_empty()),
        )
        .await;
    assert_eq!(
        boards,
        json!({
            "boards": [{"id": "open", "name": "open", "project": null}],
            "invalid": [{
                "id": "phases",
                "problem": "columns: Issues on this repository has no property phase",
            }],
        })
    );
    let (status, body) = forge
        .post(
            &kanban("move"),
            json!({"board": "phases", "number": 1, "column": "a"}),
        )
        .await;
    assert_eq!(status, 400, "{body}");

    // Signed in, she is herself.
    let session = common::sign_in(&forge.base, "alice.test", "/", None)
        .await
        .session();
    let by_person: serde_json::Value = common::browser()
        .post(format!(
            "{}{}",
            forge.base,
            "/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/create"
        ))
        .header("cookie", format!("tartan_session={session}"))
        .header("origin", &forge.base)
        .json(&json!({"title": "Filed in the browser", "properties": {"status": "done"}}))
        .send()
        .await
        .expect("answers")
        .json()
        .await
        .expect("json");
    assert_eq!(
        by_person["issue"]["author"],
        json!({"kind": "user", "id": forge.identity.owner}),
        "{by_person}"
    );

    // A reader (readers: "signed_in") calls the tools the manifests declare `access: "read"`,
    // read-only, including Kanban's board, which calls Issues; tools that write are refused
    // with the read-only explanation.
    let open = forge
        .gateway(tartan_config::deployment::Readers::SignedIn, Vec::new())
        .await;
    let bob = common::sign_in(&open, "bob.test", "/", None)
        .await
        .session();
    let as_bob = async |path: String, body: Value| {
        let response = common::browser()
            .post(format!("{open}{path}"))
            .header("cookie", format!("tartan_session={bob}"))
            .header("origin", &open)
            .json(&body)
            .send()
            .await
            .expect("answers");
        let status = response.status().as_u16();
        (status, response.json::<Value>().await.unwrap_or_default())
    };
    let (status, listed) = as_bob(issues_tool("list"), json!({})).await;
    assert_eq!(status, 200, "{listed}");
    let (status, board) = as_bob(kanban("board"), json!({"board": "open"})).await;
    assert_eq!(status, 200, "{board}");
    for (path, body) in [
        (issues_tool("create"), json!({"title": "By a reader"})),
        (
            kanban("move"),
            json!({"board": "open", "number": 1, "column": "done"}),
        ),
    ] {
        let (status, refusal) = as_bob(path, body).await;
        assert_eq!(status, 403, "{refusal}");
        assert!(refusal.to_string().contains("read-only"), "{refusal}");
    }
    forge.stop().await;
}
