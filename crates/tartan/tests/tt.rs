//! `tt` against a running forge and the development network (design D16, spec tt-cli): the
//! loopback login hand-off and its first-run signing login, defaults, access tokens, token
//! logins, logout and the operator commands.
//!
//! Runs the real `tt` binary, built with `--features dev-identity` (set `TT_BIN` to use one
//! already built), with `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` under
//! the test's temporary directory and `TT_CREDENTIAL_STORE=file`. Needs a Rivet engine binary:
//! set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use serde_json::{Value, json};
use tartan_config::deployment::Readers;

use tartan_forge::identity_handle;
use tartan_identity_api::SignatureRecordQuery;

use common::tt::{Answer, Machine, text, tt_binary};
use common::{Forge, browser, sign_in};

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

    // Revoking a signature: signed in with a CLI session as the record's DID, it is deleted
    // at the PDS and withdrawn on the forge; with only an access token, the forge re-proves it
    // and tt says withdrawing needs `tt login`.
    let uri = first.signed_commit("withdrawn");
    let output = first
        .login(
            &["sign", "revoke", &uri, "--no-open"],
            vec![Answer::Follow { extra: "" }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Withdrew the signature"),
        "{}",
        text(&output)
    );
    let rkey = uri.rsplit('/').next().expect("rkey").to_owned();
    let row = identity_handle(&forge.client)
        .expect("identity")
        .send(SignatureRecordQuery {
            did: alice.clone(),
            rkey: rkey.clone(),
        })
        .await
        .expect("query")
        .expect("a tracked row");
    assert!(row.revoked_at.is_some());
    assert_eq!(
        serde_json::to_value(row.revoked_reason).expect("json"),
        json!("withdrawn")
    );
    // The token `ci` used was revoked above: sign it in with a fresh one.
    let created = first.run(
        &[
            "token",
            "create",
            "--name",
            "ci2",
            "--scopes",
            "read",
            "--expires-in-days",
            "1",
        ],
        None,
    );
    assert!(created.status.success(), "{}", text(&created));
    let fresh_secret = String::from_utf8_lossy(&created.stdout)
        .lines()
        .find(|line| line.starts_with("tartan_pat_"))
        .expect("secret")
        .to_owned();
    let output = ci.run(
        &["login", "--with-token", "--forge", &base],
        Some(&format!("{fresh_secret}\n")),
    );
    assert!(output.status.success(), "{}", text(&output));
    let uri = first.signed_commit("re-proved");
    let output = ci
        .login(
            &["sign", "revoke", &uri, "--no-open"],
            vec![Answer::Follow { extra: "" }],
        )
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    // The forge never relied on this record, so there is nothing for it to re-prove.
    assert!(printed.contains("re-prove"), "{printed}");
    assert!(printed.contains("needs a session"), "{printed}");

    // The operator key file `tt admin oauth keygen` prints is one the forge loads.
    let keygen = first.run(&["admin", "oauth", "keygen"], None);
    assert!(keygen.status.success(), "{}", text(&keygen));
    tartan::settings::parse_keyset(&String::from_utf8_lossy(&keygen.stdout))
        .expect("the forge accepts the generated JWKS");

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
