//! Git through the whole stack: RustFS, walgit, the Rivet engine and the gateway, driven by the
//! `git` CLI.
//!
//! Needs `WALGIT_BIN`, `RIVET_ENGINE_BINARY_PATH`, `TARTAN_ISSUES_PACKAGE`, the module source
//! (`TARTAN_MODULE_SOURCE`, `CUE_BIN`), and `rustfs`, `rclone` and `git` on `PATH`;
//! `scripts/e2e.sh` provides them.

mod common;

use std::net::TcpListener as StdListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rivetkit::Registry;
use serde_json::{Value, json};
use tartan::{GatewayState, RegistryStore, router};
use tartan_forge::{
    Extensions, GitHost, Services, WalgitClient, install_services, register_actors,
};
use tokio::process::{Child, Command};

const ADMIN_TOKEN: &str = "git-test-admin-token";
const WEBHOOK_SECRET: &str = "git-test-webhook-secret";
const GIT_HOST_TOKEN: &str = "git-test-host-token";
const S3_ACCESS_KEY: &str = "tartantest";
const S3_SECRET_KEY: &str = "tartantestsecret";
const BUCKET: &str = "tartan-test";
const DEADLINE: Duration = Duration::from_secs(60);

fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

async fn wait_for_port(port: u16, what: &str, log: &Path) {
    let start = Instant::now();
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_err()
    {
        assert!(
            start.elapsed() < DEADLINE,
            "{what} did not start on {port}:\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A child process that is killed when the test ends.
fn spawn(command: &mut Command, log: &Path) -> Child {
    let file = std::fs::File::create(log).expect("log file");
    command
        .stdout(Stdio::from(file.try_clone().expect("clone log handle")))
        .stderr(Stdio::from(file))
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"))
}

async fn run(dir: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .output()
        .await
        .unwrap_or_else(|error| panic!("run {program}: {error}"));
    assert!(
        output.status.success(),
        "{program} {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn work_dir(dir: &Path) -> PathBuf {
    dir.join("work")
}

fn walgit_config(dir: &Path, port: u16, s3_port: u16, gateway_port: u16, roles: &str) -> PathBuf {
    let path = dir.join(format!("walgit-{port}.toml"));
    let cache = dir.join(format!("walgit-cache-{port}"));
    let config = format!(
        r#"
[server]
listen = "127.0.0.1:{port}"
public_url = "http://127.0.0.1:{port}"
auto_create_on_push = false
roles = {roles}

[server.tls]
mode = "off"

[server.auth]
mode = "token"
anonymous_read = true
tokens = [{{ principal = "tartan", token = "{GIT_HOST_TOKEN}", write = true, admin = true }}]

[store]
backend = "s3"
bucket = "{BUCKET}"
prefix = "git"

[store.s3]
endpoint = "http://127.0.0.1:{s3_port}"
region = "us-east-1"
access_key_env = "TARTAN_S3_ACCESS_KEY"
secret_key_env = "TARTAN_S3_SECRET_KEY"
force_path_style = true

[cache]
dir = "{cache}"
max_bytes = "1GiB"

[events]
webhook_url = "http://127.0.0.1:{gateway_port}/-/hooks/git-host"
webhook_secret = "{WEBHOOK_SECRET}"
sweep_interval = "1s"
"#,
        cache = cache.display(),
    );
    std::fs::write(&path, config).expect("write walgit config");
    path
}

fn walgit(config: &Path, log: &Path) -> Child {
    let binary = std::env::var("WALGIT_BIN").expect("WALGIT_BIN names a walgit binary");
    spawn(
        Command::new(binary)
            .arg("--config")
            .arg(config)
            .env("TARTAN_S3_ACCESS_KEY", S3_ACCESS_KEY)
            .env("TARTAN_S3_SECRET_KEY", S3_SECRET_KEY),
        log,
    )
}

async fn api(http: &reqwest::Client, base: &str, path: &str, body: Value) -> Value {
    let response = http
        .post(format!("{base}{path}"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&body)
        .send()
        .await
        .expect("gateway answers");
    let status = response.status();
    let body: Value = response.json().await.expect("json");
    assert!(status.is_success(), "{path}: {status} {body}");
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs WALGIT_BIN, rustfs, rclone and a Rivet engine; run scripts/e2e.sh"]
async fn git_round_trips_through_walgit_and_emits_ref_events() {
    let temp = tempfile::tempdir().expect("temp dir");
    let dir = temp.path();
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).expect("logs dir");

    // Object storage.
    let s3_port = free_port();
    std::fs::create_dir_all(dir.join("rustfs")).expect("rustfs dir");
    let _rustfs = spawn(
        Command::new("rustfs")
            .args(["server", "--address", &format!("127.0.0.1:{s3_port}")])
            .arg(dir.join("rustfs"))
            .env("RUSTFS_ACCESS_KEY", S3_ACCESS_KEY)
            .env("RUSTFS_SECRET_KEY", S3_SECRET_KEY),
        &logs.join("rustfs.log"),
    );
    wait_for_port(s3_port, "rustfs", &logs.join("rustfs.log")).await;
    let made = Command::new("rclone")
        .args(["mkdir", &format!("test:{BUCKET}")])
        .env("RCLONE_CONFIG_TEST_TYPE", "s3")
        .env("RCLONE_CONFIG_TEST_PROVIDER", "Other")
        .env(
            "RCLONE_CONFIG_TEST_ENDPOINT",
            format!("http://127.0.0.1:{s3_port}"),
        )
        .env("RCLONE_CONFIG_TEST_ACCESS_KEY_ID", S3_ACCESS_KEY)
        .env("RCLONE_CONFIG_TEST_SECRET_ACCESS_KEY", S3_SECRET_KEY)
        .status()
        .await
        .expect("rclone runs");
    assert!(made.success(), "create the bucket");

    // The gateway's listener first, so walgit can be told where to post events.
    let listener = common::gateway_listener().await;
    let gateway_port = listener.local_addr().expect("address").port();
    let base = format!("http://127.0.0.1:{gateway_port}");

    // walgit: one host with every role, including the events bridge.
    let walgit_port = free_port();
    let config = walgit_config(dir, walgit_port, s3_port, gateway_port, "[]");
    let _walgit = walgit(&config, &logs.join("walgit.log"));
    wait_for_port(walgit_port, "walgit", &logs.join("walgit.log")).await;

    // The actors and the gateway.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let registry = RegistryStore::new(store.clone());
    let extensions = Arc::new(Extensions::new(
        tartan_extensions::ExtensionRuntime::new().expect("runtime"),
        store,
    ));
    install_services(Services {
        extensions: extensions.clone(),
        forge_id: format!("git-test-{nanos}"),
        git_host: Arc::new(WalgitClient::new(
            &format!("http://127.0.0.1:{walgit_port}"),
            Some(GIT_HOST_TOKEN.to_owned()),
        )),
    })
    .expect("installed once");
    let mut actor_registry = Registry::new();
    register_actors(&mut actor_registry);
    let actors = rivetkit::test::setup(actor_registry)
        .await
        .expect("engine and registry");
    let client = actors.client().clone();
    let state = GatewayState {
        client: client.clone(),
        admin_token: Arc::from(ADMIN_TOKEN),
        webhook_secret: Arc::from(WEBHOOK_SECRET.as_bytes()),
        registry: registry.clone(),
    };
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    let http = reqwest::Client::new();
    let subscribed = http
        .put(format!("{base}/-/api/subscribers/refs"))
        .bearer_auth(ADMIN_TOKEN)
        .json(&json!({"pattern": "ref.>"}))
        .send()
        .await
        .expect("answers");
    assert_eq!(subscribed.status().as_u16(), 204);
    api(&http, &base, "/-/api/orgs", json!({"slug": "acme"})).await;
    let repo = api(
        &http,
        &base,
        "/-/api/orgs/acme/repos",
        json!({"slug": "api"}),
    )
    .await;
    let clone_url = repo["clone_url"].as_str().expect("clone url").to_owned();
    // walgit takes the token up front as a bearer header; it never challenges for one.
    let auth = format!("http.extraHeader=Authorization: Bearer {GIT_HOST_TOKEN}");
    let push = |refspec: &'static str| {
        let auth = auth.clone();
        let url = clone_url.clone();
        let work = work_dir(dir);
        async move {
            run(
                &work,
                "git",
                &["-c", &auth, "push", "--quiet", &url, refspec],
            )
            .await
        }
    };

    // Push: create main, a branch, update main, delete the branch.
    let work = work_dir(dir);
    std::fs::create_dir_all(&work).expect("work dir");
    run(&work, "git", &["init", "--quiet", "--initial-branch=main"]).await;
    std::fs::write(work.join("README.md"), "one\n").expect("write");
    run(&work, "git", &["add", "README.md"]).await;
    run(&work, "git", &["commit", "--quiet", "-m", "one"]).await;
    let first = run(&work, "git", &["rev-parse", "HEAD"]).await;
    push("main").await;
    push("main:refs/heads/topic").await;
    std::fs::write(work.join("README.md"), "two\n").expect("write");
    run(&work, "git", &["commit", "--quiet", "-am", "two"]).await;
    let second = run(&work, "git", &["rev-parse", "HEAD"]).await;
    push("main").await;
    push(":refs/heads/topic").await;

    // Clone from the host that took the pushes.
    let clone = dir.join("clone");
    run(
        dir,
        "git",
        &[
            "clone",
            "--quiet",
            &clone_url,
            clone.to_str().expect("utf-8"),
        ],
    )
    .await;
    assert_eq!(run(&clone, "git", &["rev-parse", "HEAD"]).await, second);
    let readme = std::fs::read_to_string(clone.join("README.md")).expect("read");
    assert_eq!(readme, "two\n");

    // Clone from a second walgit host with an empty cache: the bucket is the repository.
    let reader_port = free_port();
    let reader_config = walgit_config(dir, reader_port, s3_port, gateway_port, r#"["serve"]"#);
    let _reader = walgit(&reader_config, &logs.join("walgit-reader.log"));
    wait_for_port(
        reader_port,
        "second walgit",
        &logs.join("walgit-reader.log"),
    )
    .await;
    let reader_url = clone_url.replace(&walgit_port.to_string(), &reader_port.to_string());
    let from_reader = dir.join("from-reader");
    run(
        dir,
        "git",
        &[
            "clone",
            "--quiet",
            &reader_url,
            from_reader.to_str().expect("utf-8"),
        ],
    )
    .await;
    assert_eq!(
        run(&from_reader, "git", &["rev-parse", "HEAD"]).await,
        second
    );

    // The ref events, once each, in push order.
    let start = Instant::now();
    let updates = loop {
        let events: Value = http
            .get(format!("{base}/-/api/subscribers/refs/events?limit=500"))
            .bearer_auth(ADMIN_TOKEN)
            .send()
            .await
            .expect("answers")
            .json()
            .await
            .expect("json");
        let updates: Vec<(String, String, String)> = events
            .as_array()
            .expect("a list")
            .iter()
            .map(|received| {
                let data = &received["event"]["data"];
                let text = |key: &str| data[key].as_str().unwrap_or_default().to_owned();
                (text("action"), text("ref_name"), text("new"))
            })
            .collect();
        if updates.len() >= 4 {
            break updates;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "ref events so far: {updates:?}; walgit log:\n{}",
            std::fs::read_to_string(logs.join("walgit.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let zero = "0".repeat(40);
    assert_eq!(
        updates,
        vec![
            (
                "create".to_owned(),
                "refs/heads/main".to_owned(),
                first.clone()
            ),
            (
                "create".to_owned(),
                "refs/heads/topic".to_owned(),
                first.clone()
            ),
            (
                "update".to_owned(),
                "refs/heads/main".to_owned(),
                second.clone()
            ),
            ("delete".to_owned(), "refs/heads/topic".to_owned(), zero),
        ]
    );

    // The forge reads files at a commit through walgit.
    let walgit_client = WalgitClient::new(
        &format!("http://127.0.0.1:{walgit_port}"),
        Some(GIT_HOST_TOKEN.to_owned()),
    );
    let readme = walgit_client
        .read_file("acme", "api", &second, "README.md")
        .await
        .expect("read a file");
    assert_eq!(readme.as_deref(), Some(&b"two\n"[..]));
    let missing = walgit_client
        .read_file("acme", "api", &second, "tartan.cue")
        .await
        .expect("read a missing file");
    assert_eq!(missing, None);

    // A tartan.cue pushed to main installs Issues from the module the forge synced.
    let issues = tartan_extensions::ExtensionPackage::from_dir(
        std::env::var("TARTAN_ISSUES_PACKAGE")
            .expect("TARTAN_ISSUES_PACKAGE names the Issues package")
            .as_ref(),
    )
    .expect("the Issues package");
    let modules = common::ModuleSource::default();
    modules.publish(&issues, &issues.manifest.raw, None).await;
    let report = common::sync(&modules.deployment(), &registry, &client, &extensions).await;
    assert_eq!(
        report.registered,
        vec![format!("rawkode.issues@{}", issues.manifest.version)],
        "{report:?}"
    );
    let config = format!(
        "package tartan\n\nextensions: \"rawkode.issues\": version: \"{}\"\n",
        issues.manifest.version
    );
    std::fs::write(work.join("tartan.cue"), config).expect("write");
    run(&work, "git", &["add", "tartan.cue"]).await;
    run(&work, "git", &["commit", "--quiet", "-m", "Install Issues"]).await;
    push("main").await;
    let start = Instant::now();
    loop {
        let listed: Value = http
            .get(format!("{base}/-/api/orgs/acme/repos/api/extensions"))
            .bearer_auth(ADMIN_TOKEN)
            .send()
            .await
            .expect("answers")
            .json()
            .await
            .expect("json");
        if listed.to_string().contains("rawkode.issues") {
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "Issues was not installed from the pushed tartan.cue: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let created = api(
        &http,
        &base,
        "/-/api/orgs/acme/repos/api/extensions/rawkode.issues/tools/create",
        json!({"title": "Installed by Git"}),
    )
    .await;
    assert_eq!(created["issue"]["number"], 1);

    actors.shutdown().await;
}
