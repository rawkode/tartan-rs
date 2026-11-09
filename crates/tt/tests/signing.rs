//! End-to-end tests of `tt sign` with real Git against the development network (design D12,
//! *Tests*): the signing login, Git's signing and verification calls, the offline queue, the
//! setup and the pre-push hook.
//!
//! Every `tt` and `git` process runs with `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
//! `XDG_CACHE_HOME` under the test's own temporary directory and `TT_CREDENTIAL_STORE=file`, so
//! a test never reads or writes the developer's `tt` defaults, credentials, queue, cache or Git
//! configuration. A small in-process server stands in for the forge's
//! `/-/auth/tt/client-metadata.json`.

#![cfg(feature = "dev-identity")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use axum::Json;
use axum::routing::get;
use serde_json::{Value, json};
use tartan_devnet::{Devnet, DevnetConfig, RateLimit};
use tokio::io::{AsyncBufReadExt as _, BufReader};

const TT: &str = env!("CARGO_BIN_EXE_tt");
const APPLY: &str = "com.atproto.repo.applyWrites";
const GET_PROOF: &str = "com.atproto.sync.getRecord";
const CREATE_SCOPE: &str = "atproto repo:academy.rawkode.tartan.signature?action=create";

struct World {
    devnet: Devnet,
    forge: String,
    home: tempfile::TempDir,
    http: reqwest::Client,
    _metadata: tokio::task::JoinHandle<()>,
}

impl World {
    async fn start() -> Self {
        let devnet = Devnet::start(DevnetConfig::default())
            .await
            .expect("devnet");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let forge = format!("http://{}", listener.local_addr().expect("address"));
        let document = tt_metadata(&forge);
        let router = axum::Router::new().route(
            "/-/auth/tt/client-metadata.json",
            get(move || async move { Json(document) }),
        );
        let metadata = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        World {
            devnet,
            forge,
            home: tempfile::tempdir().expect("temp"),
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("http"),
            _metadata: metadata,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn env(&self, command: &mut Command) {
        let home = self.home();
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("TT_CREDENTIAL_STORE", "file")
            .env("TARTAN_DEV_IDENTITY", "1")
            .env("TARTAN_PLC_URL", self.devnet.url())
            .env("TARTAN_DEV_HANDLE_RESOLVER", self.devnet.url())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.com")
            .env("GIT_COMMITTER_NAME", "Alice")
            .env("GIT_COMMITTER_EMAIL", "alice@example.com");
    }

    fn tt(&self, args: &[&str]) -> Output {
        let mut command = Command::new(TT);
        self.env(&mut command);
        command
            .args(args)
            .current_dir(self.home())
            .stdin(Stdio::null())
            .output()
            .expect("run tt")
    }

    fn git(&self, repo: &Path, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        self.env(&mut command);
        command
            .args(args)
            .current_dir(repo)
            .stdin(Stdio::null())
            .output()
            .expect("run git")
    }

    fn git_ok(&self, repo: &Path, args: &[&str]) -> String {
        let output = self.git(repo, args);
        assert!(output.status.success(), "git {args:?}: {}", text(&output));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A repository whose Git signs and verifies with this `tt` through `-c`-style local
    /// settings in its own `.git/config`.
    fn repo(&self, name: &str, did: Option<&str>) -> PathBuf {
        let repo = self.home().join(name);
        std::fs::create_dir_all(&repo).expect("mkdir");
        self.git_ok(&repo, &["init", "-q", "-b", "main"]);
        self.git_ok(&repo, &["config", "gpg.format", "x509"]);
        self.git_ok(&repo, &["config", "gpg.x509.program", TT]);
        if let Some(did) = did {
            self.git_ok(&repo, &["config", "user.signingKey", did]);
        }
        repo
    }

    fn commit(&self, repo: &Path, message: &str) -> Output {
        std::fs::write(repo.join("file.txt"), message).expect("write");
        self.git_ok(repo, &["add", "file.txt"]);
        self.git(repo, &["commit", "-q", "-S", "-m", message])
    }

    fn commit_ok(&self, repo: &Path, message: &str) {
        let output = self.commit(repo, message);
        assert!(output.status.success(), "commit: {}", text(&output));
    }

    /// `tt sign --login` for `handle` with extra `args`, approved by following the printed
    /// URL with a plain HTTP client (the devnet auto-approves the `login_hint`).
    async fn sign_login(&self, handle: &str, args: &[&str]) -> Output {
        let mut all = vec!["sign", "--login", "--handle", handle, "--no-open"];
        all.extend_from_slice(args);
        self.run_login(&all, None).await
    }

    /// Runs a `tt` login command, follows the printed URL (adding `extra` to it) and returns
    /// the finished process's output.
    async fn run_login(&self, args: &[&str], extra: Option<&str>) -> Output {
        let mut command = tokio::process::Command::new(TT);
        let mut std_command = Command::new(TT);
        self.env(&mut std_command);
        command
            .env_clear()
            .envs(
                std_command.get_envs().filter_map(|(key, value)| {
                    value.map(|value| (key.to_owned(), value.to_owned()))
                }),
            )
            .args(args)
            .current_dir(self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn tt");
        let stderr = child.stderr.take().expect("stderr");
        let mut lines = BufReader::new(stderr).lines();
        let mut seen = String::new();
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                seen.push_str(&line);
                seen.push('\n');
                let trimmed = line.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    return Some(trimmed.to_owned());
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        if let Some(url) = url {
            let url = format!("{url}{}", extra.unwrap_or_default());
            follow(&self.http, &url).await;
        }
        let rest = tokio::spawn(async move {
            let mut rest = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                rest.push_str(&line);
                rest.push('\n');
            }
            rest
        });
        let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .expect("tt finishes")
            .expect("tt output");
        let rest = rest.await.unwrap_or_default();
        Output {
            status: output.status,
            stdout: output.stdout,
            stderr: format!("{seen}{rest}").into_bytes(),
        }
    }

    async fn did(&self, handle: &str) -> String {
        self.devnet.did(handle).await.expect("seeded person")
    }

    async fn record(&self, did: &str, rkey: &str) -> Option<Value> {
        let response = self
            .http
            .get(format!(
                "{}/xrpc/com.atproto.repo.getRecord?repo={did}&collection=academy.rawkode.tartan.signature&rkey={rkey}",
                self.devnet.url()
            ))
            .send()
            .await
            .expect("getRecord");
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }

    fn queue_len(&self) -> usize {
        let dir = self.home().join("state/tt/queue");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
            .map(|entry| {
                std::fs::read_to_string(entry.path())
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .count()
            })
            .sum()
    }

    /// Ends every open rate-limit or back-off window, as if their time had passed.
    fn close_windows(&self) {
        let dir = self.home().join("state/tt/queue");
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                if entry.file_name().to_string_lossy().ends_with(".state.json") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    fn config_toml(&self) -> String {
        std::fs::read_to_string(self.home().join("config/tt/config.toml")).unwrap_or_default()
    }
}

fn tt_metadata(forge: &str) -> Value {
    json!({
        "client_id": format!("{forge}/-/auth/tt/client-metadata.json"),
        "client_name": "tt (test forge)",
        "client_uri": forge,
        "application_type": "native",
        "redirect_uris": ["http://127.0.0.1/callback", "http://[::1]/callback"],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": "atproto repo:academy.rawkode.tartan.signature?action=create repo:academy.rawkode.tartan.signature?action=delete",
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

/// Follows redirects by hand, as a browser would, until a page answers.
async fn follow(http: &reqwest::Client, start: &str) {
    let mut url = start.to_owned();
    for _ in 0..5 {
        let Ok(response) = http.get(&url).send().await else {
            return;
        };
        if !response.status().is_redirection() {
            return;
        }
        let Some(location) = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
        else {
            return;
        };
        url = location.to_owned();
    }
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The signature block of `object` in `repo` (commit or tag).
fn signature_of(world: &World, repo: &Path, object: &str) -> (String, String) {
    let raw = world.git_ok(repo, &["cat-file", "-p", object]);
    let mut block = String::new();
    let mut inside = false;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("gpgsig ") {
            inside = true;
            block.push_str(rest);
            block.push('\n');
        } else if inside && line.starts_with(' ') {
            block.push_str(&line[1..]);
            block.push('\n');
        } else {
            inside = false;
        }
    }
    let field = |name: &str| {
        block
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or_default()
            .to_owned()
    };
    (field("Record: "), field("Salt: "))
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_login_asks_exactly_the_create_scope_and_never_writes_the_default_forge() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    let output = world.sign_login("alice.test", &["--forge", &forge]).await;
    assert!(output.status.success(), "{}", text(&output));
    // The devnet records the PAR again when it asks for a DPoP nonce first.
    let pars = world.devnet.par_requests().await;
    assert!(!pars.is_empty());
    let scope = pars[0]
        .iter()
        .find(|(key, _)| key == "scope")
        .map(|(_, value)| value.as_str());
    assert_eq!(scope, Some(CREATE_SCOPE));
    let hint = pars[0]
        .iter()
        .find(|(key, _)| key == "login_hint")
        .map(|(_, value)| value.as_str());
    assert_eq!(hint, Some("alice.test"));
    let config = world.config_toml();
    assert!(
        config.contains(&format!("default_signing_did = \"{alice}\"")),
        "{config}"
    );
    assert!(!config.contains("default_forge"), "{config}");
    // No secret outside the credential files.
    assert!(!config.contains("refresh"), "{config}");
    let status = world.tt(&["status"]);
    let shown = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(shown.contains(&alice), "{shown}");
    assert!(shown.contains("/-/auth/tt/client-metadata.json"), "{shown}");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_forge_configured_fails_before_any_request() {
    let world = World::start().await;
    let output = world.tt(&["sign", "--login", "--handle", "alice.test", "--no-open"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output)
            .contains("no forge configured: run `tt login --forge <url>` or pass `--forge`"),
        "{}",
        text(&output)
    );
    assert!(world.devnet.par_requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_granular_scope_fails_with_an_explanation() {
    let world = World::start().await;
    world
        .devnet
        .set_switches(|s| s.par_error = Some("invalid_scope".into()))
        .await;
    let forge = world.forge.clone();
    let output = world.sign_login("alice.test", &["--forge", &forge]).await;
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("granular repo: scopes"), "{message}");
    assert!(message.contains("transition:generic"), "{message}");
    // Only the one PAR, with the create scope: no broader fallback.
    let pars = world.devnet.par_requests().await;
    assert!(pars.iter().all(|par| {
        par.iter()
            .any(|(key, value)| key == "scope" && value == CREATE_SCOPE)
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_localhost_client_declares_both_loopback_redirects_and_scopes() {
    let world = World::start().await;
    let output = world
        .sign_login("bob.test", &["--client", "localhost"])
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let pars = world.devnet.par_requests().await;
    let client_id = pars[0]
        .iter()
        .find(|(key, _)| key == "client_id")
        .map(|(_, value)| value.clone())
        .expect("client_id");
    let url = reqwest::Url::parse(&client_id).expect("url");
    assert_eq!(url.host_str(), Some("localhost"));
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let redirects: Vec<&str> = pairs
        .iter()
        .filter(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        redirects,
        ["http://127.0.0.1/callback", "http://[::1]/callback"]
    );
    let scope = &pairs
        .iter()
        .find(|(key, _)| key == "scope")
        .expect("scope")
        .1;
    assert!(scope.split(' ').any(|token| token == "atproto"));
    assert!(scope.contains("repo:academy.rawkode.tartan.signature?action=create"));
    assert!(scope.contains("repo:academy.rawkode.tartan.signature?action=delete"));
    let redirect = pars[0]
        .iter()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.clone())
        .expect("redirect_uri");
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"));
}

#[tokio::test(flavor = "multi_thread")]
async fn git_signs_and_verifies_commits_and_tags() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.commit_ok(&repo, "first");

    // Record content: exactly the keyed digest, no salt.
    let (uri, salt) = signature_of(&world, &repo, "HEAD");
    assert!(uri.starts_with(&format!("at://{alice}/academy.rawkode.tartan.signature/")));
    let rkey = uri.rsplit('/').next().expect("rkey");
    let record = world.record(&alice, rkey).await.expect("published");
    let value = &record["value"];
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["$type", "algorithm", "createdAt", "digest", "namespace"]
    );
    assert_eq!(value["namespace"], "git");
    assert_eq!(value["algorithm"], "hmac-sha256");
    assert!(!record.to_string().contains(&salt));
    let raw = world.git_ok(&repo, &["cat-file", "commit", "HEAD"]);
    let payload = strip_signature(&raw);
    let salt_bytes = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&salt)
            .expect("salt")
    };
    let digest = {
        use hmac::Mac as _;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&salt_bytes).expect("key");
        mac.update(payload.as_bytes());
        mac.finalize().into_bytes()
    };
    let expected = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    };
    assert_eq!(value["digest"]["$bytes"], expected);

    // verify-commit and verify-tag.
    let verified = world.git(&repo, &["verify-commit", "--raw", "HEAD"]);
    assert!(verified.status.success(), "{}", text(&verified));
    let raw_status = stderr(&verified);
    assert!(
        raw_status.contains(&format!("[GNUPG:] GOODSIG {alice} @alice.test ({alice})")),
        "{raw_status}"
    );
    assert!(
        raw_status.contains("[GNUPG:] TRUST_UNDEFINED 0 tartan"),
        "{raw_status}"
    );
    world.git_ok(&repo, &["tag", "-s", "-m", "v1", "v1"]);
    let tag = world.git(&repo, &["verify-tag", "v1"]);
    assert!(tag.status.success(), "{}", text(&tag));

    // The same payload signed twice gets different salts and records.
    let (first_uri, first_salt) = signature_of(&world, &repo, "HEAD");
    world.git_ok(
        &repo,
        &["commit", "-q", "-S", "--amend", "--no-edit", "--date=now"],
    );
    let (second_uri, second_salt) = signature_of(&world, &repo, "HEAD");
    assert_ne!(first_uri, second_uri);
    assert_ne!(first_salt, second_salt);

    // Untrusted until listed in tt.trustedSigner.
    world.git_ok(&repo, &["checkout", "-q", "-b", "feature"]);
    world.commit_ok(&repo, "feature work");
    world.git_ok(&repo, &["checkout", "-q", "main"]);
    let refused = world.git(
        &repo,
        &["merge", "--verify-signatures", "--no-edit", "feature"],
    );
    assert!(!refused.status.success(), "{}", text(&refused));
    let minimum = world.git(
        &repo,
        &["-c", "gpg.minTrustLevel=fully", "verify-commit", "feature"],
    );
    assert!(!minimum.status.success());
    world.git_ok(&repo, &["config", "--add", "tt.trustedSigner", &alice]);
    let minimum = world.git(
        &repo,
        &["-c", "gpg.minTrustLevel=fully", "verify-commit", "feature"],
    );
    assert!(minimum.status.success(), "{}", text(&minimum));
    let merged = world.git(
        &repo,
        &["merge", "--verify-signatures", "--no-edit", "feature"],
    );
    assert!(merged.status.success(), "{}", text(&merged));

    // A second look within the cache window makes no request.
    world.git_ok(&repo, &["log", "--show-signature"]);
    let before = world.devnet.xrpc_count(GET_PROOF);
    world.git_ok(&repo, &["log", "--show-signature"]);
    assert_eq!(world.devnet.xrpc_count(GET_PROOF), before);

    // A tampered commit: same signature, another message.
    let tampered_raw = raw.replace("first", "forged");
    let tampered = write_object(&world, &repo, "commit", &tampered_raw);
    let bad = world.git(&repo, &["verify-commit", "--raw", &tampered]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("[GNUPG:] BADSIG"), "{}", text(&bad));

    // The Salt line altered after a cached verification: the cache misses, BADSIG.
    let mut other_salt = salt.clone().into_bytes();
    other_salt[0] = if other_salt[0] == b'A' { b'B' } else { b'A' };
    let altered_raw = raw.replace(&salt, &String::from_utf8(other_salt).expect("utf8"));
    let altered = write_object(&world, &repo, "commit", &altered_raw);
    let bad = world.git(&repo, &["verify-commit", "--raw", &altered]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("[GNUPG:] BADSIG"), "{}", text(&bad));
}

/// The commit without its `gpgsig` header: what Git signed.
fn strip_signature(raw: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    let mut in_message = false;
    for line in raw.split_inclusive('\n') {
        if in_message {
            out.push_str(line);
            continue;
        }
        if line == "\n" {
            in_message = true;
            out.push_str(line);
            continue;
        }
        if line.starts_with("gpgsig ") {
            inside = true;
            continue;
        }
        if inside && line.starts_with(' ') {
            continue;
        }
        inside = false;
        out.push_str(line);
    }
    out
}

fn write_object(world: &World, repo: &Path, kind: &str, content: &str) -> String {
    let path = world.home().join("object.txt");
    std::fs::write(&path, content).expect("write");
    world
        .git_ok(
            repo,
            &[
                "hash-object",
                "-t",
                kind,
                "-w",
                path.to_str().expect("utf8"),
            ],
        )
        .trim()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn not_logged_in_fails_naming_tt_sign_login() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let repo = world.repo("repo", Some(&alice));
    let output = world.commit(&repo, "first");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("tt sign --login"),
        "{}",
        text(&output)
    );
    let status = world.tt(&["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("Last signing failure"),
        "{}",
        text(&status)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn configured_as_the_openpgp_program_tt_refuses() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.home().join("pgp");
    std::fs::create_dir_all(&repo).expect("mkdir");
    world.git_ok(&repo, &["init", "-q"]);
    std::fs::write(repo.join("f"), "x").expect("write");
    world.git_ok(&repo, &["add", "f"]);
    let before = world.devnet.xrpc_count(APPLY);
    for format in [None, Some("openpgp")] {
        let mut args = vec!["-c".to_owned(), format!("gpg.program={TT}")];
        if let Some(format) = format {
            args.extend(["-c".to_owned(), format!("gpg.format={format}")]);
        }
        args.extend(
            [
                "-c",
                &format!("user.signingKey={alice}"),
                "commit",
                "-q",
                "-S",
                "-m",
                "x",
            ]
            .map(str::to_owned),
        );
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = world.git(&repo, &refs);
        assert!(!output.status.success());
        assert!(
            stderr(&output).contains("gpg.format=x509"),
            "{}",
            text(&output)
        );
    }
    assert_eq!(
        world.devnet.xrpc_count(APPLY),
        before,
        "no record was written"
    );
    // Git's OpenPGP verify argv.
    let mut command = Command::new(TT);
    world.env(&mut command);
    let output = command
        .args([
            "--keyid-format=long",
            "--status-fd=1",
            "--verify",
            "/dev/null",
            "-",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("tt");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("[GNUPG:] ERRSIG"));
    assert!(stderr(&output).contains("gpg.format=x509"));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_options_and_other_x509_blocks() {
    let world = World::start().await;
    let output = world.tt(&["--status-fd=2", "--clearsign"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!stderr(&output).is_empty());

    let gitsign = world.home().join("gitsign.sig");
    std::fs::write(
        &gitsign,
        "-----BEGIN SIGNED MESSAGE-----\nMIIEVAYJKoZIhvcNAQcCoIIERTCCBEECAQExDTALBglghkgBZQMEAgEw\n-----END SIGNED MESSAGE-----\n",
    )
    .expect("write");
    let output = world.tt(&[
        "--status-fd=1",
        "--verify",
        gitsign.to_str().expect("utf8"),
        "-",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let status = String::from_utf8_lossy(&output.stdout);
    assert!(
        status.starts_with("[GNUPG:] NEWSIG\n[GNUPG:] ERRSIG unsupported"),
        "{status}"
    );
    assert!(stderr(&output).contains("gpg.x509.program=gitsign"));
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_commits_queue_and_publish_later() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));

    // Offline: the commit is created, its record waits.
    world.devnet.set_switches(|s| s.pds_offline = true).await;
    world.commit_ok(&repo, "on a train");
    assert_eq!(world.queue_len(), 1);
    let queued = world.git(&repo, &["verify-commit", "HEAD"]);
    assert!(!queued.status.success());
    assert!(
        stderr(&queued).contains("has not been published yet"),
        "{}",
        text(&queued)
    );
    // Within the back-off window the next commit queues without contacting the PDS.
    world.devnet.set_switches(|s| s.pds_offline = false).await;
    let before = world.devnet.xrpc_count(APPLY);
    world.commit_ok(&repo, "still offline as far as tt knows");
    assert_eq!(world.devnet.xrpc_count(APPLY), before);
    assert_eq!(world.queue_len(), 2);
    let status = world.tt(&["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("2 signature record(s)"),
        "{}",
        text(&status)
    );

    // Back online after the window: the new record alone, then the queued ones.
    world.close_windows();
    let before = world.devnet.xrpc_count(APPLY);
    world.commit_ok(&repo, "back online");
    // One call for the new record and a later one for the queue; the first request to the
    // PDS may also be answered with a fresh DPoP nonce and repeated.
    let calls = world.devnet.xrpc_count(APPLY) - before;
    assert!((2..=3).contains(&calls), "{calls} applyWrites requests");
    assert_eq!(world.queue_len(), 0);
    let log = world.git(&repo, &["log", "--format=%G?", "-3"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "U\nU\nU",
        "{}",
        text(&log)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limited_signing_queues_at_once_and_flush_reports_the_reset() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world
        .devnet
        .set_switches(|s| {
            s.rate_limit = Some(RateLimit {
                points: 3,
                window_secs: 3600,
            })
        })
        .await;
    world.commit_ok(&repo, "one");
    world.commit_ok(&repo, "two (429)");
    let after_429 = world.devnet.xrpc_count(APPLY);
    let started = std::time::Instant::now();
    for index in 0..10 {
        world.commit_ok(&repo, &format!("more {index}"));
    }
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(
        world.devnet.xrpc_count(APPLY),
        after_429,
        "no write before the reset"
    );
    assert_eq!(world.queue_len(), 11);
    let flush = world.tt(&["sign", "--flush"]);
    assert_eq!(flush.status.code(), Some(1), "{}", text(&flush));
    let message = stderr(&flush);
    assert!(message.contains("11 signature record(s)"), "{message}");
    assert!(message.contains("rate limit"), "{message}");
    assert!(message.contains("retry after"), "{message}");
    // The hook form never fails.
    let hook = world.tt(&["sign", "--flush", "--hook"]);
    assert_eq!(hook.status.code(), Some(0));
    // With `fail`, signing fails instead of queueing.
    std::fs::write(
        world.home().join("config/tt/config.toml"),
        format!(
            "{}[sign]\noffline = \"fail\"\n",
            world
                .config_toml()
                .replace("[sign]\noffline = \"queue\"\n", "")
        ),
    )
    .expect("config");
    let failed = world.commit(&repo, "must be published");
    assert!(!failed.status.success(), "{}", text(&failed));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_session_fails_naming_shared_sessions() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    world.devnet.set_switches(|s| s.access_token_secs = 1).await;
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world
        .devnet
        .revoke_sessions("alice.test", None)
        .await
        .expect("revoke");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let output = world.commit(&repo, "after revocation");
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("tt sign --login"), "{message}");
    assert!(message.contains("shared between machines"), "{message}");
    assert_eq!(world.queue_len(), 0, "nothing is queued");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_forge_down_while_signing_queues_and_names_the_client_metadata() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    world.devnet.set_switches(|s| s.access_token_secs = 1).await;
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    tokio::time::sleep(Duration::from_secs(2)).await;
    world
        .devnet
        .set_switches(|s| s.fail_client_metadata = vec!["*".into()])
        .await;
    world.commit_ok(&repo, "forge down");
    assert_eq!(world.queue_len(), 1);
    let status = world.tt(&["status"]);
    let shown = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(shown.contains("client metadata"), "{shown}");
    assert!(shown.contains("/-/auth/tt/client-metadata.json"), "{shown}");
    world
        .devnet
        .set_switches(|s| {
            s.fail_client_metadata.clear();
            s.access_token_secs = 3600;
        })
        .await;
    world.close_windows();
    let flush = world.tt(&["sign", "--flush"]);
    assert!(flush.status.success(), "{}", text(&flush));
    assert_eq!(world.queue_len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_answer_is_reconciled_without_a_second_create() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world
        .devnet
        .set_switches(|s| s.drop_next_write = true)
        .await;
    world.commit_ok(&repo, "answer lost");
    assert_eq!(world.queue_len(), 1);
    world.close_windows();
    world.commit_ok(&repo, "online again");
    assert_eq!(world.queue_len(), 0);
    // Both verify, and the first was never created twice (a second create would have failed
    // the whole call with a 500 and left it queued).
    let log = world.git(&repo, &["log", "--format=%G?", "-2"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "U\nU",
        "{}",
        text(&log)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_accounts_and_explicit_dids() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let bob = world.did("bob.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    assert!(
        world
            .sign_login("bob.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    // Bob's login kept alice as the default signing DID.
    assert!(world.config_toml().contains(&alice));
    assert!(!world.config_toml().contains(&bob));
    let repo = world.repo("repo", Some(&bob));
    world.commit_ok(&repo, "as bob");
    let (uri, _) = signature_of(&world, &repo, "HEAD");
    assert!(uri.contains(&bob), "{uri}");

    // Setup writes --did, never config.toml.
    let config = world.config_toml();
    let setup = world.git_repo_tt(&repo, &["sign", "--setup", "--did", &bob]);
    assert!(setup.status.success(), "{}", text(&setup));
    assert_eq!(
        world.git_ok(&repo, &["config", "user.signingKey"]).trim(),
        bob
    );
    assert_eq!(world.config_toml(), config);

    // With no default and two sessions, setup refuses naming --did.
    std::fs::write(world.home().join("config/tt/config.toml"), "").expect("config");
    let other = world.repo("other", None);
    let refused = world.git_repo_tt(&other, &["sign", "--setup"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--did"), "{}", text(&refused));
    assert!(
        world
            .git(&other, &["config", "user.signingKey"])
            .stdout
            .is_empty()
    );
}

impl World {
    fn git_repo_tt(&self, repo: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(TT);
        self.env(&mut command);
        command
            .args(args)
            .current_dir(repo)
            .stdin(Stdio::null())
            .output()
            .expect("run tt")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_hooks_and_undo() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.home().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    world.git_ok(&repo, &["init", "-q", "-b", "main"]);
    // An existing hook that counts its runs and reads Git's ref list.
    let hooks = repo.join(".git/hooks");
    let counter = world.home().join("original-hook-runs");
    let original = format!(
        "#!/bin/sh\nread line\necho \"$1 $line\" >> '{}'\nexit 0\n",
        counter.display()
    );
    std::fs::write(hooks.join("pre-push"), &original).expect("hook");
    set_executable(&hooks.join("pre-push"));

    let setup = world.git_repo_tt(&repo, &["sign", "--setup", "--hook"]);
    assert!(setup.status.success(), "{}", text(&setup));
    let again = world.git_repo_tt(&repo, &["sign", "--setup", "--hook"]);
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(
        world.git_ok(&repo, &["config", "gpg.format"]).trim(),
        "x509"
    );
    assert_eq!(
        world.git_ok(&repo, &["config", "user.signingKey"]).trim(),
        alice
    );
    assert_eq!(
        world.git_ok(&repo, &["config", "commit.gpgSign"]).trim(),
        "true"
    );
    assert_eq!(
        world.git_ok(&repo, &["config", "tag.gpgSign"]).trim(),
        "true"
    );
    assert_eq!(
        world
            .git_ok(&repo, &["config", "--get-all", "tt.trustedSigner"])
            .trim(),
        alice
    );
    assert_eq!(
        std::fs::read_to_string(hooks.join("pre-push.tt-chained")).expect("chained"),
        original
    );
    assert!(
        std::fs::read_to_string(hooks.join("pre-push"))
            .expect("hook")
            .contains("# tt-sign-hook v1")
    );

    // Signed without -S.
    std::fs::write(repo.join("f"), "x").expect("write");
    world.git_ok(&repo, &["add", "f"]);
    world.git_ok(&repo, &["commit", "-q", "-m", "signed by setup"]);
    let verified = world.git(&repo, &["verify-commit", "HEAD"]);
    assert!(verified.status.success(), "{}", text(&verified));

    // A push runs tt's flush and the original hook once, with Git's arguments and input.
    let remote = world.home().join("remote.git");
    world.git_ok(
        world.home(),
        &["init", "-q", "--bare", remote.to_str().expect("utf8")],
    );
    world.git_ok(
        &repo,
        &["remote", "add", "origin", remote.to_str().expect("utf8")],
    );
    world.devnet.set_switches(|s| s.pds_offline = true).await;
    std::fs::write(repo.join("f"), "offline").expect("write");
    world.git_ok(&repo, &["commit", "-qam", "offline"]);
    assert_eq!(world.queue_len(), 1);
    let push = world.git(&repo, &["push", "-q", "origin", "main"]);
    assert!(push.status.success(), "{}", text(&push));
    assert!(stderr(&push).contains("remain queued"), "{}", text(&push));
    let runs = std::fs::read_to_string(&counter).expect("runs");
    assert_eq!(runs.lines().count(), 1, "{runs}");
    assert!(runs.starts_with("origin refs/heads/main "), "{runs}");

    // A chained hook that refuses still refuses the push.
    std::fs::write(hooks.join("pre-push.tt-chained"), "#!/bin/sh\nexit 1\n").expect("hook");
    set_executable(&hooks.join("pre-push.tt-chained"));
    std::fs::write(repo.join("f"), "again").expect("write");
    world.git_ok(&repo, &["commit", "-qam", "again"]);
    let refused = world.git(&repo, &["push", "-q", "origin", "main"]);
    assert!(!refused.status.success());
    std::fs::write(hooks.join("pre-push.tt-chained"), &original).expect("hook");

    // --global with --hook is refused and changes nothing.
    let global = world.git_repo_tt(&repo, &["sign", "--setup", "--global", "--hook"]);
    assert!(!global.status.success());
    assert!(stderr(&global).contains("per repository"));
    assert!(!world.home().join(".gitconfig").exists());

    // Undo: the original hook is back and a changed signing key is kept.
    world.git_ok(
        &repo,
        &[
            "config",
            "user.signingKey",
            "/home/alice/.ssh/id_ed25519.pub",
        ],
    );
    let undo = world.git_repo_tt(&repo, &["sign", "--setup", "--undo"]);
    assert!(undo.status.success(), "{}", text(&undo));
    assert_eq!(
        std::fs::read_to_string(hooks.join("pre-push")).expect("hook"),
        original
    );
    assert!(!hooks.join("pre-push.tt-chained").exists());
    assert!(
        world
            .git(&repo, &["config", "gpg.format"])
            .stdout
            .is_empty()
    );
    assert!(
        world
            .git(&repo, &["config", "gpg.x509.program"])
            .stdout
            .is_empty()
    );
    assert_eq!(
        world.git_ok(&repo, &["config", "user.signingKey"]).trim(),
        "/home/alice/.ssh/id_ed25519.pub"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_or_tracked_hook_directories_need_the_flag() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let repo = world.repo("repo", Some(&alice));
    let shared = world.home().join("shared-hooks");
    std::fs::create_dir_all(&shared).expect("mkdir");
    world.git_ok(
        &repo,
        &["config", "core.hooksPath", shared.to_str().expect("utf8")],
    );
    let refused = world.git_repo_tt(&repo, &["sign", "--setup", "--verify-only", "--hook"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--hooks-path-ok"),
        "{}",
        text(&refused)
    );
    assert!(!shared.join("pre-push").exists());
    let accepted = world.git_repo_tt(
        &repo,
        &[
            "sign",
            "--setup",
            "--verify-only",
            "--hook",
            "--hooks-path-ok",
        ],
    );
    assert!(accepted.status.success(), "{}", text(&accepted));
    assert!(shared.join("pre-push").exists());

    // A tracked .husky directory.
    let husky = world.repo("husky", Some(&alice));
    std::fs::create_dir_all(husky.join(".husky")).expect("mkdir");
    std::fs::write(husky.join(".husky/pre-commit"), "#!/bin/sh\n").expect("write");
    world.git_ok(&husky, &["add", ".husky"]);
    world.git_ok(&husky, &["config", "core.hooksPath", ".husky"]);
    let refused = world.git_repo_tt(&husky, &["sign", "--setup", "--verify-only", "--hook"]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("--hooks-path-ok"),
        "{}",
        text(&refused)
    );

    // A linked worktree whose hooksPath is the main repository's own hooks: accepted.
    let main = world.repo("main", Some(&alice));
    std::fs::write(main.join("f"), "x").expect("write");
    world.git_ok(&main, &["add", "f"]);
    world.git_ok(
        &main,
        &["-c", "commit.gpgSign=false", "commit", "-qm", "base"],
    );
    let linked = world.home().join("linked");
    world.git_ok(
        &main,
        &["worktree", "add", "-q", linked.to_str().expect("utf8")],
    );
    let hooks = main.join(".git/hooks");
    world.git_ok(
        &linked,
        &["config", "core.hooksPath", hooks.to_str().expect("utf8")],
    );
    let accepted = world.git_repo_tt(&linked, &["sign", "--setup", "--verify-only", "--hook"]);
    assert!(accepted.status.success(), "{}", text(&accepted));
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_only_leaves_ssh_signing_alone() {
    let world = World::start().await;
    let repo = world.home().join("reviewer");
    std::fs::create_dir_all(&repo).expect("mkdir");
    world.git_ok(&repo, &["init", "-q"]);
    world.git_ok(world.home(), &["config", "--global", "gpg.format", "ssh"]);
    world.git_ok(
        world.home(),
        &["config", "--global", "commit.gpgSign", "true"],
    );
    let output = world.git_repo_tt(&repo, &["sign", "--setup", "--verify-only", "--global"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        world
            .git_ok(world.home(), &["config", "--global", "gpg.format"])
            .trim(),
        "ssh"
    );
    assert_eq!(
        world
            .git_ok(world.home(), &["config", "--global", "commit.gpgSign"])
            .trim(),
        "true"
    );
    assert!(
        world
            .git(world.home(), &["config", "--global", "user.signingKey"])
            .stdout
            .is_empty()
    );
    assert!(
        !world
            .git_ok(world.home(), &["config", "--global", "gpg.x509.program"])
            .trim()
            .is_empty()
    );
    assert!(
        world.devnet.par_requests().await.is_empty(),
        "no login started"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rebase_signs_only_what_it_creates() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.git_ok(&repo, &["config", "commit.gpgSign", "true"]);
    for index in 0..51 {
        std::fs::write(repo.join("f"), format!("{index}")).expect("write");
        world.git_ok(&repo, &["add", "f"]);
        world.git_ok(&repo, &["commit", "-qm", &format!("c{index}")]);
    }
    let before = world.devnet.xrpc_count(APPLY);
    world.git_ok(&repo, &["rebase", "-q", "HEAD~50"]);
    assert_eq!(
        world.devnet.xrpc_count(APPLY),
        before,
        "a fast-forward rebase signs nothing"
    );
    let started = std::time::Instant::now();
    world.git_ok(
        &repo,
        &[
            "rebase",
            "-q",
            "--force-rebase",
            "--exec",
            "true",
            "HEAD~50",
        ],
    );
    assert_eq!(world.devnet.xrpc_count(APPLY), before + 50);
    assert!(started.elapsed() < Duration::from_secs(120));
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_signing_refreshes_once() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    world.devnet.set_switches(|s| s.access_token_secs = 1).await;
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    world
        .devnet
        .set_switches(|s| s.access_token_secs = 3600)
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let first = world.repo("one", Some(&alice));
    let second = world.repo("two", Some(&alice));
    std::fs::write(first.join("f"), "1").expect("write");
    std::fs::write(second.join("f"), "2").expect("write");
    world.git_ok(&first, &["add", "f"]);
    world.git_ok(&second, &["add", "f"]);
    let spawn = |repo: &Path| {
        let mut command = Command::new("git");
        world.env(&mut command);
        command
            .args(["commit", "-q", "-S", "-m", "parallel"])
            .current_dir(repo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("git")
    };
    let (a, b) = (spawn(&first), spawn(&second));
    let (a, b) = (
        a.wait_with_output().expect("a"),
        b.wait_with_output().expect("b"),
    );
    assert!(a.status.success(), "{}", text(&a));
    assert!(b.status.success(), "{}", text(&b));
    // A reused refresh token would have revoked the session: a third signing still works.
    world.commit_ok(&first, "after");
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_resolve_finds_an_owner_did() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let output = world.tt(&["identity", "resolve", "alice.test"]);
    assert!(output.status.success(), "{}", text(&output));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(printed.contains(&format!("DID:    {alice}")), "{printed}");
    assert!(printed.contains("@alice.test (verified)"), "{printed}");
    assert!(printed.contains(world.devnet.url()), "{printed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signing_login_for_another_forge_changes_no_default() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let bob = world.did("bob.test").await;
    let forge = world.forge.clone();
    std::fs::create_dir_all(world.home().join("config/tt")).expect("mkdir");
    std::fs::write(
        world.home().join("config/tt/config.toml"),
        format!("default_forge = \"{forge}\"\n"),
    )
    .expect("config");
    assert!(world.sign_login("alice.test", &[]).await.status.success());
    // A second forge serving its own client metadata.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let other = format!("http://{}", listener.local_addr().expect("address"));
    let document = tt_metadata(&other);
    let router = axum::Router::new().route(
        "/-/auth/tt/client-metadata.json",
        get(move || async move { Json(document) }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let output = world.sign_login("bob.test", &["--forge", &other]).await;
    assert!(output.status.success(), "{}", text(&output));
    let config = world.config_toml();
    assert!(
        config.contains(&format!("default_forge = \"{forge}\"")),
        "{config}"
    );
    assert!(
        config.contains(&format!("default_signing_did = \"{alice}\"")),
        "{config}"
    );
    let status = String::from_utf8_lossy(&world.tt(&["status"]).stdout).into_owned();
    assert!(status.contains(&bob), "{status}");
    assert!(
        status.contains(&format!("{other}/-/auth/tt/client-metadata.json")),
        "{status}"
    );
    // --set-default makes the new DID the default signing DID, never the forge.
    let output = world
        .sign_login("bob.test", &["--forge", &other, "--set-default"])
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let config = world.config_toml();
    assert!(
        config.contains(&format!("default_signing_did = \"{bob}\"")),
        "{config}"
    );
    assert!(
        config.contains(&format!("default_forge = \"{forge}\"")),
        "{config}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ended_public_client_session_fails_naming_tt_sign_login() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    world
        .devnet
        .set_switches(|s| {
            s.access_token_secs = 1;
            s.public_session_secs = 1;
        })
        .await;
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let repo = world.repo("repo", Some(&alice));
    let output = world.commit(&repo, "after the session ended");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("tt sign --login"),
        "{}",
        text(&output)
    );
    assert_eq!(world.queue_len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_https_client_suggests_the_localhost_client() {
    let world = World::start().await;
    let forge = world.forge.clone();
    for error in ["invalid_client_metadata", "invalid_redirect_uri"] {
        world
            .devnet
            .set_switches(|s| s.par_error = Some(error.into()))
            .await;
        let output = world.sign_login("alice.test", &["--forge", &forge]).await;
        assert!(!output.status.success());
        let message = stderr(&output);
        assert!(message.contains(error), "{message}");
        assert!(message.contains("--client localhost"), "{message}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_client_metadata_url_is_named_before_any_request() {
    let world = World::start().await;
    let closed = format!(
        "http://127.0.0.1:{}",
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("address")
            .port()
    );
    let output = world.tt(&[
        "sign",
        "--login",
        "--handle",
        "alice.test",
        "--no-open",
        "--forge",
        &closed,
    ]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains(&format!(
            "{closed}/-/auth/tt/client-metadata.json could not be loaded"
        )),
        "{message}"
    );
    assert!(world.devnet.par_requests().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_replaced_while_queued_is_dropped_and_reported() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.devnet.set_switches(|s| s.pds_offline = true).await;
    world.commit_ok(&repo, "queued");
    world.devnet.set_switches(|s| s.pds_offline = false).await;
    let (uri, _) = signature_of(&world, &repo, "HEAD");
    let rkey = uri.rsplit('/').next().expect("rkey").to_owned();
    // Someone else writes another record under the same key.
    let salt = tartan_atproto::signature::record::Salt::from_bytes([7; 32]);
    world
        .devnet
        .write_signature("alice.test", b"something else", &salt, None, Some(&rkey))
        .await
        .expect("write");
    world.close_windows();
    let flush = world.tt(&["sign", "--flush"]);
    assert!(flush.status.success(), "{}", text(&flush));
    assert!(stderr(&flush).contains("replaced"), "{}", text(&flush));
    assert_eq!(world.queue_len(), 0);
    let status = String::from_utf8_lossy(&world.tt(&["status"]).stdout).into_owned();
    assert!(
        status.contains(&rkey) && status.contains("cannot verify"),
        "{status}"
    );
    let verify = world.git(&repo, &["verify-commit", "--raw", "HEAD"]);
    assert!(
        stderr(&verify).contains("[GNUPG:] BADSIG"),
        "{}",
        text(&verify)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn verification_failures_are_badsig_or_errsig() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.commit_ok(&repo, "one");
    world.commit_ok(&repo, "two");

    // An unreachable PDS: ERRSIG.
    world.devnet.set_switches(|s| s.pds_offline = true).await;
    let output = world.git(&repo, &["verify-commit", "--raw", "HEAD~1"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("[GNUPG:] ERRSIG"),
        "{}",
        text(&output)
    );
    world.devnet.set_switches(|s| s.pds_offline = false).await;
    let _ = std::fs::remove_file(world.home().join("cache/tt/hosts.json"));

    // Verified once, then the record replaced: BADSIG without the cache.
    let output = world.git(&repo, &["verify-commit", "HEAD"]);
    assert!(output.status.success(), "{}", text(&output));
    let (uri, _) = signature_of(&world, &repo, "HEAD");
    let rkey = uri.rsplit('/').next().expect("rkey").to_owned();
    let salt = tartan_atproto::signature::record::Salt::from_bytes([9; 32]);
    let other = tartan_atproto::signature::record::SignatureRecord::for_git(
        b"other",
        &salt,
        "2026-11-09T12:00:00Z".into(),
    );
    world
        .devnet
        .replace_record(
            "alice.test",
            "academy.rawkode.tartan.signature",
            &rkey,
            other.to_json(),
            None,
        )
        .await
        .expect("replace");
    let mut command = Command::new("git");
    world.env(&mut command);
    let output = command
        .env("TT_NO_CACHE", "1")
        .args(["verify-commit", "--raw", "HEAD"])
        .current_dir(&repo)
        .output()
        .expect("git");
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(&format!("[GNUPG:] BADSIG {alice}")),
        "{}",
        text(&output)
    );

    // The CID line re-spelt in another multibase: BADSIG, also after the canonical form was
    // cached.
    let raw = world.git_ok(&repo, &["cat-file", "commit", "HEAD~1"]);
    let verified = world.git(&repo, &["verify-commit", "HEAD~1"]);
    assert!(verified.status.success(), "{}", text(&verified));
    let cid_line = raw
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("CID: "))
        .expect("a CID line")
        .to_owned();
    let cid: cid::Cid = cid_line.parse().expect("cid");
    let upper = cid
        .to_string_of_base(cid::multibase::Base::Base32Upper)
        .expect("base32upper");
    let respelt = write_object(&world, &repo, "commit", &raw.replace(&cid_line, &upper));
    let output = world.git(&repo, &["verify-commit", "--raw", &respelt]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("[GNUPG:] BADSIG"),
        "{}",
        text(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_queued_signature_needs_no_authorization() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    let forge = world.forge.clone();
    assert!(
        world
            .sign_login("alice.test", &["--forge", &forge])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.devnet.set_switches(|s| s.pds_offline = true).await;
    world.commit_ok(&repo, "queued");
    assert_eq!(world.queue_len(), 1);
    let pars = world.devnet.par_requests().await.len();
    let output = world.git_repo_tt(&repo, &["sign", "revoke", "HEAD"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(world.queue_len(), 0);
    assert_eq!(world.devnet.par_requests().await.len(), pars);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_with_the_localhost_client_deletes_the_record() {
    let world = World::start().await;
    let alice = world.did("alice.test").await;
    assert!(
        world
            .sign_login("alice.test", &["--client", "localhost"])
            .await
            .status
            .success()
    );
    let repo = world.repo("repo", Some(&alice));
    world.commit_ok(&repo, "to withdraw");
    let (uri, _) = signature_of(&world, &repo, "HEAD");
    let rkey = uri.rsplit('/').next().expect("rkey").to_owned();
    assert!(world.record(&alice, &rkey).await.is_some());
    let output = world
        .run_login_in(
            &repo,
            &[
                "sign",
                "revoke",
                "HEAD",
                "--client",
                "localhost",
                "--no-open",
            ],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        world.record(&alice, &rkey).await.is_none(),
        "the record is deleted"
    );
    let pars = world.devnet.par_requests().await;
    let scope = pars
        .last()
        .and_then(|par| par.iter().find(|(key, _)| key == "scope"))
        .map(|(_, value)| value.clone());
    assert_eq!(
        scope.as_deref(),
        Some("atproto repo:academy.rawkode.tartan.signature?action=delete")
    );
    // The deleted record no longer verifies the commit.
    let verify = world.git(&repo, &["verify-commit", "HEAD"]);
    assert!(!verify.status.success());
}

impl World {
    /// [`World::run_login`] in `dir`.
    async fn run_login_in(&self, dir: &Path, args: &[&str]) -> Output {
        let mut std_command = Command::new(TT);
        self.env(&mut std_command);
        let mut command = tokio::process::Command::new(TT);
        command
            .env_clear()
            .envs(
                std_command.get_envs().filter_map(|(key, value)| {
                    value.map(|value| (key.to_owned(), value.to_owned()))
                }),
            )
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn tt");
        let stderr = child.stderr.take().expect("stderr");
        let mut lines = BufReader::new(stderr).lines();
        let mut seen = String::new();
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                seen.push_str(&line);
                seen.push('\n');
                let trimmed = line.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    return Some(trimmed.to_owned());
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        if let Some(url) = url {
            follow(&self.http, &url).await;
        }
        let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .expect("tt finishes")
            .expect("tt output");
        let mut rest = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            rest.push_str(&line);
            rest.push('\n');
        }
        Output {
            status: output.status,
            stdout: output.stdout,
            stderr: format!("{seen}{rest}").into_bytes(),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unmarked_hook_beside_a_chained_one_is_refused() {
    let world = World::start().await;
    let repo = world.repo("repo", None);
    let hooks = repo.join(".git/hooks");
    std::fs::write(hooks.join("pre-push"), "#!/bin/sh\nexit 0\n").expect("hook");
    std::fs::write(hooks.join("pre-push.tt-chained"), "#!/bin/sh\nexit 0\n").expect("hook");
    let output = world.git_repo_tt(&repo, &["sign", "--setup", "--verify-only", "--hook"]);
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(hooks.join("pre-push")).expect("hook"),
        "#!/bin/sh\nexit 0\n"
    );
    // The program is the absolute path: the tt on PATH (none here) is not this binary.
    world.git_ok(&repo, &["config", "--unset", "gpg.x509.program"]);
    let output = world.git_repo_tt(&repo, &["sign", "--setup", "--verify-only"]);
    assert!(output.status.success(), "{}", text(&output));
    let program = world.git_ok(&repo, &["config", "gpg.x509.program"]);
    assert!(Path::new(program.trim()).is_absolute(), "{program}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_whose_tt_is_gone_warns_and_still_runs_the_chained_hook() {
    let world = World::start().await;
    let repo = world.repo("repo", None);
    let hooks = repo.join(".git/hooks");
    let marker = world.home().join("chained-ran");
    std::fs::write(
        hooks.join("pre-push"),
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .expect("hook");
    set_executable(&hooks.join("pre-push"));
    let output = world.git_repo_tt(
        &repo,
        &[
            "sign",
            "--setup",
            "--verify-only",
            "--hook",
            "--program",
            "/nonexistent/tt",
        ],
    );
    assert!(output.status.success(), "{}", text(&output));
    std::fs::write(repo.join("f"), "x").expect("write");
    world.git_ok(&repo, &["add", "f"]);
    world.git_ok(&repo, &["commit", "-qm", "unsigned"]);
    let remote = world.home().join("remote.git");
    world.git_ok(
        world.home(),
        &["init", "-q", "--bare", remote.to_str().expect("utf8")],
    );
    let push = world.git(
        &repo,
        &["push", "-q", remote.to_str().expect("utf8"), "main"],
    );
    assert!(push.status.success(), "{}", text(&push));
    assert!(stderr(&push).contains("was not found"), "{}", text(&push));
    assert!(marker.exists(), "the chained hook ran");
}
