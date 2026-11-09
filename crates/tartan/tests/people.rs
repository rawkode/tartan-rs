//! People shown by handle (design D9 *Caching and display*, spec atproto-identity) and what
//! `tt` needs from the forge (design D7 *tt's OAuth client*, D16 *Forge login hand-off*, spec
//! tt-cli): `GET /-/api/people`, `POST /-/api/people/resolve`, handle re-verification, `tt`'s
//! client metadata and the CLI hand-off routes.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_oauth::request::{OAuthMetadata, par};
use jacquard_oauth::resolver::OAuthResolver;
use jacquard_oauth::types::OAuthClientMetadata;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tartan::jetstream::{JetstreamOptions, spawn as spawn_jetstream};
use tartan_forge::{ReadEvents, identity_handle};
use tartan_identity_api::{KnownAs, People, WatchedDids};
use tokio_util::sync::CancellationToken;

use common::{DEADLINE, Forge, browser, sign_in};

async fn get_people(forge: &Forge, dids: &[&str]) -> (u16, Value) {
    let query: Vec<String> = dids
        .iter()
        .map(|did| format!("did={}", common::percent(did)))
        .collect();
    forge
        .get(&format!("/-/api/people?{}", query.join("&")))
        .await
}

/// Polls `GET /-/api/people` for `did` until `done` holds.
async fn people_until(forge: &Forge, did: &str, done: impl Fn(&Value) -> bool) -> Value {
    let start = Instant::now();
    loop {
        let (status, answer) = get_people(forge, &[did]).await;
        assert_eq!(status, 200, "{answer}");
        if done(&answer[did]) {
            return answer[did].clone();
        }
        assert!(start.elapsed() < DEADLINE, "people for {did}: {answer}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A syntactically valid did:plc numbered `n`.
fn fake_plc(n: u32) -> String {
    let mut id = String::new();
    let mut rest = n;
    for _ in 0..24 {
        id.push(char::from(b'a' + u8::try_from(rest % 26).expect("small")));
        rest /= 26;
    }
    format!("did:plc:{id}")
}

async fn identity_events(forge: &Forge) -> Vec<(String, Value)> {
    identity_handle(&forge.client)
        .expect("identity")
        .send(ReadEvents {
            after: 0,
            limit: 1000,
        })
        .await
        .expect("events")
        .into_iter()
        .map(|envelope| (envelope.kind, envelope.data))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn people_and_the_cli() {
    let forge = Forge::start("people").await;
    let devnet = forge.identity.devnet.clone();
    let cancel = CancellationToken::new();
    let mut options = JetstreamOptions::new(&devnet.jetstream_url());
    options.watch_every = Duration::from_millis(200);
    options.reconnect_after = Duration::from_millis(200);
    spawn_jetstream(forge.client.clone(), options, cancel.clone());

    people(&forge).await;
    tt_client_metadata(&forge).await;
    cli_hand_off(&forge).await;

    cancel.cancel();
    forge.stop().await;
}

async fn people(forge: &Forge) {
    let devnet = &forge.identity.devnet;
    let alice = forge.identity.owner.clone();
    let carol = devnet.did("carol.test").await.expect("carol");
    let identity = identity_handle(&forge.client).expect("identity");

    // The owner's handle is verified at start and answered from the cache.
    let shown = people_until(forge, &alice, |person| person["handle"] == "alice.test").await;
    assert_eq!(shown["status"], "active");

    // An unknown DID is answered null at once; a lookup starts in the background and the
    // handle appears later, without the DID becoming known or watched.
    let (status, first) = get_people(forge, &[&carol]).await;
    assert_eq!(status, 200);
    assert_eq!(first[&carol], Value::Null, "{first}");
    people_until(forge, &carol, |person| person["handle"] == "carol.test").await;
    let cached = identity
        .send(People {
            dids: vec![carol.clone()],
        })
        .await
        .expect("people")
        .remove(0)
        .expect("a cached row");
    assert!(cached.known_as.is_empty(), "{cached:?}");
    let watched = identity.send(WatchedDids {}).await.expect("watched");
    assert!(
        watched.contains(&alice) && !watched.contains(&carol),
        "{watched:?}"
    );

    // At most 100 DIDs per request; a malformed DID is refused.
    let many: Vec<String> = (0..101_u32).map(fake_plc).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(get_people(forge, &many).await.0, 400);
    assert_eq!(get_people(forge, &["alice.test"]).await.0, 400);

    // A handle change reaches the forge through Jetstream's identity event: the handle is
    // re-verified at once and `person.handle.changed` is recorded for the same DID.
    devnet
        .set_handle("alice.test", "alice2.test")
        .await
        .expect("handle changed");
    people_until(forge, &alice, |person| person["handle"] == "alice2.test").await;
    let events = identity_events(forge).await;
    assert!(
        events
            .iter()
            .any(|(kind, data)| kind == "person.handle.changed"
                && data["did"] == alice
                && data["old"] == "alice.test"
                && data["new"] == "alice2.test"),
        "{events:?}"
    );

    // A stale handle whose re-verification cannot reach the PLC directory keeps the last
    // verified handle.
    devnet.set_switches(|s| s.plc_offline = true).await;
    forge.clock.advance(Duration::from_secs(25 * 3600));
    let (_, stale) = get_people(forge, &[&alice]).await;
    assert_eq!(stale[&alice]["handle"], "alice2.test");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, kept) = get_people(forge, &[&alice]).await;
    assert_eq!(kept[&alice]["handle"], "alice2.test", "{kept}");
    devnet.set_switches(|s| s.plc_offline = false).await;

    // Person inputs resolve handles in both directions without making the DID known.
    let bob = devnet.did("bob.test").await.expect("bob");
    let (status, resolved) = forge
        .post("/-/api/people/resolve", json!({"handle": "@Bob.Test"}))
        .await;
    assert_eq!(status, 200, "{resolved}");
    assert_eq!(resolved["did"], bob);
    let (status, _) = forge
        .post("/-/api/people/resolve", json!({"handle": "nobody.test"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(
        forge
            .post("/-/api/people/resolve", json!({"handle": "not a handle"}))
            .await
            .0,
        400
    );
    let bob_row = identity
        .send(People {
            dids: vec![bob.clone()],
        })
        .await
        .expect("people")
        .remove(0);
    assert!(
        bob_row.is_none_or(|row| !row.known_as.contains(&KnownAs::SignedIn)),
        "resolving does not make a DID known"
    );

    // Resolution is rate-limited per client address.
    let mut limited = false;
    for _ in 0..40 {
        let (status, _) = forge
            .post("/-/api/people/resolve", json!({"handle": "nobody.test"}))
            .await;
        if status == 429 {
            limited = true;
            break;
        }
    }
    assert!(limited, "resolution is rate-limited");
}

async fn tt_client_metadata(forge: &Forge) {
    let url = format!("{}/-/auth/tt/client-metadata.json", forge.base);
    let response = browser().get(&url).send().await.expect("metadata");
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .expect("type")
            .starts_with("application/json")
    );
    let document: Value = response.json().await.expect("json");
    assert_eq!(document["client_id"], url);
    assert_eq!(document["application_type"], "native");
    assert_eq!(document["token_endpoint_auth_method"], "none");
    assert_eq!(document["dpop_bound_access_tokens"], true);
    assert_eq!(
        document["redirect_uris"],
        json!(["http://127.0.0.1/callback", "http://[::1]/callback"])
    );
    assert_eq!(
        document["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(
        document["scope"],
        "atproto repo:academy.rawkode.tartan.signature?action=create \
         repo:academy.rawkode.tartan.signature?action=delete"
    );

    // The devnet's authorization server fetches this client id and accepts a loopback
    // redirect on a port the metadata does not name, as `tt` sends it.
    let resolver = forge.identity.resolver.clone();
    let pds = forge.identity.devnet.url().to_owned();
    let server_metadata = resolver
        .get_resource_server_metadata(&pds)
        .await
        .expect("the authorization server's metadata");
    let mut client = document.clone();
    client["redirect_uris"] = json!(["http://127.0.0.1:49152/callback"]);
    client["scope"] = json!("atproto repo:academy.rawkode.tartan.signature?action=create");
    let client_metadata: OAuthClientMetadata<SmolStr> =
        serde_json::from_value(client).expect("client metadata");
    let mut metadata = OAuthMetadata {
        server_metadata,
        client_metadata,
        keyset: None,
    };
    let pushed = par(
        &resolver,
        Some(SmolStr::from("dave.test")),
        None,
        &mut metadata,
        None,
    )
    .await
    .expect("the PAR is accepted");
    assert!(!pushed.request_uri.is_empty());
    let received = forge.identity.devnet.par_requests().await;
    let last = received.last().expect("a PAR body");
    let field = |name: &str| {
        last.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(field("client_id").as_deref(), Some(url.as_str()));
    assert_eq!(
        field("redirect_uri").as_deref(),
        Some("http://127.0.0.1:49152/callback")
    );
    assert_eq!(field("client_assertion"), None);
}

fn pkce() -> (String, String) {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("random");
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

async fn approve(base: &str, session: &str, origin: &str, body: Value) -> (u16, Value) {
    let response = browser()
        .post(format!("{base}/-/auth/cli/approve"))
        .header("cookie", format!("tartan_session={session}"))
        .header("origin", origin)
        .json(&body)
        .send()
        .await
        .expect("approve answers");
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn exchange(base: &str, code: &str, verifier: &str) -> (u16, Value) {
    let response = browser()
        .post(format!("{base}/-/auth/cli/token"))
        .json(&json!({"code": code, "code_verifier": verifier}))
        .send()
        .await
        .expect("token answers");
    (
        response.status().as_u16(),
        response.json().await.unwrap_or(Value::Null),
    )
}

/// The `code` of an approval's loopback redirect, after checking its target and `state`.
fn code_of(redirect: &str, port: u16, state: &str) -> String {
    let url = reqwest::Url::parse(redirect).expect("a redirect URL");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert_eq!(url.port(), Some(port));
    assert_eq!(url.path(), "/forge");
    let pairs: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], state);
    pairs["code"].clone()
}

async fn me(base: &str, bearer: &str) -> (u16, Value) {
    let response = browser()
        .get(format!("{base}/-/api/me"))
        .bearer_auth(bearer)
        .send()
        .await
        .expect("me answers");
    (
        response.status().as_u16(),
        response.json().await.unwrap_or(Value::Null),
    )
}

async fn cli_hand_off(forge: &Forge) {
    let base = &forge.base;
    let state = URL_SAFE_NO_PAD.encode([3_u8; 32]);
    let alice = sign_in(base, "alice2.test", "/", None).await.session();

    // Approve and exchange: the CLI session acts as the person, with no Origin needed.
    let (verifier, challenge) = pkce();
    let (status, approved) = approve(
        base,
        &alice,
        base,
        json!({"port": 54321, "state": state, "challenge": challenge, "device": "laptop"}),
    )
    .await;
    assert_eq!(status, 200, "{approved}");
    let code = code_of(
        approved["redirect"].as_str().expect("redirect"),
        54321,
        &state,
    );
    let (status, session) = exchange(base, &code, &verifier).await;
    assert_eq!(status, 200, "{session}");
    let secret = session["session"].as_str().expect("a secret").to_owned();
    assert!(secret.starts_with("tartan_cli_"));
    let (status, who) = me(base, &secret).await;
    assert_eq!(status, 200);
    assert_eq!(who["did"], forge.identity.owner);
    assert_eq!(who["kind"], "cli");
    assert_eq!(who["rights"], json!(["read", "write", "admin"]));

    // Code replayed: refused, no session.
    let (status, replayed) = exchange(base, &code, &verifier).await;
    assert_eq!(status, 400, "{replayed}");
    assert!(replayed.get("session").is_none());

    // Wrong verifier: refused, and the code is consumed.
    let (verifier, challenge) = pkce();
    let (_, approved) = approve(
        base,
        &alice,
        base,
        json!({"port": 1024, "state": state, "challenge": challenge, "device": "laptop"}),
    )
    .await;
    let code = code_of(
        approved["redirect"].as_str().expect("redirect"),
        1024,
        &state,
    );
    let (wrong, _) = pkce();
    assert_eq!(exchange(base, &code, &wrong).await.0, 400);
    assert_eq!(exchange(base, &code, &verifier).await.0, 400, "consumed");

    // Cross-site approval: a foreign Origin is refused before any code is issued.
    let (_, challenge) = pkce();
    let (status, body) = approve(
        base,
        &alice,
        "https://evil.example",
        json!({"port": 54321, "state": state, "challenge": challenge, "device": "laptop"}),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(body.get("redirect").is_none());

    // Non-loopback target: only a port is accepted, so another host cannot be named.
    for body in [
        json!({"port": "evil.example:8080", "state": state, "challenge": challenge, "device": "laptop"}),
        json!({"port": 8080, "state": state, "challenge": challenge, "device": "laptop",
               "redirect_uri": "http://evil.example:8080/forge"}),
        json!({"port": 80, "state": state, "challenge": challenge, "device": "laptop"}),
        json!({"port": 65536, "state": state, "challenge": challenge, "device": "laptop"}),
    ] {
        let (status, answer) = approve(base, &alice, base, body).await;
        assert_eq!(status, 400, "{answer}");
    }

    // No session: no approval.
    let (status, _) = approve(
        base,
        "not-a-session",
        base,
        json!({"port": 54321, "state": state, "challenge": challenge, "device": "laptop"}),
    )
    .await;
    assert_eq!(status, 401);

    // A person the forge does not let read can complete the hand-off; the CLI session has no
    // rights, exactly like their browser session.
    let carol = sign_in(base, "carol.test", "/", None).await.session();
    let (verifier, challenge) = pkce();
    let (status, approved) = approve(
        base,
        &carol,
        base,
        json!({"port": 40000, "state": state, "challenge": challenge, "device": "carol's laptop"}),
    )
    .await;
    assert_eq!(status, 200, "{approved}");
    let code = code_of(
        approved["redirect"].as_str().expect("redirect"),
        40000,
        &state,
    );
    let (status, session) = exchange(base, &code, &verifier).await;
    assert_eq!(status, 200);
    let secret = session["session"].as_str().expect("a secret");
    let (status, who) = me(base, secret).await;
    assert_eq!(status, 200);
    assert_eq!(who["rights"], json!([]));
    assert_eq!(who["kind"], "cli");
    let response = browser()
        .get(format!("{base}/-/api/orgs"))
        .bearer_auth(secret)
        .send()
        .await
        .expect("orgs");
    assert_eq!(response.status(), 403);
}
