//! End-to-end tests of the devnet over HTTP, driven by the same client code the forge and `tt`
//! use: `tartan-atproto`'s verifying resolver and proof checks, and jacquard-oauth's PAR, code
//! exchange, refresh and DPoP calls.

use std::time::Duration;

use axum::Json;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_oauth::dpop::wrap_request_with_dpop;
use jacquard_oauth::keyset::Keyset;
use jacquard_oauth::request::{
    OAuthMetadata, OAuthRequest, exchange_code, oauth_request, par, revoke,
};
use jacquard_oauth::resolver::OAuthResolver;
use jacquard_oauth::session::{DpopClientData, DpopReqData};
use jacquard_oauth::types::{
    OAuthClientMetadata, OAuthTokenResponse, RefreshRequestParameters, TokenGrantType, TokenSet,
};
use serde_json::{Value, json};
use tartan_atproto::Mode;
use tartan_atproto::did::Did;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tartan_atproto::handle::Handle;
use tartan_atproto::proof::{ProofOutcome, RecordAnswer, RecordRef, fetch_record, prove_record};
use tartan_atproto::resolver::{ResolverConfig, VerifyingResolver};
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::record::{Salt, SignatureRecord};
use tokio_tungstenite::tungstenite::Message;

use super::*;

const SIGN_CREATE: &str = "repo:academy.rawkode.tartan.signature?action=create";
const SIGN_DELETE: &str = "repo:academy.rawkode.tartan.signature?action=delete";

struct Net {
    devnet: Devnet,
    resolver: VerifyingResolver,
    http: reqwest::Client,
    clients: String,
    _client_server: JoinHandle<()>,
    web_key: p256::SecretKey,
}

/// A devnet, a resolver pointed at it and a server for client metadata documents.
async fn net() -> Net {
    let devnet = Devnet::start(DevnetConfig::default())
        .await
        .expect("devnet");
    let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("fetch");
    let resolver = VerifyingResolver::new(
        fetch,
        ResolverConfig::development(devnet.url(), devnet.url()),
    );
    let web_key = p256::SecretKey::from_slice(&[9u8; 32]).expect("key");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let clients = format!("http://{}", listener.local_addr().expect("addr"));
    let jwks = json!({"keys": [public_jwk(&web_key.public_key(), "k1")]});
    let (tt, web) = (tt_metadata(&clients), web_metadata(&clients));
    let router = axum::Router::new()
        .route("/tt.json", get(move || async move { Json(tt) }))
        .route("/web.json", get(move || async move { Json(web) }))
        .route("/jwks.json", get(move || async move { Json(jwks) }));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Net {
        devnet,
        resolver,
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("http"),
        clients,
        _client_server: task,
        web_key,
    }
}

fn public_jwk(key: &p256::PublicKey, kid: &str) -> Value {
    let mut jwk = serde_json::to_value(jose_jwk::Jwk {
        key: jose_jwk::Key::from(&jose_jwk::crypto::Key::from(*key)),
        prm: Default::default(),
    })
    .expect("jwk");
    jwk["kid"] = json!(kid);
    jwk
}

fn tt_metadata(base: &str) -> Value {
    json!({
        "client_id": format!("{base}/tt.json"),
        "client_name": "tt (test)",
        "application_type": "native",
        "redirect_uris": ["http://127.0.0.1/callback", "http://[::1]/callback"],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": format!("atproto {SIGN_CREATE} {SIGN_DELETE}"),
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

fn web_metadata(base: &str) -> Value {
    json!({
        "client_id": format!("{base}/web.json"),
        "client_name": "Tartan (test)",
        "application_type": "web",
        "redirect_uris": [format!("{base}/-/auth/callback")],
        "response_types": ["code"],
        "grant_types": ["authorization_code"],
        "scope": "atproto",
        "token_endpoint_auth_method": "private_key_jwt",
        "token_endpoint_auth_signing_alg": "ES256",
        "jwks_uri": format!("{base}/jwks.json"),
        "dpop_bound_access_tokens": true,
    })
}

/// `tt`'s metadata as `tt` itself holds it: the redirect carries the listening port and the
/// scope is what it requests.
fn tt_client(net: &Net, port: u16, scope: &str) -> Value {
    let mut metadata = tt_metadata(&net.clients);
    metadata["redirect_uris"] = json!([format!("http://127.0.0.1:{port}/callback")]);
    metadata["scope"] = json!(scope);
    metadata
}

fn keyset(key: &p256::SecretKey) -> Keyset {
    let mut jwk = jose_jwk::Jwk {
        key: jose_jwk::Key::from(&jose_jwk::crypto::Key::from(key.clone())),
        prm: Default::default(),
    };
    jwk.prm.kid = Some("k1".into());
    Keyset::try_from(vec![jwk]).expect("keyset")
}

struct Login {
    token: TokenSet,
    dpop: DpopReqData,
    metadata: OAuthMetadata,
    location: url::Url,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("location", &self.location.as_str())
            .finish_non_exhaustive()
    }
}

impl Login {
    fn client_data(&self) -> DpopClientData {
        DpopClientData {
            dpop_key: self.dpop.dpop_key.clone(),
            dpop_authserver_nonce: self.dpop.dpop_authserver_nonce.clone().unwrap_or_default(),
            dpop_host_nonce: SmolStr::default(),
        }
    }
}

async fn oauth_metadata(net: &Net, client: Value, keyset: Option<Keyset>) -> OAuthMetadata {
    let server_metadata = net
        .resolver
        .get_authorization_server_metadata(net.devnet.url())
        .await
        .expect("server metadata");
    OAuthMetadata {
        server_metadata,
        client_metadata: serde_json::from_value::<OAuthClientMetadata>(client).expect("client"),
        keyset,
    }
}

/// PAR, the authorize page (followed without a browser) and the code exchange. Errors are the
/// `Debug` text of whatever failed.
async fn login(
    net: &Net,
    client: Value,
    keyset: Option<Keyset>,
    hint: &str,
) -> Result<Login, String> {
    let mut metadata = oauth_metadata(net, client, keyset).await;
    let mut request = par(
        &net.resolver,
        Some(SmolStr::from(hint)),
        None,
        &mut metadata,
        None,
    )
    .await
    .map_err(|error| format!("{error:?}"))?;
    let location = authorize(net, &metadata, &request.request_uri, "").await;
    let query: Vec<(String, String)> = location.query_pairs().into_owned().collect();
    let get = |name: &str| util::first(&query, name).map(str::to_owned);
    assert_eq!(get("state").as_deref(), Some(request.state.as_str()));
    assert_eq!(get("iss").as_deref(), Some(net.devnet.url()));
    let code = get("code").ok_or_else(|| format!("no code in {location}"))?;
    let verifier = request.pkce_verifier.clone();
    let token = exchange_code(
        &net.resolver,
        &mut request.dpop_data,
        &code,
        &verifier,
        &metadata,
    )
    .await
    .map_err(|error| format!("{error:?}"))?;
    Ok(Login {
        token,
        dpop: request.dpop_data,
        metadata,
        location,
    })
}

/// Follows the authorization URL with a plain HTTP client and returns the redirect target.
async fn authorize(
    net: &Net,
    metadata: &OAuthMetadata,
    request_uri: &str,
    extra: &str,
) -> url::Url {
    let mut url = url::Url::parse(&format!("{}/oauth/authorize", net.devnet.url())).expect("url");
    url.query_pairs_mut()
        .append_pair("client_id", metadata.client_metadata.client_id.as_str())
        .append_pair("request_uri", request_uri);
    let response = net
        .http
        .get(format!("{url}{extra}"))
        .send()
        .await
        .expect("authorize");
    assert_eq!(response.status(), 303, "{:?}", response.text().await);
    let location = response.headers()["location"]
        .to_str()
        .expect("location")
        .to_owned();
    url::Url::parse(&location).expect("location")
}

async fn refresh_with(
    net: &Net,
    login: &mut Login,
    refresh_token: &str,
) -> Result<OAuthTokenResponse, String> {
    oauth_request::<OAuthTokenResponse, _, _, _>(
        &net.resolver,
        &mut login.dpop,
        OAuthRequest::Refresh(RefreshRequestParameters {
            grant_type: TokenGrantType::RefreshToken,
            refresh_token,
            scope: None,
        }),
        &login.metadata,
    )
    .await
    .map_err(|error| format!("{error:?}"))
}

/// A DPoP-authorized POST to the PDS, with jacquard's nonce handling.
async fn pds_post(
    net: &Net,
    token: &str,
    dpop: &mut DpopClientData,
    method: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{}/xrpc/{method}", net.devnet.url()))
        .header("Authorization", format!("DPoP {token}"))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body).expect("json"))
        .expect("request");
    let response = wrap_request_with_dpop(&net.resolver, dpop, false, request)
        .await
        .expect("send");
    (
        response.status().as_u16(),
        serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    )
}

async fn bearer_post(
    net: &Net,
    token: &str,
    method: &str,
    body: Value,
) -> reqwest::Result<reqwest::Response> {
    net.http
        .post(format!("{}/xrpc/{method}", net.devnet.url()))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
}

async fn app_password_token(net: &Net, who: &str) -> String {
    let password = net.devnet.create_app_password(who).await.expect("password");
    let session: Value = net
        .http
        .post(format!(
            "{}/xrpc/com.atproto.server.createSession",
            net.devnet.url()
        ))
        .json(&json!({"identifier": who, "password": password}))
        .send()
        .await
        .expect("session")
        .json()
        .await
        .expect("json");
    session["accessJwt"].as_str().expect("token").to_owned()
}

fn create(collection: &str, rkey: &str, value: Value) -> Value {
    json!({
        "$type": "com.atproto.repo.applyWrites#create",
        "collection": collection,
        "rkey": rkey,
        "value": value,
    })
}

fn post(text: &str) -> Value {
    json!({"$type": "app.bsky.feed.post", "text": text, "createdAt": "2026-11-09T12:00:00Z"})
}

fn signature_value() -> Value {
    signature_value_with([3u8; 32])
}

fn signature_value_with(salt: [u8; 32]) -> Value {
    let salt = Salt::from_bytes(salt);
    SignatureRecord::for_git(b"payload", &salt, "2026-11-09T12:00:00Z".into()).to_json()
}

/// Resolves `did` with the verifying resolver and proves the record from its current PDS.
async fn prove(net: &Net, did: &str, collection: &str, rkey: &str) -> ProofOutcome {
    let did = Did::parse(did, Mode::Development).expect("did");
    let identity = net
        .resolver
        .resolve_did(&did)
        .await
        .expect("resolve")
        .identity
        .expect("not tombstoned");
    let record = RecordRef {
        did,
        collection: collection.into(),
        rkey: rkey.into(),
    };
    let pds = identity.pds.expect("pds");
    match fetch_record(net.resolver.fetch(), &pds, &record).await {
        RecordAnswer::Car(car) => {
            prove_record(
                car,
                record,
                identity.signing_key.expect("key"),
                util::now_ms(),
            )
            .await
        }
        other => panic!("{other:?}"),
    }
}

async fn record_answer(net: &Net, did: &str, rkey: &str, pds: &str) -> RecordAnswer {
    let record = RecordRef {
        did: Did::parse(did, Mode::Development).expect("did"),
        collection: COLLECTION.into(),
        rkey: rkey.into(),
    };
    fetch_record(net.resolver.fetch(), pds, &record).await
}

fn handle(text: &str) -> Handle {
    Handle::parse(text, Mode::Development).expect("handle")
}

#[tokio::test]
async fn seeded_dids_are_stable_across_starts() {
    let first = Devnet::start(DevnetConfig::default())
        .await
        .expect("devnet");
    let second = Devnet::start(DevnetConfig::default())
        .await
        .expect("devnet");
    let other = Devnet::start(DevnetConfig {
        seed: "other".into(),
        ..DevnetConfig::default()
    })
    .await
    .expect("devnet");
    assert_ne!(first.url(), second.url());
    let first_people = first.people().await;
    assert_eq!(
        first_people
            .iter()
            .map(|p| p.handle.as_str())
            .collect::<Vec<_>>(),
        SEEDED_HANDLES
    );
    for (a, b) in first_people.iter().zip(second.people().await) {
        assert_eq!(a.did, b.did);
        assert_eq!(
            seeded_did("dev", &a.handle).as_deref(),
            Some(a.did.as_str())
        );
    }
    assert_ne!(first.did("alice.test").await, other.did("alice.test").await);
    assert_eq!(seeded_did("dev", "nobody.test"), None);
}

#[tokio::test]
async fn resolves_and_proves_a_signature_record() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let salt = Salt::generate().expect("salt");
    let written = net
        .devnet
        .write_signature("alice.test", b"tree 0123\n", &salt, None, None)
        .await
        .expect("write");
    assert!(
        written
            .uri
            .starts_with(&format!("at://{alice}/{COLLECTION}/"))
    );

    let resolved = net
        .resolver
        .resolve_verified_handle(&handle("alice.test"))
        .await
        .expect("handle");
    assert_eq!(resolved.as_str(), alice);

    match prove(&net, &alice, COLLECTION, &written.rkey).await {
        ProofOutcome::Present {
            record_cid, record, ..
        } => {
            assert_eq!(record_cid.to_string(), written.cid);
            let record = SignatureRecord::decode(&record).expect("signature record");
            record
                .check(b"tree 0123\n", &salt)
                .expect("signs the payload");
            assert_eq!(record.cid().to_string(), written.cid);
        }
        other => panic!("{other:?}"),
    }

    // A deleted record's getRecord answer is a 200 non-existence CAR.
    net.devnet
        .delete_record("alice.test", COLLECTION, &written.rkey, None)
        .await
        .expect("delete");
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Absent { .. }
    ));

    net.devnet
        .set_switches(|s| s.drop_absence_path_block = true)
        .await;
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Incomplete(_)
    ));
    net.devnet
        .merge_switches(
            json!({"drop_absence_path_block": false, "record_not_found_for_missing": true}),
        )
        .await
        .expect("switches");
    assert_eq!(
        record_answer(&net, &alice, &written.rkey, net.devnet.url()).await,
        RecordAnswer::RecordNotFound
    );
}

#[tokio::test]
async fn a_create_only_token_cannot_delete_or_write_elsewhere() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let login = login(
        &net,
        tt_client(&net, 50123, &format!("atproto {SIGN_CREATE}")),
        None,
        "alice.test",
    )
    .await
    .expect("login");
    assert_eq!(login.token.sub.as_str(), alice);
    let bodies = net.devnet.par_requests().await;
    let scope = util::first(bodies.last().expect("a PAR"), "scope").map(str::to_owned);
    assert_eq!(
        scope.as_deref(),
        Some(format!("atproto {SIGN_CREATE}").as_str())
    );

    let token = login.token.access_token.to_string();
    let mut dpop = login.client_data();
    let (status, body) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.applyWrites",
        json!({"repo": alice, "writes": [create(COLLECTION, "3m2abcdefgh22", signature_value())]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["results"][0]["$type"],
        "com.atproto.repo.applyWrites#createResult"
    );

    let (status, body) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.deleteRecord",
        json!({"repo": alice, "collection": COLLECTION, "rkey": "3m2abcdefgh22"}),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let (status, _) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.applyWrites",
        json!({"repo": alice, "writes": [create("app.bsky.feed.post", "3m2abcdefgh23", post("hi"))]}),
    )
    .await;
    assert_eq!(status, 403);
    assert!(matches!(
        prove(&net, &alice, COLLECTION, "3m2abcdefgh22").await,
        ProofOutcome::Present { .. }
    ));

    // The session cannot write another person's repository.
    let bob = net.devnet.did("bob.test").await.expect("bob");
    let (status, _) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.applyWrites",
        json!({"repo": bob, "writes": [create(COLLECTION, "3m2abcdefgh24", signature_value())]}),
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn a_create_on_an_existing_key_fails_the_whole_batch() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let existing = net
        .devnet
        .write_record(
            "alice.test",
            COLLECTION,
            Some("3m2aaaaaaaa22"),
            signature_value(),
            None,
        )
        .await
        .expect("write");
    let token = app_password_token(&net, "alice.test").await;
    let response = bearer_post(
        &net,
        &token,
        "com.atproto.repo.applyWrites",
        json!({"repo": alice, "writes": [
            create(COLLECTION, "3m2bbbbbbbb22", signature_value()),
            create(COLLECTION, &existing.rkey, signature_value()),
        ]}),
    )
    .await
    .expect("send");
    assert_eq!(response.status(), 500);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["error"], "InternalServerError");
    let lookup = net
        .http
        .get(format!(
            "{}/xrpc/com.atproto.repo.getRecord?repo={alice}&collection={COLLECTION}&rkey=3m2bbbbbbbb22",
            net.devnet.url()
        ))
        .send()
        .await
        .expect("get");
    assert_eq!(lookup.status(), 400);
    let state = net.devnet.shared.state.lock().await;
    let repo = &state.repos[&(0, alice.clone())];
    assert_eq!(repo.rev(), existing.rev, "nothing was committed");
    drop(state);

    // More than 200 writes are refused.
    let writes: Vec<Value> = (0..201)
        .map(|i| create("app.bsky.feed.post", &format!("p{i}"), post("x")))
        .collect();
    let response = bearer_post(
        &net,
        &token,
        "com.atproto.repo.applyWrites",
        json!({"repo": alice, "writes": writes}),
    )
    .await
    .expect("send");
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn the_rate_limit_switch_answers_429_with_a_reset() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let token = app_password_token(&net, "alice.test").await;
    net.devnet
        .set_switches(|s| {
            s.rate_limit = Some(RateLimit {
                points: 5,
                window_secs: 3600,
            })
        })
        .await;
    let write = |rkey: &str| json!({"repo": alice, "writes": [create(COLLECTION, rkey, signature_value())]});
    let first = bearer_post(
        &net,
        &token,
        "com.atproto.repo.applyWrites",
        write("3m2ccccccccc2"),
    )
    .await
    .expect("send");
    assert_eq!(first.status(), 200);
    let second = bearer_post(
        &net,
        &token,
        "com.atproto.repo.applyWrites",
        write("3m2ccccccccd2"),
    )
    .await
    .expect("send");
    assert_eq!(second.status(), 429);
    let reset: u64 = second.headers()["ratelimit-reset"]
        .to_str()
        .expect("header")
        .parse()
        .expect("seconds");
    assert!(reset * 1000 > util::now_ms());
}

#[tokio::test]
async fn a_public_client_uses_any_loopback_port_and_rotates_refresh_tokens() {
    let net = net().await;
    let mut login = login(
        &net,
        tt_client(&net, 54321, &format!("atproto {SIGN_CREATE}")),
        None,
        "bob.test",
    )
    .await
    .expect("login");
    assert_eq!(
        login.location.as_str().split('?').next(),
        Some("http://127.0.0.1:54321/callback")
    );
    let first = login
        .token
        .refresh_token
        .clone()
        .expect("a refresh token")
        .to_string();

    let refreshed = refresh_with(&net, &mut login, &first)
        .await
        .expect("refresh");
    let second = refreshed.refresh_token.expect("rotated").to_string();
    assert_ne!(first, second);
    assert_eq!(
        refreshed.sub.as_deref(),
        Some(net.devnet.did("bob.test").await.expect("bob").as_str())
    );

    // The used token is refused, and presenting it ends the session.
    let replay = refresh_with(&net, &mut login, &first)
        .await
        .expect_err("replay");
    assert!(replay.contains("invalid_grant"), "{replay}");
    let after = refresh_with(&net, &mut login, &second)
        .await
        .expect_err("revoked");
    assert!(after.contains("invalid_grant"), "{after}");

    // The revocation endpoint ends a session.
    let mut revoked = login_as_carol(&net, tt_client(&net, 2, "atproto")).await;
    let refresh = revoked
        .token
        .refresh_token
        .clone()
        .expect("refresh")
        .to_string();
    revoke(
        &net.resolver,
        &mut revoked.dpop,
        &refresh,
        &revoked.metadata,
    )
    .await
    .expect("revoke");
    let error = refresh_with(&net, &mut revoked, &refresh)
        .await
        .expect_err("revoked");
    assert!(error.contains("invalid_grant"), "{error}");

    // The token endpoint's error switch.
    net.devnet
        .set_switches(|s| s.token_error = Some("invalid_client".into()))
        .await;
    let error = refused_login(&net, tt_client(&net, 3, "atproto"), None).await;
    assert!(error.contains("invalid_client"), "{error}");
    net.devnet.set_switches(|s| s.token_error = None).await;

    // Another port and [::1] work too.
    let mut ipv6 = tt_client(&net, 1, "atproto");
    ipv6["redirect_uris"] = json!(["http://[::1]:61000/callback"]);
    let other = login_as_carol(&net, ipv6).await;
    assert_eq!(
        other.location.as_str().split('?').next(),
        Some("http://[::1]:61000/callback")
    );
}

async fn login_as_carol(net: &Net, client: Value) -> Login {
    login(net, client, None, "carol.test").await.expect("login")
}

#[tokio::test]
async fn redirects_scopes_and_pkce_are_checked() {
    let net = net().await;
    let mut evil = tt_client(&net, 1, "atproto");
    evil["redirect_uris"] = json!(["http://evil.example/callback"]);
    let error = login(&net, evil, None, "alice.test")
        .await
        .expect_err("refused");
    assert!(error.contains("invalid_redirect_uri"), "{error}");

    let undeclared = login(
        &net,
        tt_client(&net, 1, "atproto transition:generic"),
        None,
        "alice.test",
    )
    .await
    .expect_err("refused");
    assert!(undeclared.contains("invalid_scope"), "{undeclared}");

    net.devnet
        .set_switches(|s| s.par_error = Some("invalid_scope".into()))
        .await;
    let refused = login(&net, tt_client(&net, 1, "atproto"), None, "alice.test")
        .await
        .expect_err("refused");
    assert!(refused.contains("invalid_scope"), "{refused}");
    net.devnet
        .set_switches(|s| {
            s.par_error = None;
            s.narrow_scope = Some("atproto".into());
        })
        .await;
    let narrowed = login(
        &net,
        tt_client(&net, 1, &format!("atproto {SIGN_CREATE}")),
        None,
        "alice.test",
    )
    .await
    .expect("login");
    assert_eq!(narrowed.token.scope.as_deref(), Some("atproto"));

    // A wrong PKCE verifier.
    let mut metadata = oauth_metadata(&net, tt_client(&net, 1, "atproto"), None).await;
    let mut request = par(
        &net.resolver,
        Some(SmolStr::from("alice.test")),
        None,
        &mut metadata,
        None,
    )
    .await
    .expect("par");
    let location = authorize(&net, &metadata, &request.request_uri, "").await;
    let code = location
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .expect("code");
    let wrong = "x".repeat(43);
    let error = exchange_code(
        &net.resolver,
        &mut request.dpop_data,
        &code,
        &wrong,
        &metadata,
    )
    .await
    .expect_err("refused");
    assert!(format!("{error:?}").contains("invalid_grant"), "{error:?}");

    // A denied request redirects with access_denied.
    let request = par(
        &net.resolver,
        Some(SmolStr::from("alice.test")),
        None,
        &mut metadata,
        None,
    )
    .await
    .expect("par");
    let denied = authorize(&net, &metadata, &request.request_uri, "&deny=1").await;
    assert!(
        denied
            .query_pairs()
            .any(|(k, v)| k == "error" && v == "access_denied")
    );
}

#[tokio::test]
async fn stale_nonces_are_refused_and_then_retried() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let mut login = login(
        &net,
        tt_client(&net, 1, &format!("atproto {SIGN_CREATE}")),
        None,
        "alice.test",
    )
    .await
    .expect("login");
    let token = login.token.access_token.to_string();
    let mut dpop = login.client_data();
    let record = json!({"repo": alice, "collection": COLLECTION, "record": signature_value()});
    let (status, _) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.createRecord",
        record.clone(),
    )
    .await;
    assert_eq!(status, 200);
    let (old_as, old_rs) = {
        let state = net.devnet.shared.state.lock().await;
        (state.oauth.as_nonce.clone(), state.oauth.rs_nonce.clone())
    };
    assert_eq!(dpop.dpop_host_nonce.as_str(), old_rs);

    net.devnet.rotate_nonces().await;

    // A proof carrying a stale nonce is refused with a fresh one.
    let key = p256::ecdsa::SigningKey::from_slice(&[5u8; 32]).expect("key");
    let proof = |method: &str, url: &str, nonce: &str, ath: Option<String>| {
        let mut claims = json!({
            "htm": method, "htu": url, "iat": util::now_ms() / 1000,
            "jti": util::random_token(8), "nonce": nonce,
        });
        if let Some(ath) = ath {
            claims["ath"] = json!(ath);
        }
        crate::jwt::testing::sign(
            &key,
            &json!({"typ": "dpop+jwt", "alg": "ES256", "jwk": crate::jwt::testing::public_jwk(&key)}),
            &claims,
        )
    };
    let token_url = format!("{}/oauth/token", net.devnet.url());
    let response = net
        .http
        .post(&token_url)
        .header("DPoP", proof("POST", &token_url, &old_as, None))
        .form(&[("grant_type", "refresh_token"), ("refresh_token", "x")])
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 400);
    let fresh = response.headers()["dpop-nonce"]
        .to_str()
        .expect("nonce")
        .to_owned();
    assert_ne!(fresh, old_as);
    assert_eq!(
        response.json::<Value>().await.expect("json")["error"],
        "use_dpop_nonce"
    );

    let write_url = format!("{}/xrpc/com.atproto.repo.createRecord", net.devnet.url());
    let response = net
        .http
        .post(&write_url)
        .header("Authorization", format!("DPoP {token}"))
        .header(
            "DPoP",
            proof("POST", &write_url, &old_rs, Some(crate::jwt::ath(&token))),
        )
        .json(&record)
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), 401);
    assert!(
        response.headers()["www-authenticate"]
            .to_str()
            .expect("header")
            .contains("use_dpop_nonce")
    );
    assert_ne!(
        response.headers()["dpop-nonce"].to_str().expect("nonce"),
        old_rs
    );

    // jacquard's clients retry with the new nonces and succeed.
    let (status, body) = pds_post(
        &net,
        &token,
        &mut dpop,
        "com.atproto.repo.createRecord",
        record,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let refresh = login
        .token
        .refresh_token
        .clone()
        .expect("refresh")
        .to_string();
    login.dpop.dpop_authserver_nonce = Some(SmolStr::from(old_as.as_str()));
    refresh_with(&net, &mut login, &refresh)
        .await
        .expect("refresh after a stale AS nonce");
}

#[tokio::test]
async fn a_confidential_client_authenticates_with_private_key_jwt() {
    let net = net().await;
    let client = web_metadata(&net.clients);
    let login = login(
        &net,
        client.clone(),
        Some(keyset(&net.web_key)),
        "alice.test",
    )
    .await
    .expect("login");
    assert_eq!(login.token.scope.as_deref(), Some("atproto"));
    assert!(login.token.refresh_token.is_none());
    assert_eq!(
        login.location.as_str().split('?').next(),
        Some(format!("{}/-/auth/callback", net.clients).as_str())
    );

    // A key that is not in the client's JWKS is refused.
    let other = p256::SecretKey::from_slice(&[10u8; 32]).expect("key");
    let error = refused_login(&net, client.clone(), Some(keyset(&other))).await;
    assert!(error.contains("invalid_client"), "{error}");

    // The client's metadata cannot be fetched (the forge is down).
    let client_id = client["client_id"].as_str().expect("id").to_owned();
    net.devnet
        .set_switches(|s| s.fail_client_metadata = vec![client_id])
        .await;
    let error = refused_login(&net, client, Some(keyset(&net.web_key))).await;
    assert!(error.contains("invalid_client_metadata"), "{error}");
}

async fn refused_login(net: &Net, client: Value, keyset: Option<Keyset>) -> String {
    login(net, client, keyset, "alice.test")
        .await
        .expect_err("refused")
}

#[tokio::test]
async fn the_localhost_development_client_is_accepted() {
    let net = net().await;
    let client = json!({
        "client_id": "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcallback&scope=atproto",
        "application_type": "native",
        "redirect_uris": ["http://127.0.0.1:4321/callback"],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": "atproto",
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    });
    let login = login(&net, client, None, "alice.test")
        .await
        .expect("login");
    assert_eq!(
        login.location.as_str().split('?').next(),
        Some("http://127.0.0.1:4321/callback")
    );
    assert_eq!(
        login.token.sub.as_str(),
        net.devnet.did("alice.test").await.expect("alice")
    );
}

#[tokio::test]
async fn sessions_tokens_and_metadata_controls() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    net.devnet.set_switches(|s| s.access_token_secs = 1).await;
    let client = tt_client(&net, 1, &format!("atproto {SIGN_CREATE}"));
    let mut login = login(&net, client.clone(), None, "alice.test")
        .await
        .expect("login");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let mut dpop = login.client_data();
    let (status, _) = pds_post(
        &net,
        login.token.access_token.as_str(),
        &mut dpop,
        "com.atproto.repo.createRecord",
        json!({"repo": alice, "collection": COLLECTION, "record": signature_value()}),
    )
    .await;
    assert_eq!(status, 401, "an expired access token is refused");

    // tt's client metadata cannot be fetched: refreshes answer invalid_client_metadata.
    let refresh = login
        .token
        .refresh_token
        .clone()
        .expect("refresh")
        .to_string();
    net.devnet
        .set_switches(|s| s.fail_client_metadata = vec!["*".into()])
        .await;
    let error = refresh_with(&net, &mut login, &refresh)
        .await
        .expect_err("refused");
    assert!(error.contains("invalid_client_metadata"), "{error}");
    net.devnet
        .set_switches(|s| s.fail_client_metadata.clear())
        .await;
    let refreshed = refresh_with(&net, &mut login, &refresh)
        .await
        .expect("refresh");
    assert_eq!(refreshed.expires_in, Some(1));

    // Revoking the person's sessions, as their app-session settings do.
    let revoked = net
        .devnet
        .revoke_sessions(
            "alice.test",
            Some(client["client_id"].as_str().expect("id")),
        )
        .await
        .expect("revoke");
    assert_eq!(revoked, 1);
    let error = refresh_with(
        &net,
        &mut login,
        refreshed.refresh_token.as_deref().expect("token"),
    )
    .await
    .expect_err("revoked");
    assert!(error.contains("invalid_grant"), "{error}");

    // The public-client session lifetime.
    net.devnet
        .set_switches(|s| {
            s.access_token_secs = 3600;
            s.public_session_secs = 0;
        })
        .await;
    let mut short = login_as_carol(&net, client).await;
    let token = short
        .token
        .refresh_token
        .clone()
        .expect("refresh")
        .to_string();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let error = refresh_with(&net, &mut short, &token)
        .await
        .expect_err("expired");
    assert!(error.contains("invalid_grant"), "{error}");

    // App passwords write any collection.
    let bearer = app_password_token(&net, "alice.test").await;
    let response = bearer_post(
        &net,
        &bearer,
        "com.atproto.repo.createRecord",
        json!({"repo": alice, "collection": "app.bsky.feed.post", "record": post("hello")}),
    )
    .await
    .expect("send");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn account_controls_and_lost_answers() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let written = net
        .devnet
        .write_record("alice.test", COLLECTION, None, signature_value(), None)
        .await
        .expect("write");
    let repo_status = || async {
        net.http
            .get(format!(
                "{}/xrpc/com.atproto.sync.getRepoStatus?did={alice}",
                net.devnet.url()
            ))
            .send()
            .await
            .expect("send")
            .json::<Value>()
            .await
            .expect("json")
    };

    net.devnet
        .set_status("alice.test", "deactivated")
        .await
        .expect("status");
    assert_eq!(
        repo_status().await,
        json!({"did": alice, "active": false, "status": "deactivated", "rev": written.rev})
    );
    assert_eq!(
        record_answer(&net, &alice, &written.rkey, net.devnet.url()).await,
        RecordAnswer::RepoUnavailable("RepoDeactivated".into())
    );
    net.devnet
        .set_status("alice.test", "throttled")
        .await
        .expect("status");
    assert!(matches!(
        record_answer(&net, &alice, &written.rkey, net.devnet.url()).await,
        RecordAnswer::Car(_)
    ));
    assert!(
        net.devnet
            .set_status("alice.test", "everyone")
            .await
            .is_err()
    );
    net.devnet
        .set_status("alice.test", "active")
        .await
        .expect("status");
    assert_eq!(repo_status().await["active"], true);

    // The PDS offline and back.
    net.devnet.set_switches(|s| s.pds_offline = true).await;
    assert!(matches!(
        record_answer(&net, &alice, &written.rkey, net.devnet.url()).await,
        RecordAnswer::Unavailable(_)
    ));
    net.devnet.set_switches(|s| s.pds_offline = false).await;
    assert!(matches!(
        record_answer(&net, &alice, &written.rkey, net.devnet.url()).await,
        RecordAnswer::Car(_)
    ));

    // The PLC directory offline.
    net.devnet.set_switches(|s| s.plc_offline = true).await;
    let did = Did::parse(&alice, Mode::Development).expect("did");
    assert!(
        net.resolver
            .resolve_did(&did)
            .await
            .expect_err("offline")
            .is_transient()
    );
    net.devnet.set_switches(|s| s.plc_offline = false).await;

    // A write committed whose answer is lost.
    let bearer = app_password_token(&net, "alice.test").await;
    net.devnet.set_switches(|s| s.drop_next_write = true).await;
    let lost = bearer_post(
        &net,
        &bearer,
        "com.atproto.repo.applyWrites",
        json!({"repo": alice, "writes": [create(COLLECTION, "3m2lostlost22", signature_value())]}),
    )
    .await;
    assert!(lost.is_err(), "{lost:?}");
    assert!(matches!(
        prove(&net, &alice, COLLECTION, "3m2lostlost22").await,
        ProofOutcome::Present { .. }
    ));
    assert!(!net.devnet.switches().await.drop_next_write);

    // getRepoStatus can be slowed down.
    net.devnet
        .set_switches(|s| s.repo_status_delay_ms = 300)
        .await;
    let started = std::time::Instant::now();
    repo_status().await;
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[tokio::test]
async fn identity_controls_follow_the_plc_log() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let did = Did::parse(&alice, Mode::Development).expect("did");
    let written = net
        .devnet
        .write_record("alice.test", COLLECTION, None, signature_value(), None)
        .await
        .expect("write");
    let resolve = || async {
        net.resolver
            .resolve_did(&did)
            .await
            .expect("resolve")
            .identity
    };
    let before = resolve().await.expect("identity");

    // A rotation the PDS has not caught up with for one answer.
    net.devnet
        .rotate_key("alice.test", 1)
        .await
        .expect("rotate");
    let after = resolve().await.expect("identity");
    assert_ne!(before.signing_key, after.signing_key);
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Invalid(_)
    ));
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Present { .. }
    ));

    // A migration whose identity event is withheld: the old host keeps its own copy under the
    // old key.
    net.devnet
        .migrate("alice.test", true)
        .await
        .expect("migrate");
    let migrated = resolve().await.expect("identity");
    assert_eq!(
        migrated.pds.as_deref(),
        Some(net.devnet.migration_url().await.as_str())
    );
    assert_ne!(migrated.signing_key, after.signing_key);
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Present { .. }
    ));
    net.devnet
        .delete_record("alice.test", COLLECTION, &written.rkey, Some(0))
        .await
        .expect("delete from the old copy");
    match record_answer(&net, &alice, &written.rkey, net.devnet.url()).await {
        RecordAnswer::Car(car) => {
            let record = RecordRef {
                did: did.clone(),
                collection: COLLECTION.into(),
                rkey: written.rkey.clone(),
            };
            let old_key = after.signing_key.clone().expect("key");
            assert!(matches!(
                prove_record(car, record, old_key, util::now_ms()).await,
                ProofOutcome::Absent { .. }
            ));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        prove(&net, &alice, COLLECTION, &written.rkey).await,
        ProofOutcome::Present { .. }
    ));
    let identity_events = net
        .devnet
        .shared
        .state
        .lock()
        .await
        .events
        .iter()
        .filter(|e| e.kind == "identity" && e.did == alice)
        .count();
    assert_eq!(identity_events, 1, "only the rotation was announced");

    // A handle change.
    net.devnet
        .set_handle("alice.test", "alicia.test")
        .await
        .expect("handle");
    assert_eq!(
        net.resolver
            .resolve_verified_handle(&handle("alicia.test"))
            .await
            .expect("resolves")
            .as_str(),
        alice
    );
    assert!(
        net.resolver
            .resolve_verified_handle(&handle("alice.test"))
            .await
            .is_err()
    );

    // A tombstone.
    net.devnet
        .tombstone("alicia.test")
        .await
        .expect("tombstone");
    assert_eq!(resolve().await, None);
}

async fn next_message<S>(socket: &mut S) -> Option<Value>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match tokio::time::timeout(Duration::from_millis(500), socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str(&text).ok(),
        _ => None,
    }
}

#[tokio::test]
async fn jetstream_filters_replace_each_other() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    let url = format!(
        "{}?wantedCollections={COLLECTION}&requireHello=true",
        net.devnet.jetstream_url()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("connect");
    let write = |who: &'static str, collection: &'static str, value: Value| {
        let devnet = &net.devnet;
        async move {
            devnet
                .write_record(who, collection, None, value, None)
                .await
                .expect("write")
        }
    };

    // Nothing is sent before the hello.
    write("alice.test", COLLECTION, signature_value()).await;
    assert_eq!(next_message(&mut socket).await, None);

    let update = |payload: Value| {
        Message::text(json!({"type": "options_update", "payload": payload}).to_string())
    };
    socket
        .send(update(
            json!({"wantedCollections": [COLLECTION], "wantedDids": [alice]}),
        ))
        .await
        .expect("send");
    tokio::time::sleep(Duration::from_millis(100)).await;
    write("alice.test", "app.bsky.feed.post", post("ignored")).await;
    write("bob.test", COLLECTION, signature_value()).await;
    let signed = write("alice.test", COLLECTION, signature_value()).await;
    let event = next_message(&mut socket).await.expect("an event");
    assert_eq!(event["kind"], "commit");
    assert_eq!(event["commit"]["rkey"], signed.rkey.as_str());
    assert_eq!(event["commit"]["operation"], "create");
    assert_eq!(event["commit"]["cid"], signed.cid.as_str());
    assert_eq!(next_message(&mut socket).await, None);

    // Updates.
    let replaced = net
        .devnet
        .replace_record(
            "alice.test",
            COLLECTION,
            &signed.rkey,
            signature_value_with([4u8; 32]),
            None,
        )
        .await
        .expect("replace");
    let event = next_message(&mut socket).await.expect("update");
    assert_eq!(event["commit"]["operation"], "update");
    assert_eq!(event["commit"]["cid"], replaced.cid.as_str());
    assert_ne!(replaced.cid, signed.cid);

    // Deletes, and account and identity events, which no collection filter hides.
    net.devnet
        .delete_record("alice.test", COLLECTION, &signed.rkey, None)
        .await
        .expect("delete");
    let event = next_message(&mut socket).await.expect("delete");
    assert_eq!(event["commit"]["operation"], "delete");
    net.devnet
        .set_status("alice.test", "takendown")
        .await
        .expect("status");
    let event = next_message(&mut socket).await.expect("account");
    assert_eq!(event["kind"], "account");
    assert_eq!(event["account"]["active"], false);
    assert_eq!(event["account"]["status"], "takendown");
    net.devnet
        .emit_jetstream(&alice, "account", json!({"active": true, "did": alice}))
        .await
        .expect("emit");
    let forged = next_message(&mut socket).await.expect("forged");
    assert_eq!(forged["account"]["active"], true);

    // An update carrying only wantedDids clears the collection filter.
    socket
        .send(update(json!({"wantedDids": [alice]})))
        .await
        .expect("send");
    tokio::time::sleep(Duration::from_millis(100)).await;
    write("alice.test", "app.bsky.feed.post", post("seen")).await;
    let event = next_message(&mut socket).await.expect("a post");
    assert_eq!(event["commit"]["collection"], "app.bsky.feed.post");

    // An update of 10,000 DIDs is accepted, and its collection filter is in force.
    let mut dids: Vec<String> = (0..9_999).map(|i| format!("did:plc:{i:024}")).collect();
    dids.push(alice.clone());
    socket
        .send(update(
            json!({"wantedCollections": [COLLECTION], "wantedDids": dids}),
        ))
        .await
        .expect("send");
    tokio::time::sleep(Duration::from_millis(200)).await;
    write("alice.test", "app.bsky.feed.post", post("hidden")).await;
    let signed = write("alice.test", COLLECTION, signature_value()).await;
    assert_eq!(
        next_message(&mut socket).await.expect("signature")["commit"]["rkey"],
        signed.rkey.as_str()
    );

    // Identity events.
    net.devnet
        .set_handle("alice.test", "alicia.test")
        .await
        .expect("handle");
    let event = next_message(&mut socket).await.expect("identity");
    assert_eq!(event["identity"]["handle"], "alicia.test");

    // More than 10,000 closes the connection.
    dids.push("did:plc:onetoomanyonetoomanyonet".into());
    socket
        .send(update(json!({"wantedDids": dids})))
        .await
        .expect("send");
    let closed = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("answer");
    assert!(matches!(
        closed,
        Some(Ok(Message::Close(_))) | None | Some(Err(_))
    ));

    // A cursor replays the log.
    let cursor = net
        .devnet
        .shared
        .state
        .lock()
        .await
        .events
        .first()
        .expect("events")
        .time_us;
    let url = format!(
        "{}?wantedDids={alice}&cursor={cursor}",
        net.devnet.jetstream_url()
    );
    let (mut replay, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .expect("connect");
    assert_eq!(
        next_message(&mut replay).await.expect("first")["time_us"],
        cursor
    );
}

#[tokio::test]
async fn the_tarpit_holds_xrpc_requests_and_counts_them() {
    let net = net().await;
    let info = net.devnet.start_tarpit(3).await.expect("tarpit");
    assert_eq!(info.dids.len(), 3);
    let did = Did::parse(&info.dids[1], Mode::Development).expect("did:web");
    let identity = net
        .resolver
        .resolve_did(&did)
        .await
        .expect("resolve")
        .identity
        .expect("identity");
    let host = format!("t1.localhost:{}", info.port);
    assert_eq!(
        identity.pds.as_deref(),
        Some(format!("http://{host}").as_str())
    );

    let client = reqwest::Client::builder()
        .no_proxy()
        .resolve("t1.localhost", ([127, 0, 0, 1], info.port).into())
        .timeout(Duration::from_millis(300))
        .build()
        .expect("client");
    let held = client
        .get(format!(
            "http://{host}/xrpc/com.atproto.sync.getRecord?did={did}"
        ))
        .send()
        .await;
    assert!(held.is_err_and(|error| error.is_timeout()));
    let counts = net.devnet.tarpit_counts().await;
    let seen = &counts[&host];
    assert_eq!(seen.requests, 2);
    assert_eq!(seen.xrpc_requests, 1);
    assert_eq!(seen.connections, 2);
    assert!(!counts.contains_key(&format!("t0.localhost:{}", info.port)));
}

#[tokio::test]
async fn records_are_listed_newest_first_with_a_cursor() {
    let net = net().await;
    let alice = net.devnet.did("alice.test").await.expect("alice");
    for rkey in ["3m2aaaaaaaa22", "3m2bbbbbbbb22", "3m2cccccccc22"] {
        net.devnet
            .write_record(
                "alice.test",
                COLLECTION,
                Some(rkey),
                signature_value(),
                None,
            )
            .await
            .expect("write");
    }
    net.devnet
        .write_record("alice.test", "app.bsky.feed.post", None, post("x"), None)
        .await
        .expect("write");
    let list = |query: String| {
        let http = net.http.clone();
        let url = format!(
            "{}/xrpc/com.atproto.repo.listRecords?repo=alice.test&collection={COLLECTION}{query}",
            net.devnet.url()
        );
        async move {
            http.get(url)
                .send()
                .await
                .expect("send")
                .json::<Value>()
                .await
                .expect("json")
        }
    };
    let page = list("&limit=2".into()).await;
    let uris: Vec<&str> = page["records"]
        .as_array()
        .expect("records")
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    assert_eq!(
        uris,
        [
            format!("at://{alice}/{COLLECTION}/3m2cccccccc22"),
            format!("at://{alice}/{COLLECTION}/3m2bbbbbbbb22")
        ]
    );
    let rest = list(format!(
        "&limit=2&cursor={}",
        page["cursor"].as_str().expect("cursor")
    ))
    .await;
    assert_eq!(rest["records"].as_array().map(Vec::len), Some(1));
    assert!(rest.get("cursor").is_none());
    let ascending = list("&reverse=true".into()).await;
    assert_eq!(
        ascending["records"][0]["uri"],
        format!("at://{alice}/{COLLECTION}/3m2aaaaaaaa22")
    );
}

#[tokio::test]
async fn controls_over_http() {
    let net = net().await;
    let salt = Salt::from_bytes([1u8; 32]);
    let base = net.devnet.url();
    let written: RecordInfo = net
        .http
        .post(format!("{base}/devnet/signatures"))
        .json(&json!({
            "who": "bob.test",
            "payload": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"payload"),
            "salt": salt.to_base64url(),
        }))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    let bob = net.devnet.did("bob.test").await.expect("bob");
    match prove(&net, &bob, COLLECTION, &written.rkey).await {
        ProofOutcome::Present { record, .. } => SignatureRecord::decode(&record)
            .expect("record")
            .check(b"payload", &salt)
            .expect("signs"),
        other => panic!("{other:?}"),
    }
    let switches: Switches = net
        .http
        .post(format!("{base}/devnet/switches"))
        .json(&json!({"pds_offline": false, "rate_limit": {"points": 9, "window_secs": 60}}))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(
        switches.rate_limit,
        Some(RateLimit {
            points: 9,
            window_secs: 60
        })
    );
    let refused = net
        .http
        .post(format!("{base}/devnet/switches"))
        .json(&json!({"no_such_switch": true}))
        .send()
        .await
        .expect("send");
    assert_eq!(refused.status(), 400);
    let people: Vec<PersonInfo> = net
        .http
        .get(format!("{base}/devnet/people"))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(people.len(), SEEDED_HANDLES.len());
    let unknown = net
        .http
        .post(format!("{base}/devnet/status"))
        .json(&json!({"who": "nobody.test", "status": "active"}))
        .send()
        .await
        .expect("send");
    assert_eq!(unknown.status(), 400);
}
