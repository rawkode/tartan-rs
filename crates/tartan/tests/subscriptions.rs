//! An installed extension receives its repository's events through `on-event`, and the events
//! of the other extensions installed on that repository, but nothing from other repositories;
//! what it emits reaches the forge's subscribers. A package's `requires` must be installed by
//! the same configuration. Installations come from pushed `tartan.cue`, and the packages from
//! modules of the fixture, synced into the forge.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_FIXTURE_PACKAGE` (the module directory of
//! `extensions/fixture`), `TARTAN_MODULE_SOURCE` and `CUE_BIN` (see `scripts/e2e.sh`).

mod common;

use std::time::Duration;

use common::{Forge, package, ref_event};
use serde_json::{Value, json};

const FIXTURE: &str = "tartan.fixture";
const LISTENER: &str = "tartan.listener";

fn install(ids: &[&str]) -> String {
    let entries: String = ids
        .iter()
        .map(|id| format!("extensions: \"{id}\": version: \"0.1.0\"\n"))
        .collect();
    format!("package tartan\n{entries}")
}

/// The `seen` tool of `extension` on `acme/<repo>`.
fn seen_tool(repo: &str, extension: &str) -> String {
    format!("/-/api/orgs/acme/repos/{repo}/extensions/{extension}/tools/seen")
}

/// What `seen` returned: its rows.
fn rows(value: &Value) -> Vec<Value> {
    value["seen"].as_array().cloned().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs TARTAN_FIXTURE_PACKAGE, the module source and a Rivet engine; run scripts/e2e.sh"]
async fn an_extension_receives_its_scope_and_emits_its_own() {
    let fixture = package("TARTAN_FIXTURE_PACKAGE");
    let forge = Forge::start("subscriptions-test").await;

    let (status, _) = forge
        .put("/-/api/subscribers/emitted", json!({"pattern": "x.>"}))
        .await;
    assert_eq!(status, 204);
    let (status, _) = forge
        .put("/-/api/subscribers/config", json!({"pattern": "config.*"}))
        .await;
    assert_eq!(status, 204);
    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    for repo in ["api", "other"] {
        forge
            .ok("/-/api/orgs/acme/repos", json!({"slug": repo}))
            .await;
    }
    // The listener is the fixture under another id: it subscribes to what the fixture emits and
    // requires the fixture beside it.
    let mut listener = fixture.manifest.raw.clone();
    listener["id"] = json!(LISTENER);
    listener["subscribe"] = json!([{"event": "x.tartan.fixture.seen"}]);
    listener["requires"] = json!([FIXTURE]);
    forge.modules.publish(&fixture, &listener, None).await;
    let report = forge.publish(&fixture, &fixture.manifest.raw).await;
    let mut registered = report.registered.clone();
    registered.sort();
    assert_eq!(
        registered,
        vec!["tartan.fixture@0.1.0", "tartan.listener@0.1.0"],
        "{report:?}"
    );

    // The configuration push is itself a ref update the fixture then receives.
    forge
        .push_config("acme/api", Some(&install(&[FIXTURE])))
        .await;
    forge
        .until_installed("acme/api", &["tartan.fixture@0.1.0"])
        .await;

    let zero = "0".repeat(40);
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let batch = json!([
        ref_event(
            "acme/api",
            forge.next_seq(),
            "refs/heads/feature",
            &zero,
            &a
        ),
        ref_event("acme/api", forge.next_seq(), "refs/heads/fail", &zero, &a),
        ref_event("acme/api", forge.next_seq(), "refs/heads/feature", &a, &b),
    ]);
    assert_eq!(forge.hook(&batch).await, 204);
    assert_eq!(
        forge.hook(&batch).await,
        204,
        "a repeated delivery is accepted"
    );
    // Another repository's events never reach the repository's installation.
    let elsewhere = json!([ref_event(
        "acme/other",
        forge.next_seq(),
        "refs/heads/x",
        &zero,
        &a
    )]);
    assert_eq!(forge.hook(&elsewhere).await, 204);

    // Three ref updates it could handle, once each; the failing one became a dead letter
    // without blocking the update after it.
    let seen_tool = seen_tool("api", FIXTURE);
    let seen_tool = seen_tool.as_str();
    let seen = forge
        .until(
            "the fixture's seen tool",
            async || forge.post(seen_tool, json!({})).await.1,
            |value| value["seen"].as_array().is_some_and(|seen| seen.len() >= 3),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        forge.post(seen_tool, json!({})).await.1,
        seen,
        "nothing more arrived"
    );
    let seen = seen["seen"].as_array().expect("a list");
    assert_eq!(seen.len(), 3, "{seen:#?}");
    let repo_source = seen[0]["source"].as_str().expect("a source").to_owned();
    assert!(repo_source.starts_with("repo:"), "{repo_source}");
    assert!(
        seen.iter()
            .all(|s| s["type"] == "ref.updated" && s["source"] == repo_source.as_str())
    );

    // What it emitted reached the forge's subscriber.
    let emitted = forge
        .until(
            "the emitted events",
            async || Value::from(forge.received("emitted").await),
            |value| value.as_array().is_some_and(|events| events.len() >= 3),
        )
        .await;
    let emitted = emitted.as_array().expect("a list");
    assert_eq!(emitted.len(), 3, "{emitted:#?}");
    for event in emitted {
        assert_eq!(event["type"], "x.tartan.fixture.seen");
        assert_eq!(event["data"]["data"]["type"], "ref.updated");
    }

    another_extension_on_the_repository_receives_its_events(&forge).await;

    forge.stop().await;
}

async fn another_extension_on_the_repository_receives_its_events(forge: &Forge) {
    // The fixture on another repository emits events of its own.
    forge
        .push_config("acme/other", Some(&install(&[FIXTURE])))
        .await;
    forge
        .until_installed("acme/other", &["tartan.fixture@0.1.0"])
        .await;
    let other_seen = seen_tool("other", FIXTURE);
    forge
        .until(
            "the other repository's fixture",
            async || forge.post(&other_seen, json!({})).await.1,
            |value| !rows(value).is_empty(),
        )
        .await;

    // Without the fixture it requires, the listener is refused and nothing changes. The
    // refusal is looked up by its problem: actors an earlier test process left on the same
    // engine can be woken by this one and report to its subscribers too.
    forge
        .push_config("acme/api", Some(&install(&[LISTENER])))
        .await;
    forge
        .until(
            "the refused configuration",
            async || Value::from(forge.received("config").await),
            |events| {
                events.as_array().is_some_and(|e| {
                    e.iter().any(|e| {
                        e["type"] == "config.refused"
                            && e["data"]["problems"]
                                .to_string()
                                .contains("tartan.listener requires tartan.fixture")
                    })
                })
            },
        )
        .await;
    assert_eq!(
        forge.installed("acme/api").await,
        vec!["tartan.fixture@0.1.0"]
    );

    // Installed beside the fixture, the listener receives everything the fixture on its own
    // repository emitted, from before it was installed too, and nothing from the other one.
    forge
        .push_config("acme/api", Some(&install(&[FIXTURE, LISTENER])))
        .await;
    forge
        .until_installed(
            "acme/api",
            &["tartan.fixture@0.1.0", "tartan.listener@0.1.0"],
        )
        .await;
    let (_, installations) = forge.get("/-/api/orgs/acme/repos/api/extensions").await;
    let fixture_source = installations
        .as_array()
        .and_then(|list| list.iter().find(|i| i["extension_id"] == FIXTURE))
        .and_then(|i| i["installation_id"].as_str())
        .map(|id| format!("extension:{id}"))
        .expect("the fixture's installation");
    let fixture_seen = seen_tool("api", FIXTURE);
    let listener_seen = seen_tool("api", LISTENER);
    let heard = forge
        .until(
            "the listener hears every event the fixture emitted",
            async || {
                let emitted = rows(&forge.post(&fixture_seen, json!({})).await.1).len();
                let heard = forge.post(&listener_seen, json!({})).await.1;
                json!({ "emitted": emitted, "heard": heard })
            },
            |value| {
                let heard = rows(&value["heard"]).len();
                heard > 0 && Some(heard as u64) == value["emitted"].as_u64()
            },
        )
        .await;
    let heard = rows(&heard["heard"]);
    assert!(
        heard
            .iter()
            .all(|row| row["type"] == "x.tartan.fixture.seen"
                && row["source"] == fixture_source.as_str()),
        "{heard:#?}"
    );
}
