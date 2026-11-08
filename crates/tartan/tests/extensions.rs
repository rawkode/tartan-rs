//! Extension packages come from the CUE modules the forge syncs into its registry; extensions
//! are installed and configured only by the configuration pushed to a repository, evaluated
//! natively against that registry, then used over HTTP. With the real Rivet engine.
//!
//! Needs `RIVET_ENGINE_BINARY_PATH`, `TARTAN_FIXTURE_PACKAGE` (the module directory of
//! `extensions/fixture`), `TARTAN_MODULE_SOURCE`, `CUE_REGISTRY` and `CUE_BIN` (see
//! `scripts/e2e.sh`).

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use common::{Forge, module_path, package};
use serde_json::{Value, json};
use tartan_config::deployment::ModuleVersion;

const FIXTURE: &str = "tartan.fixture";

/// A configuration in plain CUE, without a module.
fn install(version: &str, settings: &str) -> String {
    format!(
        "package tartan\n\nextensions: \"{FIXTURE}\": {{\n\tversion: \"{version}\"\n\tsettings: {{{settings}}}\n}}\n"
    )
}

/// A configuration whose module depends on the fixture's module at `module_version` and
/// installs it through its `#Extension`, unified with `entry`.
fn through_module(module_version: &str, entry: &str) -> Vec<(String, String)> {
    let path = module_path(FIXTURE);
    vec![
        (
            "cue.mod/module.cue".to_owned(),
            format!(
                "module: \"example.com/acme-api\"\nlanguage: version: \"v0.14.1\"\ndeps: \"{path}@v0\": v: \"{module_version}\"\n"
            ),
        ),
        (
            "tartan.cue".to_owned(),
            format!(
                "package tartan\n\nimport \"{path}\"\n\nextensions: \"{FIXTURE}\": fixture.#Extension & {entry}\n"
            ),
        ),
        (
            "env.cue".to_owned(),
            "package cuenv\n\nenv: FOO: \"bar\"\n".to_owned(),
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

/// The last `config.refused` event's problems.
async fn last_refusal(forge: &Forge) -> String {
    forge
        .received("audit")
        .await
        .iter()
        .rev()
        .find(|event| event["type"] == "config.refused")
        .map(|event| event["data"]["problems"].to_string())
        .unwrap_or_default()
}

/// Runs the `cue` command in `dir` with this process's environment, whose `CUE_REGISTRY` is the
/// forge under test.
async fn cue(dir: &Path, args: &[&str]) -> (bool, String) {
    let bin = std::env::var("CUE_BIN").unwrap_or_else(|_| "cue".to_owned());
    let output = tokio::process::Command::new(&bin)
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .unwrap_or_else(|error| panic!("run {bin}: {error}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs TARTAN_FIXTURE_PACKAGE, the module registries and a Rivet engine; run scripts/e2e.sh"]
async fn extensions_come_from_modules_and_pushed_configuration() {
    let fixture = package("TARTAN_FIXTURE_PACKAGE");
    let forge = Forge::start("extensions-test").await;
    let (status, _) = forge
        .put("/-/api/subscribers/audit", json!({"pattern": ">"}))
        .await;
    assert_eq!(status, 204);
    forge.ok("/-/api/orgs", json!({"slug": "acme"})).await;
    for repo in ["api", "other"] {
        forge
            .ok("/-/api/orgs/acme/repos", json!({"slug": repo}))
            .await;
    }

    modules_are_synced_and_their_packages_registered(&forge, &fixture).await;
    the_registry_serves_cue_and_refuses_writes(&forge).await;
    configuration_installs_and_configures(&forge).await;

    let types = forge.received_types("audit").await;
    let count = |kind: &str| types.iter().filter(|t| *t == kind).count();
    assert_eq!(count("extension.installed"), 2, "{types:?}");
    assert_eq!(count("extension.upgraded"), 1, "{types:?}");
    assert_eq!(count("extension.uninstalled"), 1, "{types:?}");
    assert_eq!(count("package.published"), 3, "{types:?}");

    forge.stop().await;
}

async fn modules_are_synced_and_their_packages_registered(
    forge: &Forge,
    fixture: &tartan_extensions::ExtensionPackage,
) {
    let module = format!("{}@v0.1.0", module_path(FIXTURE));
    let report = forge.publish(fixture, &fixture.manifest.raw).await;
    assert_eq!(report.copied, vec![module.clone()], "{report:?}");
    assert_eq!(
        report.registered,
        vec!["tartan.fixture@0.1.0"],
        "{report:?}"
    );

    // Syncing again copies nothing: the registry holds the same digest.
    let again = forge.sync().await;
    assert!(again.copied.is_empty(), "{again:?}");
    assert_eq!(again.unchanged, vec![module], "{again:?}");
    assert_eq!(again.registered, vec!["tartan.fixture@0.1.0"]);

    // A second version of the module is a second version of the package.
    let mut second = fixture.manifest.raw.clone();
    second["version"] = json!("0.2.0");
    let report = forge.publish(fixture, &second).await;
    assert!(
        report
            .registered
            .contains(&"tartan.fixture@0.2.0".to_owned()),
        "{report:?}"
    );

    // Packages that fail the checks are not available, and the sync says why.
    let mut org = fixture.manifest.raw.clone();
    org["id"] = json!("tartan.fixture-org");
    org["storage"]["scope"] = json!("org");
    forge.modules.publish(fixture, &org, None).await;
    let mut mislabelled = fixture.manifest.raw.clone();
    mislabelled["id"] = json!("tartan.fixture-mislabelled");
    forge
        .modules
        .publish(fixture, &mislabelled, Some("v0.3.0"))
        .await;
    let mut broken = fixture.clone();
    broken.component = b"not WebAssembly".to_vec();
    let mut broken_manifest = fixture.manifest.raw.clone();
    broken_manifest["id"] = json!("tartan.fixture-broken");
    let report = forge.publish(&broken, &broken_manifest).await;
    let refused: BTreeMap<&str, &str> = report
        .refused
        .iter()
        .map(|p| (p.module.as_str(), p.reason.as_str()))
        .collect();
    assert_eq!(refused.len(), 3, "{report:?}");
    assert!(
        refused["tartan.test/tartan/fixture_org@v0.1.0"].contains("repo"),
        "{refused:?}"
    );
    assert!(
        refused["tartan.test/tartan/fixture_mislabelled@v0.3.0"].contains("version 0.1.0"),
        "{refused:?}"
    );
    assert!(
        refused["tartan.test/tartan/fixture_broken@v0.1.0"].contains("component"),
        "{refused:?}"
    );

    // A source that serves other contents for a version the forge holds: the held copy stays.
    let mut changing = fixture.manifest.raw.clone();
    changing["id"] = json!("tartan.fixture-changing");
    forge.publish(fixture, &changing).await;
    let held = forge
        .registry
        .manifest("tartan.test/tartan/fixture_changing", "v0.1.0")
        .await
        .expect("read")
        .expect("held");
    let mut changed = fixture.clone();
    changed.migrations[0].1.push_str("\n-- changed\n");
    let report = forge.publish(&changed, &changing).await;
    assert_eq!(report.mismatched.len(), 1, "{report:?}");
    assert_eq!(
        report.mismatched[0].module,
        "tartan.test/tartan/fixture_changing@v0.1.0"
    );
    assert!(
        report
            .registered
            .contains(&"tartan.fixture-changing@0.1.0".to_owned()),
        "the held copy stays registered: {report:?}"
    );
    let still = forge
        .registry
        .manifest("tartan.test/tartan/fixture_changing", "v0.1.0")
        .await
        .expect("read")
        .expect("held");
    assert_eq!(still.1, held.1);

    // An unreachable source is reported; the other modules are still synced.
    let mut deployment = forge.modules.deployment();
    deployment.modules.push(ModuleVersion {
        path: "unreachable.test/nothing".into(),
        version: "v0.1.0".into(),
    });
    deployment.sources.insert(
        "unreachable.test".into(),
        "127.0.0.1:1+insecure".parse().expect("a source"),
    );
    let report = forge.sync_deployment(&deployment).await;
    assert_eq!(report.unreachable.len(), 1, "{report:?}");
    assert_eq!(
        report.unreachable[0].module,
        "unreachable.test/nothing@v0.1.0"
    );
    assert!(
        report
            .registered
            .contains(&"tartan.fixture@0.1.0".to_owned())
    );

    // The forge lists what is available; nothing can be published over HTTP.
    let (status, packages) = forge.get("/-/api/packages").await;
    assert_eq!(status, 200);
    let available: Vec<String> = packages
        .as_array()
        .expect("a list")
        .iter()
        .map(|p| {
            let field = |name: &str| p[name].as_str().unwrap_or_default().to_owned();
            format!("{}@{}", field("id"), field("version"))
        })
        .collect();
    assert_eq!(
        available,
        vec![
            "tartan.fixture@0.1.0",
            "tartan.fixture@0.2.0",
            "tartan.fixture-changing@0.1.0",
        ]
    );
    let (status, _) = forge
        .post("/-/api/packages", json!({"manifest": fixture.manifest.raw}))
        .await;
    assert_eq!(status, 405);
    let (status, _) = forge
        .post(
            "/-/api/orgs/acme/repos/api/extensions",
            json!({"package": FIXTURE, "version": "0.1.0"}),
        )
        .await;
    assert_eq!(status, 405, "there is no install route");
}

async fn the_registry_serves_cue_and_refuses_writes(forge: &Forge) {
    let name = module_path(FIXTURE);
    let http = &forge.http;
    let base = response(http.get(format!("{}/v2/", forge.base))).await;
    assert_eq!(base.status(), 200);
    let head = response(http.head(format!("{}/v2/{name}/manifests/v0.1.0", forge.base))).await;
    assert_eq!(head.status(), 200);
    let digest = header(&head, "docker-content-digest");
    assert!(digest.starts_with("sha256:"), "{digest}");
    assert_eq!(
        header(&head, "content-type"),
        "application/vnd.oci.image.manifest.v1+json"
    );
    let manifest = response(http.get(format!("{}/v2/{name}/manifests/{digest}", forge.base))).await;
    assert_eq!(header(&manifest, "docker-content-digest"), digest);
    let length: usize = header(&manifest, "content-length")
        .parse()
        .expect("a length");
    let manifest: Value = manifest.json().await.expect("json");
    let layer = manifest["layers"][0]["digest"].as_str().expect("a layer");
    let blob = response(http.head(format!("{}/v2/{name}/blobs/{layer}", forge.base))).await;
    assert_eq!(blob.status(), 200);
    assert!(length > 0);
    let tags: Value = response(http.get(format!("{}/v2/{name}/tags/list", forge.base)))
        .await
        .json()
        .await
        .expect("json");
    assert_eq!(tags, json!({"name": name, "tags": ["v0.1.0", "v0.2.0"]}));
    let missing = response(http.get(format!("{}/v2/{name}/manifests/v9.9.9", forge.base))).await;
    assert_eq!(missing.status(), 404);
    let upload = response(http.post(format!("{}/v2/{name}/blobs/uploads/", forge.base))).await;
    assert_eq!(upload.status(), 405);
    let put = response(
        http.put(format!("{}/v2/{name}/manifests/v9.9.9", forge.base))
            .body("{}"),
    )
    .await;
    assert_eq!(put.status(), 405);

    // The cue command resolves the module through the forge, as a person's would.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("cue.mod")).expect("mkdir");
    std::fs::write(
        dir.path().join("cue.mod/module.cue"),
        "module: \"example.com/consumer\"\nlanguage: version: \"v0.14.1\"\n",
    )
    .expect("write");
    std::fs::write(
        dir.path().join("consumer.cue"),
        format!(
            "package consumer\n\nimport \"{name}\"\n\nentry: fixture.#Extension & {{settings: colour: \"red\"}}\n"
        ),
    )
    .expect("write");
    let (tidy, output) = cue(dir.path(), &["mod", "tidy"]).await;
    assert!(tidy, "cue mod tidy: {output}");
    let module = std::fs::read_to_string(dir.path().join("cue.mod/module.cue")).expect("read");
    assert!(module.contains("v0.2.0"), "the latest version: {module}");
    let (exported, output) = cue(dir.path(), &["export"]).await;
    assert!(exported, "cue export: {output}");
    let value: Value = serde_json::from_str(&output).expect("json");
    assert_eq!(
        value,
        json!({"entry": {"id": FIXTURE, "version": "0.2.0", "settings": {"colour": "red"}}})
    );
}

async fn response(request: reqwest::RequestBuilder) -> reqwest::Response {
    request.send().await.expect("the gateway answers")
}

fn header(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

async fn configuration_installs_and_configures(forge: &Forge) {
    // A push to main installs the fixture from plain CUE.
    forge
        .push_config("acme/api", Some(&install("0.1.0", "")))
        .await;
    forge
        .until_installed("acme/api", &["tartan.fixture@0.1.0"])
        .await;
    let tools = format!("/-/api/orgs/acme/repos/api/extensions/{FIXTURE}/tools");
    let seen = forge.ok(&format!("{tools}/seen"), json!({})).await;
    assert!(seen["seen"].is_array(), "{seen}");
    let (status, _) = forge.post(&format!("{tools}/nope"), json!({})).await;
    assert_eq!(status, 404);
    let (status, _) = forge
        .post(
            &format!("/-/api/orgs/acme/repos/api/extensions/{FIXTURE}/render/page"),
            json!({}),
        )
        .await;
    assert_eq!(status, 404);
    let (status, _) = forge
        .post(
            &format!("/-/api/orgs/acme/repos/other/extensions/{FIXTURE}/tools/seen"),
            json!({}),
        )
        .await;
    assert_eq!(status, 404, "the other repository has no fixture");
    let (status, _) = forge
        .post(
            &format!("/-/api/orgs/acme/extensions/{FIXTURE}/tools/seen"),
            json!({}),
        )
        .await;
    assert_eq!(status, 404, "there are no org installations");

    // A push to another branch applies nothing.
    forge
        .push_branch("acme/api", "refs/heads/topic", "package tartan\n")
        .await;

    // Refused configurations change nothing.
    let refusals: Vec<(Vec<(String, String)>, &str)> = vec![
        (
            vec![("tartan.cue".into(), install("9.9.9", ""))],
            "not published",
        ),
        (
            vec![("tartan.cue".into(), "package tartan\nthis is not cue\n".into())],
            "",
        ),
        (
            vec![(
                "tartan.cue".into(),
                format!("package tartan\nextensions: \"{FIXTURE}\": {{version: \"0.1.0\", colour: \"red\"}}\n"),
            )],
            "colour",
        ),
        // A version the module's #Extension does not have conflicts with it.
        (through_module("v0.2.0", "{version: \"0.1.0\"}"), ""),
        // A module the forge does not serve cannot be imported.
        (
            vec![
                (
                    "cue.mod/module.cue".into(),
                    "module: \"example.com/acme-api\"\nlanguage: version: \"v0.14.1\"\ndeps: \"tartan.test/tartan/unlisted@v0\": v: \"v0.1.0\"\n".into(),
                ),
                (
                    "tartan.cue".into(),
                    "package tartan\n\nimport \"tartan.test/tartan/unlisted\"\n\nextensions: \"tartan.unlisted\": unlisted.#Extension\n".into(),
                ),
            ],
            "unlisted",
        ),
    ];
    for (count, (files, problem)) in refusals.iter().enumerate() {
        push(forge, "acme/api", files).await;
        wait_for_count(forge, "config.refused", count + 1).await;
        let refusal = last_refusal(forge).await;
        assert!(refusal.contains(problem), "{problem}: {refusal}");
    }
    assert_eq!(
        forge.installed("acme/api").await,
        vec!["tartan.fixture@0.1.0"]
    );

    // Through the module: its #Extension fixes the version (an upgrade to 0.2.0) and the
    // settings are the repository's.
    push(
        forge,
        "acme/api",
        &through_module("v0.2.0", "{settings: colour: \"red\"}"),
    )
    .await;
    forge
        .until_installed("acme/api", &["tartan.fixture@0.2.0"])
        .await;
    wait_for_count(forge, "extension.upgraded", 1).await;

    // Removing it uninstalls it; adding it back restores it with its data.
    let before = forge.ok(&format!("{tools}/seen"), json!({})).await;
    forge
        .push_config("acme/api", Some("package tartan\n"))
        .await;
    forge.until_installed("acme/api", &[]).await;
    let (status, _) = forge.post(&format!("{tools}/seen"), json!({})).await;
    assert_eq!(status, 404);
    forge
        .push_config("acme/api", Some(&install("0.2.0", "colour: \"red\"")))
        .await;
    forge
        .until_installed("acme/api", &["tartan.fixture@0.2.0"])
        .await;
    let after = forge.ok(&format!("{tools}/seen"), json!({})).await;
    let rows = |value: &Value| value["seen"].as_array().map_or(0, Vec::len);
    assert!(rows(&after) >= rows(&before), "{before} then {after}");
    assert!(
        rows(&before) > 0,
        "the fixture saw the configuration pushes"
    );
}
