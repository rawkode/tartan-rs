//! `tt` on a test machine: the binary, a home of its own and answers to the URLs it prints.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use super::{Forge, browser};

/// The `tt` binary: `TT_BIN`, or a `dev-identity` build made here.
pub fn tt_binary() -> PathBuf {
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
pub struct Machine {
    pub tt: PathBuf,
    pub home: tempfile::TempDir,
    devnet_url: String,
    rivet: Vec<(String, String)>,
}

impl Machine {
    pub fn new(tt: &Path, forge: &Forge) -> Self {
        Self::on_network(tt, forge.identity.devnet.url(), &forge.forge_id)
    }

    /// A machine whose `tt` uses the devnet at `devnet_url` and, for `tt admin`, the test
    /// engine's forge `forge_id`.
    pub fn on_network(tt: &Path, devnet_url: &str, forge_id: &str) -> Self {
        Machine {
            tt: tt.to_owned(),
            home: tempfile::tempdir().expect("temp"),
            devnet_url: devnet_url.to_owned(),
            rivet: vec![
                ("RIVET_ENDPOINT".into(), "http://127.0.0.1:6420".into()),
                ("RIVET_TOKEN".into(), "default".into()),
                ("RIVET_NAMESPACE".into(), "default".into()),
                ("RIVET_POOL".into(), "rivetkit-test-0".into()),
                ("TARTAN_FORGE_ID".into(), forge_id.to_owned()),
            ],
        }
    }

    pub fn envs(&self) -> Vec<(String, String)> {
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

    pub fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
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

    /// Makes a commit signed by this machine's default signing DID and answers its record URI.
    pub fn signed_commit(&self, message: &str) -> String {
        let repo = self.home.path().join("repo");
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .env_clear()
                .envs(self.envs())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Alice")
                .env("GIT_AUTHOR_EMAIL", "alice@example.com")
                .env("GIT_COMMITTER_NAME", "Alice")
                .env("GIT_COMMITTER_EMAIL", "alice@example.com")
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("git");
            assert!(output.status.success(), "git {args:?}: {}", text(&output));
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        if !repo.exists() {
            std::fs::create_dir_all(&repo).expect("mkdir");
            git(&["init", "-q"]);
            git(&["config", "gpg.format", "x509"]);
            git(&[
                "config",
                "gpg.x509.program",
                self.tt.to_str().expect("utf8"),
            ]);
        }
        std::fs::write(repo.join("f"), message).expect("write");
        git(&["add", "f"]);
        git(&["commit", "-q", "-S", "-m", message]);
        git(&["cat-file", "commit", "HEAD"])
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("Record: "))
            .expect("a Tartan signature")
            .to_owned()
    }

    pub fn config(&self) -> String {
        std::fs::read_to_string(self.home.path().join("config/tt/config.toml")).unwrap_or_default()
    }

    pub fn status(&self) -> String {
        let output = self.run(&["status"], None);
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Runs a login command and answers each URL it prints with `answer`, in order.
    pub async fn login(&self, args: &[&str], answers: Vec<Answer<'_>>) -> Output {
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
pub enum Answer<'a> {
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

pub fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
