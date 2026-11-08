//! Kanban boards over a repository's issues, configured in its committed CUE: the
//! repository's module depends on `tartan.dev/types`, Issues' module and Kanban's module, which
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
extensions: "rawkode.issues": issues.#Extension & {settings: properties: {
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
let props = extensions."rawkode.issues".settings.properties

extensions: "rawkode.kanban": kanban.#Extension & {
	#issues: extensions."rawkode.issues"
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
                "package tartan\n\nimport (\n\t\"tartan.dev/types\"\n\t\"github.com/rawkode/tartan-ext/issues\"\n\t\"github.com/rawkode/tartan-ext/kanban\"\n)\n\n// Keeps every import used whichever entries are written.\n_uses: [types.#Property, issues.#Extension, kanban.#Extension]\n{entries}"
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
        vec!["rawkode.issues@0.6.0", "rawkode.kanban@0.3.0"],
        "{report:?}"
    );

    // Kanban without the Issues it requires is refused, and installs nothing.
    push(
        &forge,
        "acme/web",
        &config(
            "extensions: \"rawkode.kanban\": kanban.#Extension & {settings: boards: open: columns: \"state\"}\n",
        ),
    )
    .await;
    refusal(&forge, "rawkode.kanban requires rawkode.issues").await;
    assert!(forge.installed("acme/web").await.is_empty());

    // Issues alone first: an issue filed before Kanban is installed still reaches its boards.
    push(&forge, "acme/api", &config(ISSUES)).await;
    forge
        .until_installed("acme/api", &["rawkode.issues@0.6.0"])
        .await;
    let issues_tool =
        |tool: &str| format!("/-/api/orgs/acme/repos/api/extensions/rawkode.issues/tools/{tool}");
    let created = forge
        .ok(
            &issues_tool("create"),
            json!({
                "title": "LSP crash",
                "project": "crates/lsp",
                "properties": {"priority": "p1", "labels": ["bug"], "owner": "alice"}
            }),
        )
        .await;
    assert_eq!(
        created["issue"]["properties"]["status"], "todo",
        "{created}"
    );

    push(&forge, "acme/api", &config(&format!("{ISSUES}{KANBAN}"))).await;
    forge
        .until_installed(
            "acme/api",
            &["rawkode.issues@0.6.0", "rawkode.kanban@0.3.0"],
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

    let kanban =
        |tool: &str| format!("/-/api/orgs/acme/repos/api/extensions/rawkode.kanban/tools/{tool}");
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
    assert_eq!(layout(&lsp), vec!["bug / todo / alice: #1"]);

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

    forge.stop().await;
}
