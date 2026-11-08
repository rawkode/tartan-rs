//! Extensions installed and configured by the `tartan.cue` pushed to a repository, then used
//! over HTTP, with the real Rivet engine.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH` and `TARTAN_ISSUES_PACKAGE` (see `scripts/e2e.sh`).

mod common;

use std::time::Duration;

use base64::Engine;
use common::{Forge, package};
use serde_json::{Value, json};

fn issues_config(version: &str, fields: &str) -> String {
    format!(
        r#"package tartan

extensions: "rawkode.issues": {{
	version: "{version}"
	settings: fields: {{{fields}}}
}}
"#
    )
}

const STATUS: &str = r#"status: {type: "select", options: ["todo", "done"], default: "todo"}"#;

async fn wait_for_count(forge: &Forge, kind: &str, count: usize) {
    forge
        .until(
            &format!("{count} {kind}"),
            async || Value::from(forge.received_types("audit").await),
            |types| {
                types
                    .as_array()
                    .is_some_and(|t| t.iter().filter(|t| *t == kind).count() >= count)
            },
        )
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs TARTAN_ISSUES_PACKAGE and a Rivet engine; run scripts/e2e.sh"]
async fn extensions_come_only_from_pushed_configuration() {
    let issues = package("TARTAN_ISSUES_PACKAGE");
    let version = issues.manifest.version.clone();
    let installed = [format!("rawkode.issues@{version}")];
    let installed: Vec<&str> = installed.iter().map(String::as_str).collect();
    let forge = Forge::start("extensions-test").await;

    let (status, _) = forge
        .put("/-/api/subscribers/audit", json!({"pattern": ">"}))
        .await;
    assert_eq!(status, 204);
    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    for repo in ["api", "other", "tartan"] {
        forge
            .ok("/-/api/orgs/acme/repos", json!({"slug": repo}))
            .await;
    }

    // Publishing: once, again (the same package), then refusing other contents.
    let published = forge.publish(&issues, &issues.manifest.raw).await;
    assert_eq!(published["id"], "rawkode.issues");
    forge.publish(&issues, &issues.manifest.raw).await;
    let mut changed = issues.clone();
    changed.migrations[0].1.push_str("\n-- changed\n");
    let upload = json!({
        "manifest": changed.manifest.raw,
        "component": base64::engine::general_purpose::STANDARD.encode(&changed.component),
        "migrations": changed.migrations.iter()
            .map(|(path, sql)| json!({"path": path, "sql": sql}))
            .collect::<Vec<_>>(),
    });
    let (status, _) = forge.post("/-/api/packages", upload).await;
    assert_eq!(status, 409);

    // There is no install route.
    let (status, _) = forge
        .post(
            "/-/api/orgs/acme/repos/api/extensions",
            json!({"package": "rawkode.issues", "version": version}),
        )
        .await;
    assert_eq!(status, 405);

    // A push to main installs Issues with its configured fields.
    forge
        .push_config("acme/api", Some(&issues_config(&version, STATUS)))
        .await;
    forge.until_installed("acme/api", &installed).await;
    let tools = "/-/api/orgs/acme/repos/api/extensions/rawkode.issues/tools";
    let created = forge
        .ok(
            &format!("{tools}/create"),
            json!({"title": "Over HTTP", "body": "Hello"}),
        )
        .await;
    assert_eq!(created["issue"]["number"], 1);
    forge
        .ok(
            &format!("{tools}/create"),
            json!({"title": "Second", "fields": {"status": "done"}}),
        )
        .await;
    let listed = forge.ok(&format!("{tools}/list"), json!({})).await;
    assert_eq!(listed["open"], 2);
    let (status, refused) = forge.post(&format!("{tools}/create"), json!({})).await;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["kind"], "invalid");
    let (status, _) = forge.post(&format!("{tools}/nope"), json!({})).await;
    assert_eq!(status, 404);
    let page = forge
        .ok(
            "/-/api/orgs/acme/repos/api/extensions/rawkode.issues/render/issues",
            json!({}),
        )
        .await;
    assert!(page.to_string().contains("Over HTTP"), "{page}");
    let (status, _) = forge
        .post(
            "/-/api/orgs/acme/repos/other/extensions/rawkode.issues/tools/list",
            json!({}),
        )
        .await;
    assert_eq!(status, 404, "the other repository has no Issues");

    // A push to another branch applies nothing.
    forge
        .push_branch("acme/api", "refs/heads/topic", "package tartan\n")
        .await;

    // Refused configurations change nothing: an unpublished version and invalid CUE.
    forge
        .push_config("acme/api", Some(&issues_config("9.9.9", STATUS)))
        .await;
    wait_for_count(&forge, "config.refused", 1).await;
    forge
        .push_config("acme/api", Some("package tartan\nthis is not cue\n"))
        .await;
    wait_for_count(&forge, "config.refused", 2).await;
    assert_eq!(forge.installed("acme/api").await, installed);

    // A settings change reconfigures the installation and keeps its data. The configuration is
    // the root package tartan: here split across files, importing a package of the repository's
    // module, next to a cuenv file in another package.
    forge
        .push_files(
            "acme/api",
            &[
                (
                    "cue.mod/module.cue",
                    "module: \"example.com/acme-api\"\nlanguage: version: \"v0.14.1\"\n",
                ),
                (
                    "issues.cue",
                    &format!(
                        "package tartan\n\nimport defs \"example.com/acme-api/config/fields\"\n\nextensions: \"rawkode.issues\": {{version: \"{version}\", settings: fields: defs.all}}\n"
                    ),
                ),
                (
                    "config/fields/fields.cue",
                    &format!("package fields\n\nall: {{\n\t{STATUS}\n\towner: type: \"text\"\n}}\n"),
                ),
                ("env.cue", "package cuenv\n\nenv: FOO: \"bar\"\n"),
            ],
        )
        .await;
    wait_for_count(&forge, "extension.configured", 1).await;
    let fields = forge.ok(&format!("{tools}/fields"), json!({})).await;
    assert_eq!(
        fields["fields"].as_array().map(Vec::len),
        Some(2),
        "{fields}"
    );

    // Removing it uninstalls it; adding it back restores it with its issues.
    forge
        .push_config("acme/api", Some("package tartan\n"))
        .await;
    forge.until_installed("acme/api", &[]).await;
    let (status, _) = forge.post(&format!("{tools}/list"), json!({})).await;
    assert_eq!(status, 404);
    forge
        .push_config("acme/api", Some(&issues_config(&version, STATUS)))
        .await;
    forge.until_installed("acme/api", &installed).await;
    let listed = forge
        .ok(&format!("{tools}/list"), json!({"state": "all"}))
        .await;
    assert_eq!(
        listed["issues"].as_array().map(Vec::len),
        Some(2),
        "{listed}"
    );

    // The org's tartan repository installs only versioned, org-scoped extensions.
    forge
        .push_config(
            "acme/tartan",
            Some("package tartan\nextensions: \"rawkode.kanban\": settings: {}\n"),
        )
        .await;
    wait_for_count(&forge, "config.refused", 3).await;
    forge
        .push_config("acme/tartan", Some(&issues_config(&version, STATUS)))
        .await;
    wait_for_count(&forge, "config.refused", 4).await;
    assert_eq!(forge.installed("acme").await, Vec::<String>::new());

    let types = forge.received_types("audit").await;
    let count = |kind: &str| types.iter().filter(|t| *t == kind).count();
    assert_eq!(count("extension.installed"), 2, "{types:?}");
    assert_eq!(count("extension.uninstalled"), 1, "{types:?}");
    assert_eq!(count("extension.configured"), 1, "{types:?}");
    assert_eq!(count("package.published"), 1, "{types:?}");
    assert_eq!(count("x.rawkode.issues.issue.created"), 2, "{types:?}");

    forge.stop().await;
}
