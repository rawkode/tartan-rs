//! `tt` against a running forge and the development network (design D16, spec tt-cli): the
//! loopback login hand-off and its first-run signing login, defaults, access tokens, token
//! logins, logout and the operator commands.
//!
//! Runs the real `tt` binary, built with `--features dev-identity` (set `TT_BIN` to use one
//! already built), with `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` under
//! the test's temporary directory and `TT_CREDENTIAL_STORE=file`. Needs a Rivet engine binary:
//! set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tartan_config::deployment::Readers;
use tokio::io::{AsyncBufReadExt as _, BufReader};

use common::{Forge, browser, sign_in};

/// The `tt` binary: `TT_BIN`, or a `dev-identity` build made here.
fn tt_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("TT_BIN") {
        return PathBuf::from(path);
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("the workspace");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .args(["build", "--quiet", "-p", "tt", "--features", "dev-identity"])
        .current_dir(workspace)
        .status()
        .expect("cargo build tt");
    assert!(status.success(), "building tt failed");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    target.join("debug").join("tt")
}

/// One person's machine: its own home and `tt` state.
struct Machine {
    tt: PathBuf,
    home: tempfile::TempDir,
    devnet_url: String,
    rivet: Vec<(String, String)>,
}

impl Machine {
    fn new(tt: &Path, forge: &Forge) -> Self {
        Machine {
            tt: tt.to_owned(),
            home: tempfile::tempdir().expect("temp"),
            devnet_url: forge.identity.devnet.url().to_owned(),
            rivet: vec![
                ("RIVET_ENDPOINT".into(), "http://127.0.0.1:6420".into()),
                ("RIVET_TOKEN".into(), "default".into()),
                ("RIVET_NAMESPACE".into(), "default".into()),
                ("RIVET_POOL".into(), "rivetkit-test-0".into()),
                ("TARTAN_FORGE_ID".into(), forge.forge_id.clone()),
            ],
        }
    }

    fn envs(&self) -> Vec<(String, String)> {
        let home = self.home.path();
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        let mut envs = vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".into(), path(home.to_path_buf())),
            ("XDG_CONFIG_HOME".into(), path(home.join("config"))),
            ("XDG_STATE_HOME".into(), path(home.join("state"))),
            ("XDG_CACHE_HOME".into(), path(home.join("cache"))),
            ("TT_CREDENTIAL_STORE".into(), "file".into()),
            ("TARTAN_DEV_IDENTITY".into(), "1".into()),
            ("TARTAN_PLC_URL".into(), self.devnet_url.clone()),
            ("TARTAN_DEV_HANDLE_RESOLVER".into(), self.devnet_url.clone()),
        ];
        envs.extend(self.rivet.iter().cloned());
        envs
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut command = Command::new(&self.tt);
        command
            .env_clear()
            .envs(self.envs())
            .args(args)
            .current_dir(self.home.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().expect("run tt");
        if let Some(input) = stdin {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write stdin");
        }
        child.wait_with_output().expect("tt output")
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.home.path().join("config/tt/config.toml")).unwrap_or_default()
    }

    fn status(&self) -> String {
        let output = self.run(&["status"], None);
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Runs a login command and answers each URL it prints with `answer`, in order.
    async fn login(&self, args: &[&str], answers: Vec<Answer<'_>>) -> Output {
        let mut child = tokio::process::Command::new(&self.tt)
            .env_clear()
            .envs(self.envs())
            .args(args)
            .current_dir(self.home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tt");
        let stderr = child.stderr.take().expect("stderr");
        let mut lines = BufReader::new(stderr).lines();
        let mut seen = String::new();
        for answer in answers {
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
            let Some(url) = url else {
                break;
            };
            answer.answer(&url).await;
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
}

/// How a test answers a URL `tt` printed.
enum Answer<'a> {
    /// The forge's CLI approval page: approve (or cancel) as the browser session.
    Forge {
        base: &'a str,
        session: &'a str,
        approve: bool,
    },
    /// The authorization server's page: follow it with a plain HTTP client, adding `extra`.
    Follow { extra: &'a str },
}

impl Answer<'_> {
    async fn answer(&self, url: &str) {
        match self {
            Answer::Forge {
                base,
                session,
                approve,
            } => {
                let query = url
                    .split_once("/#/-/cli-login?")
                    .expect("an approval URL")
                    .1;
                let pairs: std::collections::HashMap<String, String> =
                    reqwest::Url::parse(&format!("http://x/?{query}"))
                        .expect("query")
                        .query_pairs()
                        .into_owned()
                        .collect();
                let port: u16 = pairs["port"].parse().expect("port");
                let redirect = if *approve {
                    let response = browser()
                        .post(format!("{base}/-/auth/cli/approve"))
                        .header("cookie", format!("tartan_session={session}"))
                        .header("origin", *base)
                        .json(&json!({
                            "port": port,
                            "state": pairs["state"],
                            "challenge": pairs["challenge"],
                            "device": pairs["device"],
                        }))
                        .send()
                        .await
                        .expect("approve");
                    assert_eq!(response.status(), 200);
                    let body: Value = response.json().await.expect("json");
                    body["redirect"].as_str().expect("redirect").to_owned()
                } else {
                    format!(
                        "http://127.0.0.1:{port}/forge?error=access_denied&state={}",
                        pairs["state"]
                    )
                };
                let _ = browser().get(redirect).send().await;
            }
            Answer::Follow { extra } => {
                let mut next = format!("{url}{extra}");
                for _ in 0..5 {
                    let Ok(response) = browser().get(&next).send().await else {
                        return;
                    };
                    let Some(location) = response
                        .headers()
                        .get("location")
                        .and_then(|value| value.to_str().ok())
                    else {
                        return;
                    };
                    next = location.to_owned();
                }
            }
        }
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

#[tokio::test(flavor = "multi_thread")]
async fn tt_against_a_forge() {
    let tt = tt_binary();
    let forge = Forge::start("tt").await;
    let base = forge.base.clone();
    let devnet = forge.identity.devnet.clone();
    let alice = forge.identity.owner.clone();
    let session = sign_in(&base, "alice.test", "/", None).await.session();

    // Login with no forge: refused before any request.
    let fresh = Machine::new(&tt, &forge);
    let output = fresh.run(&["login", "--no-open"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--forge"));

    // Approve in the browser (no signing).
    let laptop = Machine::new(&tt, &forge);
    let pars = devnet.par_requests().await.len();
    let output = laptop
        .login(
            &["login", "--forge", &base, "--no-open", "--no-signing"],
            vec![Answer::Forge {
                base: &base,
                session: &session,
                approve: true,
            }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let status = laptop.status();
    assert!(status.contains("@alice.test"), "{status}");
    assert!(status.contains(&alice), "{status}");
    assert!(
        laptop
            .config()
            .contains(&format!("default_forge = \"{base}\""))
    );
    assert_eq!(devnet.par_requests().await.len(), pars, "no signing login");

    // Tokens from the CLI session.
    let created = laptop.run(
        &[
            "token",
            "create",
            "--name",
            "ci",
            "--scopes",
            "read",
            "--expires-in-days",
            "30",
        ],
        None,
    );
    assert!(created.status.success(), "{}", text(&created));
    let printed = String::from_utf8_lossy(&created.stdout).into_owned();
    let secret = printed
        .lines()
        .find(|line| line.starts_with("tartan_pat_"))
        .expect("the secret is printed once")
        .to_owned();
    let id = printed
        .split_whitespace()
        .nth(2)
        .expect("the token id")
        .to_owned();
    let listed = laptop.run(&["token", "list"], None);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("ci"),
        "{}",
        text(&listed)
    );

    // Remote machine: a token login never signs in for signing, and cannot manage tokens.
    let ci = Machine::new(&tt, &forge);
    let pars = devnet.par_requests().await.len();
    let output = ci.run(
        &["login", "--with-token", "--forge", &base],
        Some(&format!("{secret}\n")),
    );
    assert!(output.status.success(), "{}", text(&output));
    assert!(ci.status().contains(&alice));
    let refused = ci.run(
        &[
            "token",
            "create",
            "--name",
            "x",
            "--scopes",
            "read",
            "--expires-in-days",
            "1",
        ],
        None,
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("session is needed"));
    assert_eq!(devnet.par_requests().await.len(), pars);

    // Revoke.
    let revoked = laptop.run(&["token", "revoke", &id], None);
    assert!(revoked.status.success(), "{}", text(&revoked));
    let listed = laptop.run(&["token", "list"], None);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("revoked"),
        "{}",
        text(&listed)
    );

    // A second forge does not replace the default; --set-default does.
    let other = forge.gateway(Readers::default(), Vec::new()).await;
    let session_other = sign_in(&other, "alice.test", "/", None).await.session();
    let output = laptop
        .login(
            &["login", "--forge", &other, "--no-open", "--no-signing"],
            vec![Answer::Forge {
                base: &other,
                session: &session_other,
                approve: true,
            }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("--set-default"));
    assert!(
        laptop
            .config()
            .contains(&format!("default_forge = \"{base}\""))
    );
    let output = laptop
        .login(
            &[
                "login",
                "--forge",
                &other,
                "--no-open",
                "--no-signing",
                "--set-default",
            ],
            vec![Answer::Forge {
                base: &other,
                session: &session_other,
                approve: true,
            }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        laptop
            .config()
            .contains(&format!("default_forge = \"{other}\""))
    );

    // First run on a forwarded port: the forge approval, then the signing consent, both on
    // the same fixed port.
    let first = Machine::new(&tt, &forge);
    let port = free_port();
    let port_text = port.to_string();
    let output = first
        .login(
            &["login", "--forge", &base, "--no-open", "--port", &port_text],
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
    let status = first.status();
    assert!(status.contains("Signing: "), "{status}");
    assert!(
        status.contains(&format!("{base}/-/auth/tt/client-metadata.json")),
        "{status}"
    );
    assert!(
        first
            .config()
            .contains(&format!("default_signing_did = \"{alice}\""))
    );
    let par = devnet.par_requests().await;
    let last = par.last().expect("a PAR");
    let field = |name: &str| {
        last.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(field("login_hint").as_deref(), Some(alice.as_str()));
    assert_eq!(
        field("redirect_uri"),
        Some(format!("http://127.0.0.1:{port}/callback"))
    );

    // Signing session already present: only the forge session.
    let pars = devnet.par_requests().await.len();
    let output = first
        .login(
            &["login", "--forge", &base, "--no-open"],
            vec![Answer::Forge {
                base: &base,
                session: &session,
                approve: true,
            }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(devnet.par_requests().await.len(), pars);

    // Signing consent cancelled: the CLI session is kept, tt names `tt sign --login`.
    let cancelled = Machine::new(&tt, &forge);
    let output = cancelled
        .login(
            &["login", "--forge", &base, "--no-open"],
            vec![
                Answer::Forge {
                    base: &base,
                    session: &session,
                    approve: true,
                },
                Answer::Follow { extra: "&deny=1" },
            ],
        )
        .await;
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tt sign --login"),
        "{}",
        text(&output)
    );
    assert!(cancelled.status().contains("@alice.test"));

    // Ended from the browser: the next tt request is refused and tt says to log in.
    let sessions: Value = browser()
        .get(format!("{base}/-/api/sessions"))
        .header("cookie", format!("tartan_session={session}"))
        .send()
        .await
        .expect("sessions")
        .json()
        .await
        .expect("json");
    let cli_ids: Vec<String> = sessions
        .as_array()
        .expect("list")
        .iter()
        .filter(|entry| entry["kind"] == "cli")
        .map(|entry| entry["id"].as_str().expect("id").to_owned())
        .collect();
    assert!(!cli_ids.is_empty(), "{sessions}");
    for id in &cli_ids {
        let ended = browser()
            .delete(format!("{base}/-/api/sessions/{id}"))
            .header("cookie", format!("tartan_session={session}"))
            .header("origin", &base)
            .send()
            .await
            .expect("end");
        assert_eq!(ended.status(), 204);
    }
    let refused = cancelled.run(&["token", "list"], None);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("tt login"),
        "{}",
        text(&refused)
    );

    // Operator commands: an owner token through Rivet that the gateway accepts, and a refusal
    // for a DID that is not an owner.
    let operator = Machine::new(&tt, &forge);
    let created = operator.run(
        &[
            "admin",
            "token",
            "create",
            "--did",
            &alice,
            "--name",
            "seed",
            "--scopes",
            "read,write,admin",
            "--expires-in-days",
            "1",
        ],
        None,
    );
    assert!(created.status.success(), "{}", text(&created));
    let secret = String::from_utf8_lossy(&created.stdout)
        .lines()
        .find(|line| line.starts_with("tartan_pat_"))
        .expect("secret")
        .to_owned();
    let me: Value = browser()
        .get(format!("{base}/-/api/me"))
        .bearer_auth(&secret)
        .send()
        .await
        .expect("me")
        .json()
        .await
        .expect("json");
    assert_eq!(me["did"], alice.as_str());
    let bob = devnet.did("bob.test").await.expect("bob");
    let refused = operator.run(
        &[
            "admin",
            "token",
            "create",
            "--did",
            &bob,
            "--name",
            "x",
            "--scopes",
            "read",
            "--expires-in-days",
            "1",
        ],
        None,
    );
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        message.contains(&bob) && message.contains("not one of this forge's owners"),
        "{message}"
    );

    // Logout while the forge is down: the local credentials are deleted anyway.
    let down = Machine::new(&tt, &forge);
    let output = down
        .login(
            &["login", "--forge", &base, "--no-open", "--no-signing"],
            vec![Answer::Forge {
                base: &base,
                session: &session,
                approve: true,
            }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let credentials = down.home.path().join("state/tt/credentials");
    assert_eq!(std::fs::read_dir(&credentials).expect("dir").count(), 1);
    forge.stop().await;
    // The gateway keeps serving after the actors stop, so point logout at a closed port with
    // the same credential instead.
    let closed = format!("http://127.0.0.1:{}", free_port());
    let index = down.home.path().join("state/tt/sessions.json");
    let text_index = std::fs::read_to_string(&index).expect("index");
    std::fs::write(&index, text_index.replace(&base, &closed)).expect("index");
    let stored = std::fs::read_dir(&credentials)
        .expect("dir")
        .next()
        .expect("a file")
        .expect("entry")
        .path();
    let renamed = credentials.join(format!("{}.json", base32(&closed)));
    std::fs::rename(&stored, &renamed).expect("rename");
    let output = down.run(&["logout", "--forge", &closed], None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not be ended"),
        "{}",
        text(&output)
    );
    assert_eq!(std::fs::read_dir(&credentials).expect("dir").count(), 0);
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("address")
        .port()
}

/// `tt`'s file name for a credential: the name in unpadded lowercase base32.
fn base32(name: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in name.bytes() {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}
