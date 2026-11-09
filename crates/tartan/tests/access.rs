//! Who may do what (design D4, spec forge-access): the rights table under each `readers` mode,
//! sessions of both kinds, access tokens and their limits, the `Origin` check, and the routes
//! that stay outside `authenticate`.
//!
//! One engine and one set of actors serve three gateways, one per read mode: rights are computed
//! in the gateway from `owners` and `readers`, so each gateway shows the forge as that mode would.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tartan_config::deployment::Readers;
use tartan_forge::identity_handle;
use tartan_identity_api::{IssueCliCode, RedeemCliCode};

use common::{Forge, browser, sign_in};

/// How a test request authenticates.
#[derive(Clone, Copy)]
enum As<'a> {
    Nobody,
    /// A session cookie, sent with the forge's own `Origin`.
    Cookie(&'a str),
    /// A session cookie sent with no `Origin`, as a cross-site form would arrive.
    CookieNoOrigin(&'a str),
    Bearer(&'a str),
    /// A bearer credential and a cookie at once.
    Both(&'a str, &'a str),
}

async fn call(
    base: &str,
    who: As<'_>,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value, reqwest::header::HeaderMap) {
    let mut request = browser().request(method, format!("{base}{path}"));
    match who {
        As::Nobody => {}
        As::Cookie(session) => {
            request = request
                .header("cookie", format!("tartan_session={session}"))
                .header("origin", base);
        }
        As::CookieNoOrigin(session) => {
            request = request.header("cookie", format!("tartan_session={session}"));
        }
        As::Bearer(secret) => request = request.bearer_auth(secret),
        As::Both(secret, session) => {
            request = request
                .bearer_auth(secret)
                .header("cookie", format!("tartan_session={session}"));
        }
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("the gateway answers");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
        headers,
    )
}

async fn get(base: &str, who: As<'_>, path: &str) -> u16 {
    call(base, who, reqwest::Method::GET, path, None).await.0
}

async fn post(base: &str, who: As<'_>, path: &str, body: Value) -> u16 {
    call(base, who, reqwest::Method::POST, path, Some(body))
        .await
        .0
}

/// A CLI session for `did`, through the `tt login` hand-off's identity actions.
async fn cli_session(forge: &Forge, did: &str) -> String {
    let verifier = URL_SAFE_NO_PAD.encode([7_u8; 32]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let identity = identity_handle(&forge.client).expect("identity");
    let code = identity
        .send(IssueCliCode {
            did: did.to_owned(),
            challenge,
            device: "laptop".to_owned(),
            client_ip: "127.0.0.1".to_owned(),
        })
        .await
        .expect("answers")
        .expect("a code");
    identity
        .send(RedeemCliCode { code, verifier })
        .await
        .expect("answers")
        .expect("a session")
        .secret
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rights_by_read_mode() {
    let forge = Forge::start("access").await;
    let private = forge.base.clone();
    let devnet = forge.identity.devnet.clone();
    let bob_did = devnet.did("bob.test").await.expect("bob");
    let open = forge.gateway(Readers::SignedIn, Vec::new()).await;
    let listed = forge
        .gateway(
            Readers::Dids(vec![bob_did.parse().expect("a DID")]),
            Vec::new(),
        )
        .await;

    // The owner sets up an org and a repository with her token.
    assert_eq!(
        forge.post("/-/api/orgs", json!({"slug": "acme"})).await.0,
        201
    );
    assert_eq!(
        forge
            .post("/-/api/orgs/acme/repos", json!({"slug": "api"}))
            .await
            .0,
        201
    );
    assert_eq!(
        forge
            .post("/-/api/orgs/acme/repos", json!({"slug": "web"}))
            .await
            .0,
        201
    );

    let alice = sign_in(&private, "alice.test", "/", None).await.session();
    let bob = sign_in(&private, "bob.test", "/", None).await.session();
    let carol = sign_in(&private, "carol.test", "/", None).await.session();

    // Anonymous: 401 everywhere but /me.
    assert_eq!(get(&private, As::Nobody, "/-/api/orgs").await, 401);
    let (status, me, _) = call(
        &private,
        As::Nobody,
        reqwest::Method::GET,
        "/-/api/me",
        None,
    )
    .await;
    assert_eq!((status, me["anonymous"].clone()), (200, json!(true)));

    // Owners read, write and administer.
    assert_eq!(get(&private, As::Cookie(&alice), "/-/api/orgs").await, 200);
    assert_eq!(
        post(
            &private,
            As::Cookie(&alice),
            "/-/api/orgs",
            json!({"slug": "beta"})
        )
        .await,
        201
    );

    // readers absent ("owners"): a signed-in non-owner gets 403 everywhere but /me.
    for path in [
        "/-/api/orgs",
        "/-/api/orgs/acme/repos",
        "/-/api/packages",
        "/-/api/tokens",
    ] {
        assert_eq!(get(&private, As::Cookie(&bob), path).await, 403, "{path}");
    }
    assert_eq!(
        post(
            &private,
            As::Cookie(&bob),
            "/-/api/orgs",
            json!({"slug": "x"})
        )
        .await,
        403
    );
    let (_, me, _) = call(
        &private,
        As::Cookie(&bob),
        reqwest::Method::GET,
        "/-/api/me",
        None,
    )
    .await;
    assert_eq!(me["rights"], json!([]));
    assert_eq!(me["did"], json!(bob_did));
    // Their own sessions stay theirs to see and end.
    assert_eq!(
        get(&private, As::Cookie(&bob), "/-/api/sessions").await,
        200
    );

    // readers "signed_in": every signed-in person reads, and only reads.
    assert_eq!(
        get(&open, As::Cookie(&bob), "/-/api/orgs/acme/repos").await,
        200
    );
    assert_eq!(get(&open, As::Cookie(&carol), "/-/api/orgs").await, 200);
    assert_eq!(
        post(&open, As::Cookie(&bob), "/-/api/orgs", json!({"slug": "x"})).await,
        403
    );
    // An extension action always writes, so a reader's is refused at the gateway. A reader's
    // tool call reaches the installation, which runs only tools declared `access: "read"`
    // (tested with Issues and Kanban installed in tests/kanban.rs); here none is installed.
    let act = "/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/actions/close";
    let (status, refusal, _) = call(
        &open,
        As::Cookie(&bob),
        reqwest::Method::POST,
        act,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 403);
    assert!(refusal.to_string().contains("read-only"), "{refusal}");
    let tool = "/-/api/orgs/acme/repos/api/extensions/issues.tartan.rawkode.academy/tools/create";
    let (status, refusal, _) = call(
        &open,
        As::Cookie(&bob),
        reqwest::Method::POST,
        tool,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 404, "{refusal}");
    let (_, me, _) = call(
        &open,
        As::Cookie(&bob),
        reqwest::Method::GET,
        "/-/api/me",
        None,
    )
    .await;
    assert_eq!(me["rights"], json!(["read"]));

    // A reader list: only the listed DID reads.
    assert_eq!(get(&listed, As::Cookie(&bob), "/-/api/orgs").await, 200);
    assert_eq!(get(&listed, As::Cookie(&carol), "/-/api/orgs").await, 403);

    // A CLI session has its person's rights and needs no Origin.
    let cli = cli_session(&forge, &forge.identity.owner).await;
    assert_eq!(
        post(
            &private,
            As::Bearer(&cli),
            "/-/api/orgs",
            json!({"slug": "gamma"})
        )
        .await,
        201
    );
    let bob_cli = cli_session(&forge, &bob_did).await;
    assert_eq!(
        get(&private, As::Bearer(&bob_cli), "/-/api/orgs").await,
        403
    );
    assert_eq!(get(&open, As::Bearer(&bob_cli), "/-/api/orgs").await, 200);

    // Cookie writes need the forge's own Origin: a cross-site form is refused.
    let (status, text, _) = call(
        &private,
        As::CookieNoOrigin(&alice),
        reqwest::Method::POST,
        "/-/api/orgs",
        Some(json!({"slug": "delta"})),
    )
    .await;
    assert_eq!(status, 403);
    assert!(text.to_string().contains(&private), "{text}");

    // Tokens: created from a session, scoped, intersected with the DID's rights.
    let (status, made, _) = call(
        &private,
        As::Cookie(&alice),
        reqwest::Method::POST,
        "/-/api/tokens",
        Some(json!({"name": "ci", "scopes": ["read", "write"], "expires_in_days": 7})),
    )
    .await;
    assert_eq!(status, 201, "{made}");
    let write_token = made["secret"].as_str().expect("secret").to_owned();
    let write_id = made["token"]["id"].as_str().expect("id").to_owned();
    assert_eq!(
        get(&private, As::Bearer(&write_token), "/-/api/orgs").await,
        200
    );
    assert_eq!(
        post(
            &private,
            As::Bearer(&write_token),
            "/-/api/orgs",
            json!({"slug": "eps"})
        )
        .await,
        403
    );
    // A token cannot make tokens or list sessions.
    assert_eq!(
        post(
            &private,
            As::Bearer(&write_token),
            "/-/api/tokens",
            json!({"name": "x", "scopes": ["read"], "expires_in_days": 1})
        )
        .await,
        403
    );
    let (_, me, _) = call(
        &private,
        As::Bearer(&write_token),
        reqwest::Method::GET,
        "/-/api/me",
        None,
    )
    .await;
    assert_eq!(
        (me["kind"].clone(), me["rights"].clone()),
        (json!("token"), json!(["read", "write"]))
    );
    // Revoked: refused at once in this process, even with a good cookie alongside.
    let (status, _, _) = call(
        &private,
        As::Cookie(&alice),
        reqwest::Method::DELETE,
        &format!("/-/api/tokens/{write_id}"),
        None,
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(
        get(&private, As::Both(&write_token, &alice), "/-/api/orgs").await,
        401
    );
    assert_eq!(
        get(&private, As::Bearer("tartan_pat_unknown"), "/-/api/me").await,
        401
    );

    // A reader's token with every scope only reads.
    let (status, made, _) = call(
        &open,
        As::Cookie(&bob),
        reqwest::Method::POST,
        "/-/api/tokens",
        Some(json!({"name": "all", "scopes": ["read", "write", "admin"], "expires_in_days": 7})),
    )
    .await;
    assert_eq!(status, 201, "{made}");
    let bob_token = made["secret"].as_str().expect("secret").to_owned();
    assert_eq!(get(&open, As::Bearer(&bob_token), "/-/api/orgs").await, 200);
    assert_eq!(
        post(
            &open,
            As::Bearer(&bob_token),
            "/-/api/orgs",
            json!({"slug": "zeta"})
        )
        .await,
        403
    );

    // A repository-limited admin token.
    let (status, made, _) = call(
        &private,
        As::Cookie(&alice),
        reqwest::Method::POST,
        "/-/api/tokens",
        Some(json!({"name": "api", "scopes": ["read", "write", "admin"], "repos": ["acme/api"], "expires_in_days": 7})),
    )
    .await;
    assert_eq!(status, 201, "{made}");
    let limited = made["secret"].as_str().expect("secret").to_owned();
    assert_eq!(
        get(&private, As::Bearer(&limited), "/-/api/orgs/acme/repos/api").await,
        200
    );
    assert_eq!(
        get(&private, As::Bearer(&limited), "/-/api/orgs/acme").await,
        200
    );
    assert_eq!(
        get(&private, As::Bearer(&limited), "/-/api/orgs/acme/repos/web").await,
        403
    );
    assert_eq!(
        post(
            &private,
            As::Bearer(&limited),
            "/-/api/orgs",
            json!({"slug": "eta"})
        )
        .await,
        403
    );
    let (status, _, _) = call(
        &private,
        As::Bearer(&limited),
        reqwest::Method::PUT,
        "/-/api/placements",
        Some(json!({"target": "org/acme"})),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(
        get(
            &private,
            As::Bearer(&limited),
            "/-/api/subscribers/all/events"
        )
        .await,
        403
    );
    assert_eq!(get(&private, As::Bearer(&limited), "/-/api/me").await, 200);

    // Sessions: list with the current one marked, end the current one.
    let (status, sessions, _) = call(
        &private,
        As::Bearer(&cli),
        reqwest::Method::GET,
        "/-/api/sessions",
        None,
    )
    .await;
    assert_eq!(status, 200);
    let current: Vec<&Value> = sessions
        .as_array()
        .expect("list")
        .iter()
        .filter(|s| s["current"] == json!(true))
        .collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["kind"], json!("cli"));
    assert_eq!(current[0]["device"], json!("laptop"));
    let (status, _, _) = call(
        &private,
        As::Bearer(&cli),
        reqwest::Method::DELETE,
        "/-/api/sessions/current",
        None,
    )
    .await;
    assert_eq!(status, 204);
    assert_eq!(get(&private, As::Bearer(&cli), "/-/api/me").await, 401);

    // Response headers on the API and the registry; the registry and webhooks need no
    // credential.
    let (_, _, headers) = call(
        &private,
        As::Nobody,
        reqwest::Method::GET,
        "/-/api/me",
        None,
    )
    .await;
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["content-security-policy"],
        "sandbox; default-src 'none'"
    );
    let (status, _, headers) = call(&private, As::Nobody, reqwest::Method::GET, "/v2/", None).await;
    assert_eq!(status, 200);
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["content-security-policy"],
        "sandbox; default-src 'none'"
    );
    assert_eq!(forge.hook(&json!([])).await, 204);

    forge.stop().await;
}
