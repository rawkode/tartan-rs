//! Identity and signing end to end (design D12, *Tests*): the real Rivet engine, walgit on
//! RustFS, the gateway, tartan-devnet and the real `tt` and `git`.
//!
//! A person signs in through the devnet, logs `tt` in through the CLI hand-off (the forge's
//! approval, then the devnet's signing consent) and makes a token; `tt sign --setup` configures
//! a clone, and signed commits pushed through walgit are verified by the forge. A commit made
//! while the devnet's PDS is offline is verified once `tt sign --flush` publishes its record.
//! `git verify-commit` checks a signature with `tt`. On a `main` protected by the signing
//! rule, an unsigned commit is a violation and its configuration is refused.
//!
//! `tt` runs with `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` under the
//! test's temporary directory and `TT_CREDENTIAL_STORE=file`. Needs `WALGIT_BIN`,
//! `RIVET_ENGINE_BINARY_PATH`, and `rustfs`, `rclone` and `git` on `PATH`; `TT_BIN` names a
//! `dev-identity` `tt` (otherwise one is built). `scripts/e2e.sh` provides them.

mod common;

use std::net::TcpListener as StdListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rivetkit::Registry;
use serde_json::{Value, json};
use tartan::{GatewayState, RegistryStore, router};
use tartan_config::deployment::Readers;
use tartan_forge::{
    Extensions, GitCache, IdentityLimits, Services, WalgitClient, install_services, register_actors,
};
use tokio::process::{Child, Command};

use common::tt::{Answer, Machine, text, tt_binary};

const WEBHOOK_SECRET: &str = "e2e-webhook-secret";
const GIT_HOST_TOKEN: &str = "e2e-git-host-token";
const S3_ACCESS_KEY: &str = "tartane2e";
const S3_SECRET_KEY: &str = "tartane2esecret";
const BUCKET: &str = "tartan-e2e";
/// Signature checks retry with backoff, so a check waiting on a record can take a while.
const DEADLINE: Duration = Duration::from_secs(120);

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

fn walgit_config(dir: &Path, port: u16, s3_port: u16, gateway_port: u16) -> PathBuf {
    let path = dir.join("walgit.toml");
    let cache = dir.join("walgit-cache");
    let config = format!(
        r#"
[server]
listen = "127.0.0.1:{port}"
public_url = "http://127.0.0.1:{port}"
auto_create_on_push = false

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

/// Runs `git` as `machine`'s person in `dir`, with the walgit token and no system or global
/// configuration, and answers its output.
fn git(machine: &Machine, dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .env_clear()
        .envs(machine.envs())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Alice")
        .env("GIT_AUTHOR_EMAIL", "alice@example.com")
        .env("GIT_COMMITTER_NAME", "Alice")
        .env("GIT_COMMITTER_EMAIL", "alice@example.com")
        .args([
            "-c",
            &format!("http.extraHeader=Authorization: Bearer {GIT_HOST_TOKEN}"),
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
}

/// [`git`], asserting success; answers standard output, trimmed.
fn git_ok(machine: &Machine, dir: &Path, args: &[&str]) -> String {
    let output = git(machine, dir, args);
    assert!(output.status.success(), "git {args:?}: {}", text(&output));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The forge as one person's browser and token see it.
struct Gateway {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Gateway {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.expect("the gateway answers");
        let status = response.status();
        let body: Value = response.json().await.unwrap_or_default();
        assert!(status.is_success(), "{path}: {status} {body}");
        body
    }

    /// Every event the `all` subscriber received, as `(type, data)`.
    async fn events(&self) -> Vec<(String, Value)> {
        let received = self
            .call(
                reqwest::Method::GET,
                "/-/api/subscribers/all/events?limit=500",
                None,
            )
            .await;
        received
            .as_array()
            .expect("a list")
            .iter()
            .map(|r| {
                (
                    r["event"]["type"].as_str().unwrap_or_default().to_owned(),
                    r["event"]["data"].clone(),
                )
            })
            .collect()
    }

    /// Waits for an event of `kind` whose data satisfies `found`, and answers its data.
    async fn event(&self, kind: &str, found: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let events = self.events().await;
            if let Some((_, data)) = events.iter().find(|(k, d)| k == kind && found(d)) {
                return data.clone();
            }
            assert!(
                start.elapsed() < DEADLINE,
                "no {kind} event as expected; events: {events:?}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// Whether a `ref.signatures.checked` event judged `oid` `verified` as signed by `did`.
fn verified(data: &Value, oid: &str, did: &str) -> bool {
    data["objects"].as_array().is_some_and(|objects| {
        objects
            .iter()
            .any(|o| o["oid"] == oid && o["status"] == "verified" && o["did"] == did)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs WALGIT_BIN, rustfs, rclone, git and a Rivet engine; run scripts/e2e.sh"]
async fn sign_in_sign_push_and_protect_main() {
    let tt = tt_binary();
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
        .args(["mkdir", &format!("e2e:{BUCKET}")])
        .env("RCLONE_CONFIG_E2E_TYPE", "s3")
        .env("RCLONE_CONFIG_E2E_PROVIDER", "Other")
        .env(
            "RCLONE_CONFIG_E2E_ENDPOINT",
            format!("http://127.0.0.1:{s3_port}"),
        )
        .env("RCLONE_CONFIG_E2E_ACCESS_KEY_ID", S3_ACCESS_KEY)
        .env("RCLONE_CONFIG_E2E_SECRET_ACCESS_KEY", S3_SECRET_KEY)
        .status()
        .await
        .expect("rclone runs");
    assert!(made.success(), "create the bucket");

    // The gateway's listener first, so walgit can be told where to post events. Its origin is
    // TARTAN_PUBLIC_URL: no web server runs.
    let listener = common::gateway_listener().await;
    let gateway_port = listener.local_addr().expect("address").port();
    let base = format!("http://127.0.0.1:{gateway_port}");

    let walgit_port = free_port();
    let config = walgit_config(dir, walgit_port, s3_port, gateway_port);
    let walgit_bin = std::env::var("WALGIT_BIN").expect("WALGIT_BIN names a walgit binary");
    let _walgit = spawn(
        Command::new(walgit_bin)
            .arg("--config")
            .arg(&config)
            .env("TARTAN_S3_ACCESS_KEY", S3_ACCESS_KEY)
            .env("TARTAN_S3_SECRET_KEY", S3_SECRET_KEY),
        &logs.join("walgit.log"),
    );
    wait_for_port(walgit_port, "walgit", &logs.join("walgit.log")).await;

    // The actors, with the devnet as the identity network, and the gateway.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let forge_id = format!("e2e-{nanos}");
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let registry = RegistryStore::new(store.clone());
    let extensions = Arc::new(Extensions::new(
        tartan_extensions::ExtensionRuntime::new().expect("runtime"),
        store.clone(),
    ));
    let identity = common::IdentityWorld::start().await;
    let devnet = identity.devnet.clone();
    let alice = identity.owner.clone();
    let clock = Arc::new(common::TestClock::default());
    install_services(Services {
        extensions: extensions.clone(),
        forge_id: forge_id.clone(),
        git_host: Arc::new(
            WalgitClient::new(
                &format!("http://127.0.0.1:{walgit_port}"),
                Some(GIT_HOST_TOKEN.to_owned()),
            )
            .with_cache(GitCache::new(
                dir.join("state/git"),
                tartan_forge::GIT_CACHE_MAX_BYTES,
            )),
        ),
        clock: clock.clone(),
        owners: vec![alice.clone()],
        readers: Readers::default(),
        resolver: identity.resolver.clone(),
        identity_limits: IdentityLimits::default(),
        objects: store,
        state_dir: dir.join("state"),
    })
    .expect("installed once");
    extensions.set_available([]);
    let mut actor_registry = Registry::new();
    register_actors(&mut actor_registry);
    let actors = common::Actors::start(actor_registry, "e2e").await;
    let client = actors.client().clone();
    let state = GatewayState {
        client: client.clone(),
        access: identity.access(&base, Readers::default(), clock),
        webhook_secret: Arc::from(WEBHOOK_SECRET.as_bytes()),
        registry,
    };
    common::serve(listener, router(state));

    // Sign in through the devnet: a browser session for the owner.
    let session = common::sign_in(&base, "alice.test", "/", None)
        .await
        .session();

    // `tt login` through the CLI hand-off: the forge's approval as that browser, then the
    // devnet's signing consent, which approves at once for the login hint.
    let laptop = Machine::on_network(&tt, devnet.url(), &forge_id);
    let port = free_port().to_string();
    let output = laptop
        .login(
            &["login", "--forge", &base, "--no-open", "--port", &port],
            vec![
                Answer::Forge {
                    base: &base,
                    session: &session,
                    approve: true,
                },
                Answer::Follow { extra: "" },
            ],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        laptop
            .config()
            .contains(&format!("default_signing_did = \"{alice}\"")),
        "{}",
        laptop.config()
    );

    // `tt token create` from the CLI session; the token drives the rest of the test.
    let created = laptop.run(
        &[
            "token",
            "create",
            "--name",
            "e2e",
            "--scopes",
            "read,write,admin",
            "--expires-in-days",
            "1",
        ],
        None,
    );
    assert!(created.status.success(), "{}", text(&created));
    let token = String::from_utf8_lossy(&created.stdout)
        .lines()
        .find(|line| line.starts_with("tartan_pat_"))
        .expect("the secret is printed once")
        .to_owned();
    let forge = Gateway {
        base: base.clone(),
        token,
        http: reqwest::Client::new(),
    };
    forge
        .call(
            reqwest::Method::PUT,
            "/-/api/subscribers/all",
            Some(json!({"pattern": ">"})),
        )
        .await;
    forge
        .call(
            reqwest::Method::POST,
            "/-/api/orgs",
            Some(json!({"slug": "acme"})),
        )
        .await;
    let repo = forge
        .call(
            reqwest::Method::POST,
            "/-/api/orgs/acme/repos",
            Some(json!({"slug": "api"})),
        )
        .await;
    let clone_url = repo["clone_url"].as_str().expect("clone url").to_owned();

    // `tt sign --setup` in a clone, then a signed commit pushed through walgit is verified as
    // signed by the owner.
    let work = dir.join("work");
    git_ok(
        &laptop,
        dir,
        &[
            "clone",
            "--quiet",
            &clone_url,
            work.to_str().expect("utf-8"),
        ],
    );
    let setup = std::process::Command::new(&tt)
        .env_clear()
        .envs(laptop.envs())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["sign", "--setup"])
        .current_dir(&work)
        .output()
        .expect("tt sign --setup");
    assert!(setup.status.success(), "{}", text(&setup));
    assert_eq!(git_ok(&laptop, &work, &["config", "gpg.format"]), "x509");
    assert_eq!(
        git_ok(&laptop, &work, &["config", "user.signingKey"]),
        alice
    );
    std::fs::write(work.join("README.md"), "signed\n").expect("write");
    git_ok(&laptop, &work, &["add", "README.md"]);
    git_ok(&laptop, &work, &["commit", "--quiet", "-S", "-m", "signed"]);
    let first = git_ok(&laptop, &work, &["rev-parse", "HEAD"]);
    git_ok(&laptop, &work, &["push", "--quiet", "origin", "HEAD:main"]);
    forge
        .event("ref.signatures.checked", |d| verified(d, &first, &alice))
        .await;

    // `git verify-commit` succeeds with tt.
    let verify = git(&laptop, &work, &["verify-commit", "HEAD"]);
    assert!(verify.status.success(), "{}", text(&verify));

    // A commit made while the PDS is offline is queued, pushed, and verified once
    // `tt sign --flush` publishes its record.
    devnet.set_switches(|s| s.pds_offline = true).await;
    std::fs::write(work.join("README.md"), "signed offline\n").expect("write");
    git_ok(
        &laptop,
        &work,
        &["commit", "--quiet", "-S", "-am", "offline"],
    );
    let offline = git_ok(&laptop, &work, &["rev-parse", "HEAD"]);
    git_ok(&laptop, &work, &["push", "--quiet", "origin", "HEAD:main"]);
    devnet.set_switches(|s| s.pds_offline = false).await;
    // The failed write opened tt's two-minute back-off window; removing its state file stands
    // in for waiting it out.
    if let Ok(entries) = std::fs::read_dir(laptop.home.path().join("state/tt/queue")) {
        for entry in entries.filter_map(Result::ok) {
            if entry.file_name().to_string_lossy().ends_with(".state.json") {
                std::fs::remove_file(entry.path()).expect("remove the window");
            }
        }
    }
    let flush = laptop.run(&["sign", "--flush"], None);
    assert!(flush.status.success(), "{}", text(&flush));
    forge
        .event("ref.signatures.checked", |d| verified(d, &offline, &alice))
        .await;

    // Protect main with the signing rule, in a configuration commit the owner signs.
    std::fs::create_dir_all(work.join("cue.mod")).expect("mkdir");
    std::fs::write(work.join("cue.mod/module.cue"), common::PLAIN_MODULE).expect("write");
    std::fs::write(
        work.join("tartan.cue"),
        format!(
            "package tartan\n\nsigning: require: [{{refs: [\"refs/heads/main\"], signers: [\"{alice}\"]}}]\n"
        ),
    )
    .expect("write");
    git_ok(&laptop, &work, &["add", "cue.mod/module.cue", "tartan.cue"]);
    git_ok(
        &laptop,
        &work,
        &["commit", "--quiet", "-S", "-m", "Protect main"],
    );
    let protect = git_ok(&laptop, &work, &["rev-parse", "HEAD"]);
    git_ok(&laptop, &work, &["push", "--quiet", "origin", "HEAD:main"]);
    forge
        .event("config.applied", |d| d["sha"] == protect.as_str())
        .await;

    // An unsigned commit to the protected main: a violation, and its configuration refused.
    std::fs::write(work.join("tartan.cue"), "package tartan\n").expect("write");
    git_ok(
        &laptop,
        &work,
        &[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-am",
            "Drop the rule, unsigned",
        ],
    );
    let unsigned = git_ok(&laptop, &work, &["rev-parse", "HEAD"]);
    git_ok(&laptop, &work, &["push", "--quiet", "origin", "HEAD:main"]);
    let violation = forge
        .event("ref.signature.violation", |d| d["new"] == unsigned.as_str())
        .await;
    assert_eq!(violation["ref_name"], "refs/heads/main", "{violation}");
    let refused = forge
        .event("config.refused", |d| d["sha"] == unsigned.as_str())
        .await;
    assert!(
        refused.to_string().contains("unsigned"),
        "the refusal names the unsigned commit: {refused}"
    );
    let main = forge
        .call(
            reqwest::Method::GET,
            "/-/api/orgs/acme/repos/api/signatures?ref=refs/heads/main",
            None,
        )
        .await;
    assert_eq!(main["compliant"], false, "{main}");
    assert_eq!(main["status"], "unsigned", "{main}");
    let status = forge
        .call(
            reqwest::Method::GET,
            "/-/api/orgs/acme/repos/api/config/status",
            None,
        )
        .await;
    assert_eq!(status["applied"]["commit"], protect.as_str(), "{status}");

    actors.shutdown().await;
}
