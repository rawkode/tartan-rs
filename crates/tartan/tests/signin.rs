//! Web sign-in end to end (design D1, spec sign-in): the forge's OAuth client documents, a
//! sign-in against the devnet's authorization server, and every failure the callback reports.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tartan_config::deployment::Readers;
use tartan_forge::{ReadEvents, identity_handle};
use tartan_identity_api::{
    BeginAuth, MAX_PENDING_SIGN_INS, PendingSignIn, SignInCapacity, SignInRefusal,
};

use common::{Forge, approve, browser, finish, problem_query, set_cookies, sign_in, start_sign_in};

/// `GET /-/api/me` with a session cookie.
async fn me(base: &str, session: &str) -> (u16, Value) {
    let response = browser()
        .get(format!("{base}/-/api/me"))
        .header("cookie", format!("tartan_session={session}"))
        .send()
        .await
        .expect("me answers");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// `POST /-/auth/outcome` as a browser holding `cookie`, from `origin`.
async fn read_outcome(
    base: &str,
    id: &str,
    code: &str,
    cookie: Option<&str>,
    origin: &str,
) -> (u16, Value) {
    let mut request = browser()
        .post(format!("{base}/-/auth/outcome"))
        .header("origin", origin)
        .json(&json!({"id": id, "code": code}));
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("tartan_outcome={cookie}"));
    }
    let response = request.send().await.expect("outcome answers");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// A sign-in up to the callback URL: the pre-auth cookie and where the devnet redirects.
async fn up_to_callback(
    base: &str,
    handle: &str,
    return_to: &str,
    extra: &str,
) -> (String, String) {
    let started = start_sign_in(base, handle, return_to, None).await;
    assert_eq!(started.status, 303, "{started:?}");
    assert!(
        !started.location.contains("/#/-/sign-in"),
        "login failed: {started:?}"
    );
    let pre_auth = started.cookies["tartan_oauth"].clone();
    let callback = approve(&format!("{}{extra}", started.location)).await;
    (pre_auth, callback)
}

fn assert_problem(location: &str, code: &str) -> HashMap<String, String> {
    let query = problem_query(location);
    assert_eq!(
        query.get("problem").map(String::as_str),
        Some(code),
        "{location}"
    );
    for leaked in ["host", "error", "error_description"] {
        assert!(!query.contains_key(leaked), "{location} carries {leaked}");
    }
    query
}

/// The identity events recorded so far, as types.
async fn identity_event_types(forge: &Forge) -> Vec<(String, Value)> {
    identity_handle(&forge.client)
        .expect("identity")
        .send(ReadEvents {
            after: 0,
            limit: 500,
        })
        .await
        .expect("events")
        .into_iter()
        .map(|envelope| (envelope.kind, envelope.data))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_end_to_end() {
    let forge = Forge::start("signin").await;
    let base = forge.base.clone();
    let devnet = forge.identity.devnet.clone();
    let alice = forge.identity.owner.clone();

    // The client documents.
    let metadata: Value = browser()
        .get(format!("{base}/-/auth/client-metadata.json"))
        .send()
        .await
        .expect("metadata")
        .json()
        .await
        .expect("json");
    assert_eq!(
        metadata["client_id"],
        json!(format!("{base}/-/auth/client-metadata.json"))
    );
    assert_eq!(metadata["scope"], json!("atproto"));
    assert_eq!(
        metadata["redirect_uris"],
        json!([format!("{base}/-/auth/callback")])
    );
    let jwks: Value = browser()
        .get(format!("{base}/-/auth/jwks.json"))
        .send()
        .await
        .expect("jwks")
        .json()
        .await
        .expect("json");
    let keys = jwks["keys"].as_array().expect("keys");
    assert!(
        !keys.is_empty() && keys.iter().all(|key| key.get("d").is_none()),
        "{jwks}"
    );

    // A sign-in, back to where it started, asking only for atproto.
    let signed = sign_in(&base, "alice.test", "/#/acme/api/issues", None).await;
    assert_eq!(signed.status, 303, "{signed:?}");
    assert_eq!(signed.location, format!("{base}/#/acme/api/issues"));
    assert_eq!(
        signed.cookies.get("tartan_oauth").map(String::as_str),
        Some("")
    );
    let session = signed.session();
    let (status, who) = me(&base, &session).await;
    assert_eq!(status, 200);
    assert_eq!(who["did"], json!(alice));
    assert_eq!(who["owner"], json!(true));
    assert_eq!(who["rights"], json!(["read", "write", "admin"]));
    let pars = devnet.par_requests().await;
    let scope = pars
        .last()
        .expect("a PAR")
        .iter()
        .find(|(k, _)| k == "scope")
        .map(|(_, v)| v.clone());
    assert_eq!(scope.as_deref(), Some("atproto"));
    let events = identity_event_types(&forge).await;
    assert!(
        events
            .iter()
            .any(|(kind, data)| kind == "person.signed_in" && data["did"] == json!(alice))
    );

    // A sign-in from a browser holding a session issues a new secret and ends the old session.
    let again = sign_in(
        &base,
        "alice.test",
        "/",
        Some(&format!("tartan_session={session}")),
    )
    .await;
    let renewed = again.session();
    assert_ne!(renewed, session);
    assert_eq!(me(&base, &session).await.1["anonymous"], json!(true));
    assert_eq!(me(&base, &renewed).await.1["did"], json!(alice));

    // A stranger to this private forge gets a session with no rights and leaves no event.
    let carol = sign_in(&base, "@Carol.Test ", "/", None).await;
    let (_, who) = me(&base, &carol.session()).await;
    assert_eq!(who["rights"], json!([]));
    let carol_did = devnet.did("carol.test").await.expect("carol");
    assert!(
        !identity_event_types(&forge)
            .await
            .iter()
            .any(|(_, data)| data["did"] == json!(carol_did))
    );

    // Signing out ends the session.
    let out = browser()
        .post(format!("{base}/-/auth/logout"))
        .header("origin", &base)
        .header("cookie", format!("tartan_session={renewed}"))
        .send()
        .await
        .expect("logout");
    assert_eq!(out.status(), 204);
    assert_eq!(
        set_cookies(&out).get("tartan_session").map(String::as_str),
        Some("")
    );
    assert_eq!(me(&base, &renewed).await.1["anonymous"], json!(true));

    // A cross-site login form is refused with the expected origin named.
    let foreign = browser()
        .post(format!("{base}/-/auth/login"))
        .header("origin", "https://evil.example")
        .form(&[("handle", "alice.test")])
        .send()
        .await
        .expect("login");
    assert_eq!(foreign.status(), 403);
    assert!(foreign.text().await.expect("text").contains(&base));

    development_clients(&forge).await;
    callback_failures(&forge).await;
    login_failures(&forge).await;
    account_status(&forge).await;
    capacity(&forge).await;
    return_to_values(&forge).await;
    forge.stop().await;
}

/// The development clients: ATProto's `http://localhost` client, and a native client whose
/// metadata is hosted elsewhere (here, the gateway's own copy of the document).
async fn development_clients(forge: &Forge) {
    use tartan::settings::WebClient;
    let alice = forge.identity.owner.clone();
    let localhost = forge
        .gateway_with(Readers::default(), Vec::new(), |_| WebClient::Localhost)
        .await;
    let signed = sign_in(&localhost, "alice.test", "/#/x", None).await;
    assert_eq!(signed.location, format!("{localhost}/#/x"));
    assert_eq!(
        me(&localhost, &signed.session()).await.1["did"],
        json!(alice)
    );
    let pars = forge.identity.devnet.par_requests().await;
    let client_id = pars
        .last()
        .expect("a PAR")
        .iter()
        .find(|(k, _)| k == "client_id")
        .map(|(_, v)| v.clone());
    assert!(
        client_id
            .as_deref()
            .is_some_and(|id| id.starts_with("http://localhost?")),
        "{client_id:?}"
    );
    let assertion = pars
        .last()
        .expect("a PAR")
        .iter()
        .any(|(k, _)| k == "client_assertion");
    assert!(!assertion, "a public client sends no client assertion");

    let native = forge
        .gateway_with(Readers::default(), Vec::new(), |base| WebClient::Native {
            client_id: format!("{base}/-/auth/client-metadata.json"),
        })
        .await;
    let document: Value = browser()
        .get(format!("{native}/-/auth/client-metadata.json"))
        .send()
        .await
        .expect("metadata")
        .json()
        .await
        .expect("json");
    assert_eq!(document["application_type"], json!("native"));
    assert_eq!(document["token_endpoint_auth_method"], json!("none"));
    let signed = sign_in(&native, "alice.test", "/", None).await;
    assert_eq!(me(&native, &signed.session()).await.1["did"], json!(alice));
}

async fn callback_failures(forge: &Forge) {
    // Each part starts with a full sign-in rate-limit bucket.
    forge.clock.advance(Duration::from_secs(180));
    let base = forge.base.as_str();
    let devnet = forge.identity.devnet.clone();

    // A replayed state.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/r", "").await;
    let cookie = format!("tartan_oauth={pre_auth}");
    assert_eq!(finish(&callback, &cookie).await.status, 303);
    let replay = finish(&callback, &cookie).await;
    assert_problem(&replay.location, "expired");
    assert!(!replay.cookies.contains_key("tartan_session"));

    // A valid state from a browser without the pre-auth cookie.
    let (_, callback) = up_to_callback(base, "alice.test", "/r", "").await;
    let other = finish(&callback, "").await;
    let query = assert_problem(&other.location, "browser_mismatch");
    assert_eq!(query.get("return_to").map(String::as_str), Some("/r"));
    assert!(!other.cookies.contains_key("tartan_session"));
    assert_eq!(
        other.cookies.get("tartan_oauth").map(String::as_str),
        Some("")
    );

    // A wrong issuer, and none at all.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
    let forged = callback.replace(
        &format!("iss={}", common::percent(devnet.url())),
        "iss=https%3A%2F%2Fevil.example",
    );
    assert_ne!(forged, callback, "{callback}");
    assert_problem(
        &finish(&forged, &format!("tartan_oauth={pre_auth}"))
            .await
            .location,
        "wrong_issuer",
    );
    devnet.set_switches(|s| s.callback_without_iss = true).await;
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
    devnet
        .set_switches(|s| s.callback_without_iss = false)
        .await;
    assert_problem(
        &finish(&callback, &format!("tartan_oauth={pre_auth}"))
            .await
            .location,
        "wrong_issuer",
    );

    // Metadata without the iss parameter, at the callback.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
    devnet
        .set_switches(|s| s.omit_iss_parameter_supported = true)
        .await;
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    devnet
        .set_switches(|s| s.omit_iss_parameter_supported = false)
        .await;
    assert_problem(&finished.location, "server_unreachable");

    // A token for another DID than the sign-in started with.
    let (pre_auth, callback) = up_to_callback(base, "bob.test", "/", "&as=carol.test").await;
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    assert_problem(&finished.location, "wrong_subject");
    assert!(!finished.cookies.contains_key("tartan_session"));

    // A grant without atproto, and a scope jacquard's own parser would panic on.
    for scope in ["transition:generic", "atproto-not bogus:%%"] {
        devnet
            .set_switches(|s| s.narrow_scope = Some(scope.to_owned()))
            .await;
        let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
        let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
        assert_problem(&finished.location, "missing_scope");
    }
    devnet.set_switches(|s| s.narrow_scope = None).await;

    // Too slow: the callback arrives 11 minutes later.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
    forge.clock.advance(Duration::from_secs(11 * 60));
    assert_problem(
        &finish(&callback, &format!("tartan_oauth={pre_auth}"))
            .await
            .location,
        "expired",
    );

    // Consent denied: the description reaches only this browser, once.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/back", "&deny=1").await;
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    let query = assert_problem(&finished.location, "denied");
    let id = query["outcome"].clone();
    let outcome_cookie = finished.cookies["tartan_outcome"].clone();
    assert_eq!(read_outcome(base, &id, "denied", None, base).await.0, 404);
    assert_eq!(
        read_outcome(base, &id, "denied", Some("someone-else"), base)
            .await
            .0,
        404
    );
    assert_eq!(
        read_outcome(base, &id, "expired", Some(&outcome_cookie), base)
            .await
            .0,
        404
    );
    assert_eq!(
        read_outcome(
            base,
            &id,
            "denied",
            Some(&outcome_cookie),
            "https://evil.example"
        )
        .await
        .0,
        403
    );
    let (status, details) = read_outcome(base, &id, "denied", Some(&outcome_cookie), base).await;
    assert_eq!(status, 200, "{details}");
    assert!(details["error_description"].is_string(), "{details}");
    assert_eq!(
        read_outcome(base, &id, "denied", Some(&outcome_cookie), base)
            .await
            .0,
        404,
        "read once"
    );

    // A token request refused as invalid_client.
    devnet
        .set_switches(|s| s.token_error = Some("invalid_client".to_owned()))
        .await;
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "").await;
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    devnet.set_switches(|s| s.token_error = None).await;
    let query = assert_problem(&finished.location, "client_rejected");
    let (status, details) = read_outcome(
        base,
        &query["outcome"],
        "client_rejected",
        Some(&finished.cookies["tartan_outcome"]),
        base,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(details["error"], json!("invalid_client"));
    assert_eq!(details["host"], json!(common::host(devnet.url())));

    // Outcomes expire after 10 minutes.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/", "&deny=1").await;
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    let id = finished.problem()["outcome"].clone();
    forge.clock.advance(Duration::from_secs(10 * 60 + 1));
    assert_eq!(
        read_outcome(
            base,
            &id,
            "denied",
            Some(&finished.cookies["tartan_outcome"]),
            base
        )
        .await
        .0,
        404
    );
}

async fn login_failures(forge: &Forge) {
    // Each part starts with a full sign-in rate-limit bucket.
    forge.clock.advance(Duration::from_secs(180));
    let base = forge.base.as_str();
    let devnet = forge.identity.devnet.clone();

    // An unknown handle stores nothing.
    let started = start_sign_in(base, "nobody.test", "/x", None).await;
    let query = assert_problem(&started.location, "handle_not_found");
    assert_eq!(query.get("return_to").map(String::as_str), Some("/x"));

    // The PDS and authorization server unreachable: the host only through the outcome.
    devnet.set_switches(|s| s.pds_offline = true).await;
    let started = start_sign_in(base, "alice.test", "/", None).await;
    devnet.set_switches(|s| s.pds_offline = false).await;
    let query = assert_problem(&started.location, "server_unreachable");
    let (status, details) = read_outcome(
        base,
        &query["outcome"],
        "server_unreachable",
        Some(&started.cookies["tartan_outcome"]),
        base,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(details["host"], json!(common::host(devnet.url())));
    devnet.set_switches(|s| s.auth_offline = true).await;
    let started = start_sign_in(base, "alice.test", "/", None).await;
    devnet.set_switches(|s| s.auth_offline = false).await;
    assert_problem(&started.location, "server_unreachable");

    // PAR refused because the client metadata could not be fetched; nothing is stored, so 21
    // refusals from one address leave it below its 20 pending sign-ins.
    devnet
        .set_switches(|s| s.par_error = Some("invalid_client_metadata".to_owned()))
        .await;
    let mut last = None;
    for _ in 0..21 {
        last = Some(start_sign_in(base, "alice.test", "/", None).await);
    }
    devnet.set_switches(|s| s.par_error = None).await;
    let started = last.expect("a refusal");
    let query = assert_problem(&started.location, "client_rejected");
    let (_, details) = read_outcome(
        base,
        &query["outcome"],
        "client_rejected",
        Some(&started.cookies["tartan_outcome"]),
        base,
    )
    .await;
    assert_eq!(details["error"], json!("invalid_client_metadata"));
    let fine = sign_in(base, "alice.test", "/", None).await;
    assert_eq!(fine.status, 303);
    fine.session();
}

async fn account_status(forge: &Forge) {
    // Each part starts with a full sign-in rate-limit bucket.
    forge.clock.advance(Duration::from_secs(180));
    let base = forge.base.as_str();
    let devnet = forge.identity.devnet.clone();

    // A deactivated account: no session, no event, the host only through the outcome.
    devnet
        .set_status("dave.test", "deactivated")
        .await
        .expect("status");
    let finished = sign_in(base, "dave.test", "/d", None).await;
    let query = assert_problem(&finished.location, "account_inactive");
    assert_eq!(query.get("status").map(String::as_str), Some("deactivated"));
    assert_eq!(query.get("return_to").map(String::as_str), Some("/d"));
    assert!(!finished.cookies.contains_key("tartan_session"));
    assert_eq!(
        finished.cookies.get("tartan_oauth").map(String::as_str),
        Some("")
    );
    let (status, details) = read_outcome(
        base,
        &query["outcome"],
        "account_inactive",
        Some(&finished.cookies["tartan_outcome"]),
        base,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(details["host"], json!(common::host(devnet.url())));

    // The stored status is stale once the PDS says active again.
    devnet
        .set_status("dave.test", "active")
        .await
        .expect("status");
    sign_in(base, "dave.test", "/", None).await.session();

    // Throttled accounts sign in.
    devnet
        .set_status("erin.test", "throttled")
        .await
        .expect("status");
    sign_in(base, "erin.test", "/", None).await.session();
    devnet
        .set_status("erin.test", "active")
        .await
        .expect("status");

    // A slow getRepoStatus still completes.
    devnet
        .set_switches(|s| s.repo_status_delay_ms = 9_000)
        .await;
    let started = Instant::now();
    sign_in(base, "bob.test", "/", None).await.session();
    assert!(started.elapsed() >= Duration::from_secs(9));
    devnet.set_switches(|s| s.repo_status_delay_ms = 0).await;

    // The DID moved to another PDS while the sign-in was pending.
    let (pre_auth, callback) = up_to_callback(base, "bob.test", "/", "").await;
    devnet.migrate("bob.test", false).await.expect("migrate");
    let finished = finish(&callback, &format!("tartan_oauth={pre_auth}")).await;
    assert_problem(&finished.location, "wrong_issuer");
}

async fn capacity(forge: &Forge) {
    // Each part starts with a full sign-in rate-limit bucket.
    forge.clock.advance(Duration::from_secs(180));
    use std::net::IpAddr;
    let localhost: IpAddr = [127, 0, 0, 1].into();
    let proxied = forge
        .gateway(Readers::default(), vec![localhost.into()])
        .await;
    let direct = forge.base.as_str();

    // An untrusted peer's X-Forwarded-For is ignored, so it fills its own bucket.
    // Twenty pending sign-ins from one forwarded client: the next is rate_limited, keeping its
    // return_to, while another forwarded client behind the same proxy still signs in.
    for _ in 0..20 {
        let started = start_sign_in(&proxied, "carol.test", "/", Some("203.0.113.1")).await;
        assert!(!started.location.contains("/#/-/sign-in"), "{started:?}");
    }
    let refused = start_sign_in(&proxied, "carol.test", "/#/x", Some("203.0.113.1")).await;
    let query = assert_problem(&refused.location, "rate_limited");
    assert_eq!(query.get("return_to").map(String::as_str), Some("/#/x"));
    assert!(refused.retry_after.is_some());
    sign_in(&proxied, "carol.test", "/", None).await;
    let other = start_sign_in(&proxied, "carol.test", "/", Some("203.0.113.2")).await;
    assert!(!other.location.contains("/#/-/sign-in"), "{other:?}");
    // The direct gateway trusts no proxy: the spoofed header does not put it in that bucket.
    let spoofed = start_sign_in(direct, "carol.test", "/", Some("203.0.113.1")).await;
    assert!(!spoofed.location.contains("/#/-/sign-in"), "{spoofed:?}");

    // The forge-wide cap.
    let identity = identity_handle(&forge.client).expect("identity");
    let carol = forge
        .identity
        .devnet
        .did("carol.test")
        .await
        .expect("carol");
    let mut filled = 0;
    while identity
        .send(SignInCapacity {
            client_ip: "198.51.100.200".into(),
        })
        .await
        .expect("capacity")
        .is_ok()
    {
        let batch: Vec<_> = (0..200)
            .map(|i| {
                let identity = &identity;
                let carol = carol.clone();
                let n = filled + i;
                async move {
                    identity
                        .send(BeginAuth {
                            pending: PendingSignIn {
                                state: format!("filler-{n}"),
                                did: carol,
                                handle: None,
                                issuer: "http://127.0.0.1".into(),
                                pds: "http://127.0.0.1".into(),
                                auth_request: "{}".into(),
                                return_to: "/".into(),
                                client_ip: format!("198.51.{}.{}", n / 250, n % 250),
                                browser_sha256: "00".repeat(32),
                            },
                        })
                        .await
                }
            })
            .collect();
        for result in futures_join(batch).await {
            let _ = result.expect("begin");
        }
        filled += 200;
        assert!(filled <= MAX_PENDING_SIGN_INS + 400, "the cap never closed");
    }
    assert_eq!(
        identity
            .send(SignInCapacity {
                client_ip: "198.51.100.200".into()
            })
            .await
            .expect("capacity"),
        Err(SignInRefusal::Busy)
    );
    let busy = start_sign_in(direct, "alice.test", "/b", None).await;
    let query = assert_problem(&busy.location, "busy");
    assert_eq!(query.get("return_to").map(String::as_str), Some("/b"));
    // The fillers expire, freeing the forge.
    forge.clock.advance(Duration::from_secs(11 * 60));
}

async fn futures_join<F: std::future::Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut outputs = Vec::with_capacity(futures.len());
    let handles: Vec<_> = futures.into_iter().map(Box::pin).collect();
    for handle in handles {
        outputs.push(handle.await);
    }
    outputs
}

async fn return_to_values(forge: &Forge) {
    // Each part starts with a full sign-in rate-limit bucket.
    forge.clock.advance(Duration::from_secs(180));
    let base = forge.base.as_str();
    for (given, expected) in [
        ("//evil.example/", "/"),
        ("/\\evil.example", "/"),
        ("/#/acme/api/issues", "/#/acme/api/issues"),
        ("/#/acme/api/issues?x=1", "/#/acme/api/issues?x=1"),
    ] {
        let signed = sign_in(base, "alice.test", given, None).await;
        assert_eq!(signed.location, format!("{base}{expected}"), "{given}");
        // A problem redirect carries it percent-encoded, and it comes back intact.
        let started = start_sign_in(base, "nobody.test", given, None).await;
        assert!(
            started.location.contains(&format!(
                "return_to={}",
                tartan::auth::percent_encode(expected)
            )),
            "{}",
            started.location
        );
        assert_eq!(problem_query(&started.location)["return_to"], expected);
    }
    // return_to comes from the pending sign-in, never from the callback's query.
    let (pre_auth, callback) = up_to_callback(base, "alice.test", "/kept", "").await;
    let finished = finish(
        &format!("{callback}&return_to=%2F%2Fevil.example"),
        &format!("tartan_oauth={pre_auth}"),
    )
    .await;
    assert_eq!(finished.location, format!("{base}/kept"));
}
