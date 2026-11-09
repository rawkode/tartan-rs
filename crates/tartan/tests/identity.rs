//! The identity actor with the real Rivet engine: people and account status, sessions, access
//! tokens and the `tt login` hand-off (design D3, D5, D6, D10, D16).
//!
//! Each person is a `did:web:localhost%3A<port>` DID served by a small in-process host that
//! answers its DID document and `com.atproto.sync.getRepoStatus`, so account status can be
//! changed per test. Time-based windows run against the harness's [`TestClock`], advanced and
//! followed by a direct `Sweep`, as the scheduled wake would run.
//!
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rivetkit::client::{Client, ClientConfig};
use rivetkit::{EngineSpawnMode, Registry, ServeConfig, TypedActorHandle};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tartan_atproto::Mode;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tartan_atproto::resolver::{ResolverConfig, VerifyingResolver};
use tartan_config::deployment::Readers;
use tartan_forge::{
    Envelope, EventPattern, Extensions, Identity, ReadEvents, Received, RegisterSubscriber,
    Services, SubscriberSpec, forge_handle, identity_handle, install_services, register_actors,
    subscriber_handle,
};
use tartan_identity_api::{
    AdminCreateToken, Authenticate, CliRefusal, CreateSession, CreateToken, Credential,
    CredentialKind, EndSession, IssueCliCode, KnowDid, KnownAs, ListSessions, ListTokens,
    MAX_CLI_CODES_PER_DID, MAX_TOKENS_PER_DID, People, Person, RedeemCliCode, RefreshIdentity,
    RevokeToken, SWEEP_SCHEDULE, Schedules, SessionKind, SignedIn, Sweep, TokenRefusal,
    TokenRequest, TokenScope,
};
use tokio_util::sync::CancellationToken;

use common::{FakeGitHost, TestClock};

/// Where `rivetkit::test::setup` runs the engine.
const ENGINE: &str = "http://127.0.0.1:6420";
const DEADLINE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const DAY: Duration = Duration::from_secs(24 * 3600);

// A valid secp256k1 Multikey; the hosts serve it as every person's #atproto key.
const KEY: &str = "zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ";

/// What a host's PDS says about its account.
#[derive(Clone, Debug)]
enum Account {
    /// `getRepoStatus` answers `{active: true}`.
    Active,
    /// `getRepoStatus` answers `{active: false, status}`.
    Inactive(&'static str),
    /// `getRepoStatus` answers 400 `RepoNotFound`.
    NotFound,
    /// The DID document names a PDS nothing listens on.
    Unreachable,
}

struct HostState {
    port: u16,
    account: Mutex<Account>,
    status_requests: AtomicUsize,
}

/// One person: a did:web host that is also their PDS.
#[derive(Clone)]
struct Host {
    did: String,
    state: Arc<HostState>,
}

impl Host {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a host");
        let port = listener.local_addr().expect("address").port();
        let state = Arc::new(HostState {
            port,
            account: Mutex::new(Account::Active),
            status_requests: AtomicUsize::new(0),
        });
        let app = Router::new()
            .route("/.well-known/did.json", get(did_document))
            .route("/xrpc/com.atproto.sync.getRepoStatus", get(repo_status))
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            did: format!("did:web:localhost%3A{port}"),
            state,
        }
    }

    fn set(&self, account: Account) {
        *self.state.account.lock().expect("never poisoned") = account;
    }

    fn status_requests(&self) -> usize {
        self.state.status_requests.load(Ordering::SeqCst)
    }
}

async fn did_document(State(state): State<Arc<HostState>>) -> Json<Value> {
    let did = format!("did:web:localhost%3A{}", state.port);
    let pds = match *state.account.lock().expect("never poisoned") {
        Account::Unreachable => "http://localhost:1".to_owned(),
        _ => format!("http://localhost:{}", state.port),
    };
    Json(json!({
        "id": did,
        "alsoKnownAs": [format!("at://p{}.test", state.port)],
        "verificationMethod": [{
            "id": format!("{did}#atproto"),
            "type": "Multikey",
            "controller": did,
            "publicKeyMultibase": KEY,
        }],
        "service": [{
            "id": "#atproto_pds",
            "type": "AtprotoPersonalDataServer",
            "serviceEndpoint": pds,
        }],
    }))
}

async fn repo_status(State(state): State<Arc<HostState>>) -> Response {
    state.status_requests.fetch_add(1, Ordering::SeqCst);
    let did = format!("did:web:localhost%3A{}", state.port);
    let account = state.account.lock().expect("never poisoned").clone();
    match account {
        Account::Active | Account::Unreachable => {
            Json(json!({"did": did, "active": true, "rev": "3l"})).into_response()
        }
        Account::Inactive(status) => {
            Json(json!({"did": did, "active": false, "status": status})).into_response()
        }
        Account::NotFound => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "RepoNotFound", "message": "no such repo"})),
        )
            .into_response(),
    }
}

/// Log lines, kept so the test can check no secret was ever logged.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("never poisoned")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("never poisoned")).into_owned()
    }
}

/// The actors, served in a pool of their own so the test can restart them.
struct Runner {
    pool: String,
    shutdown: CancellationToken,
    serving: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Runner {
    fn start(pool: &str) -> Self {
        let mut registry = Registry::new();
        register_actors(&mut registry);
        let shutdown = CancellationToken::new();
        let config = ServeConfig {
            endpoint: ENGINE.to_owned(),
            token: Some("default".to_owned()),
            namespace: "default".to_owned(),
            pool_name: pool.to_owned(),
            engine_spawn: EngineSpawnMode::Never,
            ..ServeConfig::default()
        };
        let serving = tokio::spawn(registry.serve_with_config(config, shutdown.clone()));
        Self {
            pool: pool.to_owned(),
            shutdown,
            serving,
        }
    }

    async fn restart(self) -> Self {
        self.shutdown.cancel();
        let _ = self.serving.await;
        Self::start(&self.pool)
    }
}

fn sha256_hex(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn read_token(name: &str) -> TokenRequest {
    TokenRequest {
        name: name.to_owned(),
        scopes: vec![TokenScope::Read],
        repos: None,
        expires_in_days: 30,
    }
}

/// Retries `call` until it succeeds: the actor may be moving to a restarted runner.
async fn eventually<T, F, Fut>(what: &str, call: F) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let start = Instant::now();
    loop {
        match call().await {
            Ok(value) => return value,
            Err(error) => {
                assert!(start.elapsed() < DEADLINE, "{what}: {error:#}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

struct World {
    client: Client,
    clock: Arc<TestClock>,
    owner: Host,
    stranger: Host,
    signer: Host,
}

impl World {
    fn identity(&self) -> TypedActorHandle<Identity> {
        identity_handle(&self.client).expect("the identity handle")
    }

    async fn person(&self, did: &str) -> Option<Person> {
        let mut people = self
            .identity()
            .send(People {
                dids: vec![did.to_owned()],
            })
            .await
            .expect("people");
        people.pop().flatten()
    }

    async fn authenticate(&self, secret: &str, kind: CredentialKind) -> Option<String> {
        self.identity()
            .send(Authenticate {
                secret_sha256: sha256_hex(secret),
                kind,
            })
            .await
            .expect("authenticate")
            .map(|authenticated| authenticated.did)
    }

    async fn session(&self, did: &str, kind: SessionKind, device: &str) -> (String, String) {
        let session = self
            .identity()
            .send(CreateSession {
                did: did.to_owned(),
                kind,
                device: Some(device.to_owned()),
            })
            .await
            .expect("a session");
        (session.id, session.secret)
    }

    async fn events(&self) -> Vec<Envelope> {
        self.identity()
            .send(ReadEvents {
                after: 0,
                limit: 500,
            })
            .await
            .expect("events")
    }

    async fn sweep(&self) -> tartan_identity_api::SweepReport {
        self.identity().send(Sweep {}).await.expect("sweep")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identity_actor() {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(move || writer.clone())
        .try_init();
    tracing::info!("identity actor tests started");

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_nanos();
    let owner = Host::start().await;
    let stranger = Host::start().await;
    let signer = Host::start().await;
    let clock = Arc::new(TestClock::default());
    let store: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("a fetch client");
    // Nothing listens on port 1: every DID here is a did:web, so the PLC directory and handle
    // resolver are never asked.
    let resolver = VerifyingResolver::new(
        fetch,
        ResolverConfig::development("http://127.0.0.1:1", "http://127.0.0.1:1"),
    );
    install_services(Services {
        forge_id: format!("identity-{nanos}"),
        git_host: Arc::new(FakeGitHost::default()),
        extensions: Arc::new(Extensions::new(
            tartan_extensions::ExtensionRuntime::new().expect("runtime"),
            store,
        )),
        clock: clock.clone(),
        owners: vec![owner.did.clone()],
        readers: Readers::default(),
        resolver,
        identity_limits: tartan_forge::IdentityLimits::default(),
    })
    .expect("installed once");

    // Start (or reuse) the engine, then serve the actors in a pool this test can restart.
    let engine = rivetkit::test::setup(Registry::new())
        .await
        .expect("the engine");
    let pool = format!("identity-{nanos}");
    let mut runner = Runner::start(&pool);
    let client = Client::new(
        ClientConfig::new(ENGINE)
            .token("default")
            .namespace("default")
            .pool_name(&pool),
    );
    let world = World {
        client,
        clock,
        owner,
        stranger,
        signer,
    };

    owners_are_known_and_strangers_are_not(&world).await;
    runner = the_sweep_is_scheduled_once(&world, runner).await;
    let secrets = sessions(&world).await;
    let token_secrets = tokens(&world).await;
    let cli_secrets = cli_hand_off(&world).await;
    account_status(&world).await;
    known_dids_are_swept(&world).await;
    subscribers_get_identity_history(&world).await;
    secrets_never_reach_events_or_logs(
        &world,
        &logs,
        secrets
            .iter()
            .chain(&token_secrets)
            .chain(&cli_secrets)
            .cloned()
            .collect(),
    )
    .await;

    runner.shutdown.cancel();
    let _ = runner.serving.await;
    engine.shutdown().await;
}

async fn owners_are_known_and_strangers_are_not(world: &World) {
    // The actor knows its owners from its first start, and learns their status in the
    // background.
    let owner = &world.owner.did;
    let start = Instant::now();
    let person = loop {
        let person = eventually("people", || async {
            world
                .identity()
                .send(People {
                    dids: vec![owner.clone()],
                })
                .await
        })
        .await
        .pop()
        .flatten();
        match person {
            Some(person) if person.status_checked_at.is_some() => break person,
            other => {
                assert!(start.elapsed() < DEADLINE, "owner not refreshed: {other:?}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    assert_eq!(person.known_as, vec![KnownAs::Owner]);
    assert_eq!(person.status, "active");
    assert_eq!(
        person.pds,
        Some(format!("http://localhost:{}", world.owner.state.port))
    );
    assert!(
        person
            .atproto_key
            .is_some_and(|key| key.starts_with("did:key:"))
    );

    // A stranger who signs in to a private forge leaves nothing behind.
    let recorded = world
        .identity()
        .send(SignedIn {
            did: world.stranger.did.clone(),
            handle: Some("stranger.test".into()),
        })
        .await
        .expect("signed in");
    assert!(!recorded);
    assert_eq!(world.person(&world.stranger.did).await, None);

    // An owner's sign-in is recorded.
    let recorded = world
        .identity()
        .send(SignedIn {
            did: owner.clone(),
            handle: Some("owner.test".into()),
        })
        .await
        .expect("signed in");
    assert!(recorded);
    let signed_in: Vec<String> = world
        .events()
        .await
        .into_iter()
        .filter(|event| event.kind == "person.signed_in")
        .map(|event| event.data["did"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(signed_in, vec![owner.clone()]);
    let person = world.person(owner).await.expect("the owner");
    assert_eq!(person.known_as, vec![KnownAs::Owner, KnownAs::SignedIn]);
    assert_eq!(person.handle.as_deref(), Some("owner.test"));
}

async fn the_sweep_is_scheduled_once(world: &World, mut runner: Runner) -> Runner {
    for _ in 0..3 {
        runner = runner.restart().await;
        // Any call wakes the actor on the new runner, which registers the sweep again.
        let schedules = eventually("schedules", || async {
            world.identity().send(Schedules {}).await
        })
        .await;
        assert_eq!(schedules, vec![SWEEP_SCHEDULE.to_owned()]);
    }
    runner
}

/// Sessions of both kinds; answers every secret it saw.
async fn sessions(world: &World) -> Vec<String> {
    let owner = world.owner.did.as_str();
    let mut seen = Vec::new();

    let (browser_id, browser) = world.session(owner, SessionKind::Browser, "Firefox").await;
    assert_eq!(browser.len(), 43);
    let (cli_id, cli) = world.session(owner, SessionKind::Cli, "laptop").await;
    assert!(cli.starts_with("tartan_cli_") && cli.len() == 54, "{cli}");
    seen.extend([browser.clone(), cli.clone()]);

    // Each authenticates by its hash, and only as the kind it is.
    let browser_kind = CredentialKind::BrowserSession;
    let cli_kind = CredentialKind::CliSession;
    assert_eq!(
        world.authenticate(&browser, browser_kind).await,
        Some(owner.to_owned())
    );
    assert_eq!(
        world.authenticate(&cli, cli_kind).await,
        Some(owner.to_owned())
    );
    assert_eq!(world.authenticate(&browser, cli_kind).await, None);
    assert_eq!(world.authenticate(&cli, CredentialKind::Token).await, None);
    // The actor looks credentials up by their SHA-256 only: the secret itself, sent where a
    // hash belongs, matches nothing.
    let by_secret = world
        .identity()
        .send(Authenticate {
            secret_sha256: browser.clone(),
            kind: browser_kind,
        })
        .await
        .expect("authenticate");
    assert_eq!(by_secret, None);

    // The CLI session is listed with its device; no listing carries a secret.
    let listed = world
        .identity()
        .send(ListSessions {
            did: owner.to_owned(),
        })
        .await
        .expect("sessions");
    let cli_listed = listed
        .iter()
        .find(|session| session.id == cli_id)
        .expect("the CLI session is listed");
    assert_eq!(cli_listed.kind, SessionKind::Cli);
    assert_eq!(cli_listed.device.as_deref(), Some("laptop"));
    let listing = serde_json::to_string(&listed).expect("encode");
    assert!(!listing.contains(&browser) && !listing.contains(&cli));

    // Ending a session.
    let ended = world
        .identity()
        .send(EndSession {
            did: owner.to_owned(),
            id: browser_id.clone(),
        })
        .await
        .expect("end");
    assert!(ended);
    assert_eq!(world.authenticate(&browser, browser_kind).await, None);
    let listed = world
        .identity()
        .send(ListSessions {
            did: owner.to_owned(),
        })
        .await
        .expect("sessions");
    assert!(listed.iter().all(|session| session.id != browser_id));

    // Idle expiry: 8 days without use.
    let (_, idle) = world.session(owner, SessionKind::Browser, "idle").await;
    seen.push(idle.clone());
    // Absolute expiry: used every 6 days, it still ends 30 days after creation.
    let (_, busy) = world.session(owner, SessionKind::Browser, "busy").await;
    seen.push(busy.clone());
    world.clock.advance(6 * DAY);
    assert_eq!(
        world.authenticate(&busy, browser_kind).await,
        Some(owner.to_owned())
    );
    world.clock.advance(2 * DAY);
    assert_eq!(world.authenticate(&idle, browser_kind).await, None);
    assert_eq!(
        world.authenticate(&busy, browser_kind).await,
        Some(owner.to_owned())
    );
    for _ in 0..3 {
        world.clock.advance(6 * DAY);
        assert_eq!(
            world.authenticate(&busy, browser_kind).await,
            Some(owner.to_owned())
        );
    }
    // 26 days in; 30 days is the absolute limit.
    world.clock.advance(3 * DAY);
    assert_eq!(
        world.authenticate(&busy, browser_kind).await,
        Some(owner.to_owned())
    );
    world.clock.advance(DAY + HOUR);
    assert_eq!(world.authenticate(&busy, browser_kind).await, None);
    assert_eq!(world.authenticate(&cli, cli_kind).await, None);

    // The sweep deletes what expired.
    let report = world.sweep().await;
    assert!(report.expired >= 3, "{report:?}");
    seen
}

/// Access tokens; answers every secret it saw.
async fn tokens(world: &World) -> Vec<String> {
    let owner = world.owner.did.as_str();
    let mut seen = Vec::new();
    let (session_id, session) = world.session(owner, SessionKind::Browser, "Firefox").await;
    seen.push(session);

    let created = world
        .identity()
        .send(CreateToken {
            session_id: session_id.clone(),
            request: TokenRequest {
                repos: Some(vec!["acme/web".into(), "tools/*".into()]),
                ..read_token("ci")
            },
        })
        .await
        .expect("create")
        .expect("created");
    assert!(created.secret.starts_with("tartan_pat_") && created.secret.len() == 54);
    seen.push(created.secret.clone());
    let authenticated = world
        .identity()
        .send(Authenticate {
            secret_sha256: sha256_hex(&created.secret),
            kind: CredentialKind::Token,
        })
        .await
        .expect("authenticate")
        .expect("the token is accepted");
    assert_eq!(authenticated.did, owner);
    assert_eq!(
        authenticated.credential,
        Credential::Token {
            id: created.token.id.clone(),
            scopes: vec![TokenScope::Read],
            repos: Some(vec!["acme/web".into(), "tools/*".into()]),
        }
    );

    // Tokens are created only from a live session, never from a token.
    let from_token = world
        .identity()
        .send(CreateToken {
            session_id: created.token.id.clone(),
            request: read_token("from a token"),
        })
        .await
        .expect("create");
    assert_eq!(from_token, Err(TokenRefusal::NoSession));

    // Malformed requests are refused.
    let too_long = world
        .identity()
        .send(CreateToken {
            session_id: session_id.clone(),
            request: TokenRequest {
                expires_in_days: 400,
                ..read_token("long")
            },
        })
        .await
        .expect("create");
    assert!(
        matches!(too_long, Err(TokenRefusal::Invalid { .. })),
        "{too_long:?}"
    );

    // At most 50 live tokens per DID.
    for n in 1..MAX_TOKENS_PER_DID {
        let token = world
            .identity()
            .send(CreateToken {
                session_id: session_id.clone(),
                request: read_token(&format!("bulk {n}")),
            })
            .await
            .expect("create")
            .expect("under the cap");
        seen.push(token.secret);
    }
    let over = world
        .identity()
        .send(CreateToken {
            session_id: session_id.clone(),
            request: read_token("one too many"),
        })
        .await
        .expect("create");
    assert_eq!(over, Err(TokenRefusal::TooMany));

    // A stranger cannot revoke an owner's token, nor learn it exists; its DID can.
    let by_stranger = world
        .identity()
        .send(RevokeToken {
            by: world.stranger.did.clone(),
            id: created.token.id.clone(),
        })
        .await
        .expect("revoke");
    assert_eq!(by_stranger, Err(TokenRefusal::NotFound));
    world
        .identity()
        .send(RevokeToken {
            by: owner.to_owned(),
            id: created.token.id.clone(),
        })
        .await
        .expect("revoke")
        .expect("revoked by its DID");
    assert_eq!(
        world
            .authenticate(&created.secret, CredentialKind::Token)
            .await,
        None
    );
    // With one revoked, there is room for one more.
    let replacement = world
        .identity()
        .send(CreateToken {
            session_id: session_id.clone(),
            request: read_token("replacement"),
        })
        .await
        .expect("create")
        .expect("room after a revocation");
    seen.push(replacement.secret);

    // An owner revokes another person's token.
    let (signer_session, signer_secret) = world
        .session(&world.signer.did, SessionKind::Cli, "signer laptop")
        .await;
    seen.push(signer_secret);
    let signer_token = world
        .identity()
        .send(CreateToken {
            session_id: signer_session,
            request: read_token("signer"),
        })
        .await
        .expect("create")
        .expect("created from a CLI session");
    seen.push(signer_token.secret.clone());
    world
        .identity()
        .send(RevokeToken {
            by: owner.to_owned(),
            id: signer_token.token.id.clone(),
        })
        .await
        .expect("revoke")
        .expect("revoked by an owner");
    let listed = world
        .identity()
        .send(ListTokens {
            did: world.signer.did.clone(),
        })
        .await
        .expect("tokens");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].revoked_by.as_deref(), Some(owner));

    // The operator path serves owners only.
    let not_owner = world
        .identity()
        .send(AdminCreateToken {
            did: world.stranger.did.clone(),
            request: read_token("break glass"),
        })
        .await
        .expect("admin create");
    assert_eq!(
        not_owner,
        Err(TokenRefusal::NotOwner {
            did: world.stranger.did.clone()
        })
    );

    // Events name tokens without their secrets.
    let events = world.events().await;
    let created_event = events
        .iter()
        .find(|event| event.kind == "token.created" && event.data["id"] == created.token.id)
        .expect("token.created");
    assert_eq!(created_event.data["did"], owner);
    assert_eq!(created_event.data["scopes"], json!(["read"]));
    assert_eq!(created_event.data["repos"], json!(["acme/web", "tools/*"]));
    assert!(events.iter().any(|event| event.kind == "token.revoked"
        && event.data
            == json!({"id": signer_token.token.id, "did": world.signer.did, "by": owner})));
    seen
}

/// The `tt login` hand-off; answers every secret it saw.
async fn cli_hand_off(world: &World) -> Vec<String> {
    let owner = world.owner.did.as_str();
    let mut seen = Vec::new();
    let verifier = URL_SAFE_NO_PAD.encode([7_u8; 32]);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let issue = || async {
        world
            .identity()
            .send(IssueCliCode {
                did: owner.to_owned(),
                challenge: challenge.clone(),
                device: "laptop".into(),
                client_ip: "127.0.0.1".into(),
            })
            .await
            .expect("issue")
            .expect("issued")
    };
    let redeem = |code: String, verifier: String| async move {
        world
            .identity()
            .send(RedeemCliCode { code, verifier })
            .await
            .expect("redeem")
    };

    // Redeemed: a CLI session, listed with its device.
    let code = issue().await;
    seen.push(code.clone());
    let session = redeem(code.clone(), verifier.clone())
        .await
        .expect("a session");
    assert!(session.secret.starts_with("tartan_cli_"));
    seen.push(session.secret.clone());
    assert_eq!(
        world
            .authenticate(&session.secret, CredentialKind::CliSession)
            .await,
        Some(owner.to_owned())
    );
    let listed = world
        .identity()
        .send(ListSessions {
            did: owner.to_owned(),
        })
        .await
        .expect("sessions");
    let listed = listed
        .iter()
        .find(|listed| listed.id == session.id)
        .expect("listed");
    assert_eq!(listed.device.as_deref(), Some("laptop"));

    // Replayed.
    assert_eq!(
        redeem(code, verifier.clone()).await,
        Err(CliRefusal::Refused)
    );

    // Expired after 60 seconds.
    let code = issue().await;
    seen.push(code.clone());
    world.clock.advance(Duration::from_secs(61));
    assert_eq!(
        redeem(code, verifier.clone()).await,
        Err(CliRefusal::Refused)
    );

    // A wrong verifier consumes the code.
    let code = issue().await;
    seen.push(code.clone());
    let wrong = URL_SAFE_NO_PAD.encode([8_u8; 32]);
    assert_eq!(redeem(code.clone(), wrong).await, Err(CliRefusal::Refused));
    assert_eq!(
        redeem(code, verifier.clone()).await,
        Err(CliRefusal::Refused)
    );

    // At most 20 live codes per DID.
    for _ in 0..MAX_CLI_CODES_PER_DID {
        seen.push(issue().await);
    }
    let over = world
        .identity()
        .send(IssueCliCode {
            did: owner.to_owned(),
            challenge: challenge.clone(),
            device: "laptop".into(),
            client_ip: "127.0.0.1".into(),
        })
        .await
        .expect("issue");
    assert_eq!(over, Err(CliRefusal::TooMany));
    world.clock.advance(Duration::from_secs(61));
    seen
}

async fn refresh(world: &World, did: &str) -> tartan_identity_api::Refreshed {
    world
        .identity()
        .send(RefreshIdentity {
            did: did.to_owned(),
        })
        .await
        .expect("refresh")
}

async fn account_status(world: &World) {
    let signer = &world.signer;
    let did = signer.did.as_str();
    // Known DIDs are the ones the sweep keeps refreshing.
    let known = world
        .identity()
        .send(KnowDid {
            did: did.to_owned(),
            because: KnownAs::Signer,
        })
        .await
        .expect("know");
    assert!(known);
    let (_, session) = world.session(did, SessionKind::Browser, "Safari").await;
    let (cli_session, cli) = world.session(did, SessionKind::Cli, "desktop").await;
    let token = world
        .identity()
        .send(CreateToken {
            session_id: cli_session,
            request: read_token("status"),
        })
        .await
        .expect("create")
        .expect("created")
        .secret;
    let accepted = || async {
        [
            world
                .authenticate(&session, CredentialKind::BrowserSession)
                .await,
            world.authenticate(&cli, CredentialKind::CliSession).await,
            world.authenticate(&token, CredentialKind::Token).await,
        ]
        .iter()
        .all(|principal| principal.as_deref() == Some(did))
    };
    let refused = || async {
        [
            world
                .authenticate(&session, CredentialKind::BrowserSession)
                .await,
            world.authenticate(&cli, CredentialKind::CliSession).await,
            world.authenticate(&token, CredentialKind::Token).await,
        ]
        .iter()
        .all(Option::is_none)
    };
    assert!(accepted().await);

    // Takendown signer: every credential is refused.
    signer.set(Account::Inactive("takendown"));
    let refreshed = refresh(world, did).await;
    assert_eq!(refreshed.status, "takendown");
    assert!(refreshed.fresh);
    assert!(refused().await);
    let changes: Vec<Value> = world
        .events()
        .await
        .into_iter()
        .filter(|event| event.kind == "person.status.changed")
        .map(|event| event.data)
        .collect();
    assert!(changes.contains(&json!({"did": did, "status": "takendown"})));

    // Reactivated: the 6-hourly sweep learns it with no other event.
    signer.set(Account::Active);
    world.clock.advance(7 * HOUR);
    let report = world.sweep().await;
    assert!(report.refreshed.contains(&did.to_owned()), "{report:?}");
    assert_eq!(world.person(did).await.expect("known").status, "active");
    assert!(accepted().await);

    // A refresh whose fetches say the account is active changes nothing, whatever triggered it
    // (a forged Jetstream account event carries no payload into it).
    let refreshed = refresh(world, did).await;
    assert_eq!(refreshed.status, "active");
    assert!(accepted().await);

    // Throttled and desynchronized accounts keep working.
    for status in ["throttled", "desynchronized"] {
        signer.set(Account::Inactive(status));
        assert_eq!(refresh(world, did).await.status, status);
        assert!(accepted().await, "{status}");
    }

    // An unknown status fails closed.
    signer.set(Account::Inactive("some-future-status"));
    assert_eq!(refresh(world, did).await.status, "some-future-status");
    assert!(refused().await);

    // PDS unreachable: the status is kept.
    signer.set(Account::Active);
    assert_eq!(refresh(world, did).await.status, "active");
    signer.set(Account::Unreachable);
    let before = signer.status_requests();
    let refreshed = refresh(world, did).await;
    assert_eq!(refreshed.status, "active");
    assert!(!refreshed.fresh);
    assert_eq!(signer.status_requests(), before);
    assert!(accepted().await);

    // RepoNotFound is deleted.
    signer.set(Account::NotFound);
    assert_eq!(refresh(world, did).await.status, "deleted");
    assert!(refused().await);
    signer.set(Account::Active);
    assert_eq!(refresh(world, did).await.status, "active");
}

async fn known_dids_are_swept(world: &World) {
    let did = world.signer.did.as_str();
    assert_eq!(
        world.person(did).await.expect("known").known_as,
        vec![KnownAs::Signer]
    );

    // A status older than 6 hours is refreshed by the sweep.
    let before = world.person(did).await.expect("known");
    let requests = world.signer.status_requests();
    world.clock.advance(7 * HOUR);
    let report = world.sweep().await;
    assert!(report.refreshed.contains(&did.to_owned()), "{report:?}");
    assert!(world.signer.status_requests() > requests);
    let after = world.person(did).await.expect("known");
    assert!(after.status_checked_at > before.status_checked_at);

    // A signer without a verified signature for 30 days is dropped.
    world.clock.advance(31 * DAY);
    let report = world.sweep().await;
    assert!(
        report.dropped.contains(&(did.to_owned(), KnownAs::Signer)),
        "{report:?}"
    );
    assert!(world.person(did).await.expect("cached").known_as.is_empty());
    // Owners are never dropped.
    assert!(
        world
            .person(&world.owner.did)
            .await
            .expect("the owner")
            .known_as
            .contains(&KnownAs::Owner)
    );
}

async fn subscribers_get_identity_history(world: &World) {
    let forge = forge_handle(&world.client).expect("the forge handle");
    let name: tartan_forge::Slug = "audit".parse().expect("a slug");
    let pattern: EventPattern = "person.>".parse().expect("a pattern");
    forge
        .send(RegisterSubscriber {
            subscriber: SubscriberSpec {
                name: name.clone(),
                pattern,
            },
        })
        .await
        .expect("register");
    let subscriber = subscriber_handle(&world.client, name.as_str()).expect("the subscriber");
    let start = Instant::now();
    loop {
        let received = subscriber
            .send(Received {
                after: 0,
                limit: 500,
            })
            .await
            .expect("received");
        let kinds: Vec<&str> = received
            .iter()
            .filter(|received| received.event.source == "identity")
            .map(|received| received.event.kind.as_str())
            .collect();
        if kinds.contains(&"person.signed_in") && kinds.contains(&"person.status.changed") {
            break;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "identity history not delivered: {kinds:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn secrets_never_reach_events_or_logs(world: &World, logs: &LogBuffer, secrets: Vec<String>) {
    let events = serde_json::to_string(&world.events().await).expect("encode");
    let logged = logs.text();
    assert!(!logged.is_empty(), "the logs were captured");
    for secret in &secrets {
        assert!(
            !events.contains(secret.as_str()),
            "a secret is in the events"
        );
        assert!(!logged.contains(secret.as_str()), "a secret is in the logs");
    }
}
