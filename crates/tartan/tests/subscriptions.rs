//! An installed extension receives its repository's events through `on-event`, and what it
//! emits reaches the forge's subscribers. Installations come from pushed `tartan.cue`, and the
//! package from the fixture's module, synced into the forge.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_FIXTURE_PACKAGE` (the module directory of
//! `extensions/fixture`), `TARTAN_MODULE_SOURCE` and `CUE_BIN` (see `scripts/e2e.sh`).

mod common;

use std::time::Duration;

use common::{Forge, package, ref_event};
use serde_json::{Value, json};

fn install(id: &str) -> String {
    format!("package tartan\nextensions: \"{id}\": version: \"0.1.0\"\n")
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
    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    for repo in ["api", "other"] {
        forge
            .ok("/-/api/orgs/acme/repos", json!({"slug": repo}))
            .await;
    }
    let report = forge.publish(&fixture, &fixture.manifest.raw).await;
    assert_eq!(
        report.registered,
        vec!["tartan.fixture@0.1.0"],
        "{report:?}"
    );

    // The configuration push is itself a ref update the fixture then receives.
    forge
        .push_config("acme/api", Some(&install("tartan.fixture")))
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
    let seen_tool = "/-/api/orgs/acme/repos/api/extensions/tartan.fixture/tools/seen";
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

    forge.stop().await;
}
