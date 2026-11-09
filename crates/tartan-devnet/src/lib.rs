//! A local ATProto network for development and tests.
//!
//! One in-memory process serves a PLC directory, a PDS, an OAuth authorization server and a
//! Jetstream-compatible firehose, with people derived deterministically from a seed, so the
//! forge's identity and signing paths run end to end without the public network (design D12 of
//! the atproto-identity change).
//!
//! Everything is served from one origin, [`Devnet::url`]:
//! - the PLC directory at `/{did}` and `/{did}/log/audit` (`TARTAN_PLC_URL`);
//! - the PDS at `/xrpc/…` and `/.well-known/oauth-protected-resource`, including
//!   `com.atproto.identity.resolveHandle` (`TARTAN_DEV_HANDLE_RESOLVER`);
//! - the authorization server at `/.well-known/oauth-authorization-server` and `/oauth/…`;
//! - Jetstream's v1 `/subscribe` ([`Devnet::jetstream_url`], `TARTAN_JETSTREAM_URL`);
//! - test controls under `/devnet/` (see [`control`]), also available as methods on
//!   [`Devnet`] for tests that run the devnet in-process.
//!
//! A second PDS host ([`Devnet::migration_url`]) receives people moved by
//! [`Devnet::migrate`], and [`Devnet::start_tarpit`] starts a listener of did:web hosts whose
//! PDS never answers.
//!
//! The state lives in memory: seeded people are recreated identically on every start, and
//! records, handle changes and rotations do not survive a restart.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tartan_atproto::Mode;
use tartan_atproto::handle::Handle;
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::record::{Salt, SignatureRecord, created_at_now};
use tartan_atproto::signature::tid::Tid;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};
use tokio::task::JoinHandle;

pub mod control;
mod jetstream;
mod jwt;
mod keys;
mod oauth;
mod pds;
mod plc;
mod repo;
mod server;
mod tarpit;
mod util;

pub use tarpit::{TarpitCounts, TarpitInfo};

/// The people every devnet has, whatever its seed: `alice.test` is the owner in `dev.sh` and
/// `demo.sh`; the others are not owners.
pub const SEEDED_HANDLES: [&str; 5] = [
    "alice.test",
    "bob.test",
    "carol.test",
    "dave.test",
    "erin.test",
];

/// The account statuses a PDS reports through `com.atproto.sync.getRepoStatus`.
pub const STATUSES: [&str; 7] = [
    "active",
    "deactivated",
    "takendown",
    "suspended",
    "deleted",
    "throttled",
    "desynchronized",
];

/// How a devnet is started.
#[derive(Debug, Clone)]
pub struct DevnetConfig {
    /// The main listener (`127.0.0.1:0` picks a free port).
    pub listen: SocketAddr,
    /// The seed people's keys and DIDs are derived from.
    pub seed: String,
}

impl Default for DevnetConfig {
    fn default() -> Self {
        DevnetConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            seed: "dev".into(),
        }
    }
}

/// The DID a seeded person has under `seed`, without starting a devnet
/// (`tartan-devnet did <handle>`). `None` for a handle that is not seeded.
pub fn seeded_did(seed: &str, handle: &str) -> Option<String> {
    let handle = handle.trim_start_matches('@').to_ascii_lowercase();
    SEEDED_HANDLES.contains(&handle.as_str()).then(|| {
        let (rotation, key) = person_keys(seed, &handle, 0);
        plc::genesis(&facts(&handle, &rotation, &key, None), &rotation, 0).0
    })
}

fn person_keys(seed: &str, handle: &str, generation: u32) -> (SigningKey, SigningKey) {
    (
        keys::derive(seed, &format!("{handle}/rotation")),
        keys::derive(seed, &format!("{handle}/atproto/{generation}")),
    )
}

fn facts(handle: &str, rotation: &SigningKey, key: &SigningKey, pds: Option<&str>) -> plc::Facts {
    plc::Facts {
        handle: handle.to_owned(),
        atproto_key: keys::did_key(key),
        rotation_key: keys::did_key(rotation),
        pds: pds.map(str::to_owned),
    }
}

/// A test switch's rate-limit budget: the reference PDS's write points (create 3, update 2,
/// delete 1) per window and DID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Points per window.
    pub points: u32,
    /// The window's length in seconds.
    pub window_secs: u64,
}

/// The devnet's test switches. Each field is a behaviour tests can turn on; `POST
/// /devnet/switches` merges a JSON object of fields into the current set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Switches {
    /// The PDS endpoints (both hosts) close every connection without answering.
    pub pds_offline: bool,
    /// The authorization server's endpoints close every connection without answering.
    pub auth_offline: bool,
    /// The PLC directory closes every connection without answering.
    pub plc_offline: bool,
    /// `sync.getRecord` answers `RecordNotFound` for a missing record instead of a
    /// non-existence CAR.
    pub record_not_found_for_missing: bool,
    /// Non-existence CARs leave out one MST block of the path, so the proof is incomplete.
    pub drop_absence_path_block: bool,
    /// `sync.getRepoStatus` answers after this many milliseconds.
    pub repo_status_delay_ms: u64,
    /// The next repository write is committed and its connection closed without an answer.
    pub drop_next_write: bool,
    /// PAR answers this OAuth error (`invalid_client`, `invalid_client_metadata`,
    /// `invalid_scope`, …).
    pub par_error: Option<String>,
    /// The token endpoint answers this OAuth error.
    pub token_error: Option<String>,
    /// The token endpoint grants this scope instead of the one requested.
    pub narrow_scope: Option<String>,
    /// Client ids whose metadata the authorization server fails to fetch (`*` for every
    /// client), so PAR and every token request answer `invalid_client_metadata`.
    pub fail_client_metadata: Vec<String>,
    /// Access-token lifetime in seconds.
    pub access_token_secs: u64,
    /// A public client's whole session lifetime in seconds (refreshes after it fail).
    pub public_session_secs: u64,
    /// The authorization server metadata leaves out
    /// `authorization_response_iss_parameter_supported`.
    pub omit_iss_parameter_supported: bool,
    /// Authorization redirects leave out `iss`.
    pub callback_without_iss: bool,
    /// Repository write rate limits; off when `None`.
    pub rate_limit: Option<RateLimit>,
}

impl Default for Switches {
    fn default() -> Self {
        Switches {
            pds_offline: false,
            auth_offline: false,
            plc_offline: false,
            record_not_found_for_missing: false,
            drop_absence_path_block: false,
            repo_status_delay_ms: 0,
            drop_next_write: false,
            par_error: None,
            token_error: None,
            narrow_scope: None,
            fail_client_metadata: Vec::new(),
            access_token_secs: 3600,
            public_session_secs: 14 * 24 * 3600,
            omit_iss_parameter_supported: false,
            callback_without_iss: false,
            rate_limit: None,
        }
    }
}

/// A seeded person as the controls report them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonInfo {
    /// The current handle.
    pub handle: String,
    /// The DID.
    pub did: String,
    /// The PDS the DID document names.
    pub pds: String,
    /// The account status the PDS reports.
    pub status: String,
    /// Whether the DID is tombstoned.
    pub tombstoned: bool,
}

/// A record written through a control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordInfo {
    /// `at://{did}/{collection}/{rkey}`.
    pub uri: String,
    /// The record's CID.
    pub cid: String,
    /// The record key.
    pub rkey: String,
    /// The repository revision after the write.
    pub rev: String,
}

/// Why a control refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// No seeded person has this handle or DID.
    #[error("unknown person {0}")]
    UnknownPerson(String),
    /// The request is not acceptable.
    #[error("{0}")]
    Invalid(String),
}

pub(crate) struct Person {
    pub handle: String,
    pub did: String,
    pub rotation: SigningKey,
    pub generation: u32,
    pub key: SigningKey,
    pub host: usize,
    pub status: String,
    pub plc: Vec<plc::Operation>,
}

impl Person {
    pub(crate) fn tombstoned(&self) -> bool {
        self.plc
            .last()
            .and_then(|op| op.json.get("type"))
            .and_then(Value::as_str)
            == Some("plc_tombstone")
    }

    /// Statuses that still serve and accept writes.
    pub(crate) fn serving(&self) -> bool {
        matches!(
            self.status.as_str(),
            "active" | "throttled" | "desynchronized"
        )
    }
}

pub(crate) struct State {
    pub seed: String,
    pub hosts: Vec<String>,
    pub people: Vec<Person>,
    pub repos: HashMap<(usize, String), repo::Repo>,
    pub switches: Switches,
    pub oauth: oauth::OAuthState,
    pub events: Vec<Arc<jetstream::Event>>,
    pub last_time_us: u64,
    pub seq: u64,
    pub rate: HashMap<String, (u64, u32)>,
    pub app_passwords: HashMap<String, String>,
    pub bearer: HashMap<String, String>,
}

impl State {
    pub(crate) fn person(&self, who: &str) -> Option<usize> {
        let who = who.trim_start_matches('@');
        self.people
            .iter()
            .position(|p| p.did == who || p.handle.eq_ignore_ascii_case(who))
    }
}

pub(crate) struct Shared {
    pub state: Mutex<State>,
    pub events: broadcast::Sender<Arc<jetstream::Event>>,
    pub http: reqwest::Client,
    pub tarpit: Mutex<Option<(Arc<tarpit::Tarpit>, JoinHandle<()>)>>,
    /// Requests per XRPC method on the PDS hosts, so tests can show that nothing was fetched.
    pub xrpc_counts: std::sync::Mutex<HashMap<String, u64>>,
}

impl Shared {
    pub(crate) fn issuer(state: &State) -> &str {
        &state.hosts[0]
    }

    /// Appends a Jetstream event to the log and sends it to subscribers.
    pub(crate) fn emit(&self, state: &mut State, did: &str, kind: &str, body: Value) {
        let time_us = util::now_us().max(state.last_time_us + 1);
        state.last_time_us = time_us;
        let collection = body
            .get("collection")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut event = json!({"did": did, "time_us": time_us, "kind": kind});
        event[kind] = body;
        let event = Arc::new(jetstream::Event {
            time_us,
            did: did.to_owned(),
            kind: kind.to_owned(),
            collection,
            text: event.to_string(),
        });
        state.events.push(event.clone());
        let _ = self.events.send(event);
    }

    pub(crate) fn emit_identity(&self, state: &mut State, index: usize) {
        state.seq += 1;
        let person = &state.people[index];
        let body = json!({
            "did": person.did,
            "handle": person.handle,
            "seq": state.seq,
            "time": util::rfc3339(util::now_ms()),
        });
        let did = person.did.clone();
        self.emit(state, &did, "identity", body);
    }

    pub(crate) fn emit_account(&self, state: &mut State, index: usize) {
        state.seq += 1;
        let person = &state.people[index];
        let active = person.status == "active" && !person.tombstoned();
        let mut body = json!({
            "active": active,
            "did": person.did,
            "seq": state.seq,
            "time": util::rfc3339(util::now_ms()),
        });
        if !active {
            body["status"] = json!(if person.tombstoned() {
                "deleted"
            } else {
                person.status.as_str()
            });
        }
        let did = person.did.clone();
        self.emit(state, &did, "account", body);
    }

    /// Emits a commit event per applied write.
    pub(crate) fn emit_commit(&self, state: &mut State, did: &str, committed: &repo::Committed) {
        for applied in &committed.applied {
            let mut body = json!({
                "rev": committed.rev,
                "operation": applied.operation,
                "collection": applied.collection,
                "rkey": applied.rkey,
            });
            if let Some(record) = &applied.record {
                body["record"] = record.json.clone();
                body["cid"] = json!(record.cid.to_string());
            }
            self.emit(state, did, "commit", body);
        }
    }

    /// Writes `writes` to `who`'s repository on `host` (their current host when `None`),
    /// bypassing authorization, rate limits and account status.
    pub(crate) async fn control_write(
        &self,
        who: &str,
        host: Option<usize>,
        writes: Vec<repo::Write>,
    ) -> Result<repo::Committed, ControlError> {
        let mut state = self.state.lock().await;
        let index = state
            .person(who)
            .ok_or_else(|| ControlError::UnknownPerson(who.to_owned()))?;
        let did = state.people[index].did.clone();
        let host = host.unwrap_or(state.people[index].host);
        let repo = state.repos.get_mut(&(host, did.clone())).ok_or_else(|| {
            ControlError::Invalid(format!("{did} has no repository on host {host}"))
        })?;
        let committed = repo
            .apply(&writes)
            .await
            .map_err(|error| ControlError::Invalid(error.to_string()))?;
        self.emit_commit(&mut state, &did, &committed);
        Ok(committed)
    }

    /// Appends a PLC operation for person `index` stating their current facts.
    pub(crate) fn plc_update(&self, state: &mut State, index: usize) {
        let pds = state.hosts[state.people[index].host].clone();
        let person = &mut state.people[index];
        let facts = facts(&person.handle, &person.rotation, &person.key, Some(&pds));
        let prev = person
            .plc
            .last()
            .expect("every person has a genesis operation");
        let operation = plc::update(prev, &facts, &person.rotation, util::now_ms());
        person.plc.push(operation);
    }
}

/// A running devnet. Dropping it stops its listeners.
pub struct Devnet {
    shared: Arc<Shared>,
    url: String,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Devnet {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Ok(mut tarpit) = self.shared.tarpit.try_lock()
            && let Some((_, task)) = tarpit.take()
        {
            task.abort();
        }
    }
}

impl Devnet {
    /// Binds the listeners, seeds the people and starts serving.
    pub async fn start(config: DevnetConfig) -> anyhow::Result<Devnet> {
        let main = TcpListener::bind(config.listen).await?;
        let mut main_addr = main.local_addr()?;
        if main_addr.ip().is_unspecified() {
            main_addr.set_ip([127, 0, 0, 1].into());
        }
        let migration = TcpListener::bind(SocketAddr::new(main_addr.ip(), 0)).await?;
        let migration_addr = migration.local_addr()?;
        let hosts = vec![
            format!("http://{main_addr}"),
            format!("http://{migration_addr}"),
        ];

        let mut people = Vec::new();
        let mut repos = HashMap::new();
        for handle in SEEDED_HANDLES {
            let (rotation, key) = person_keys(&config.seed, handle, 0);
            let facts = facts(handle, &rotation, &key, Some(&hosts[0]));
            let now = util::now_ms();
            let (did, genesis) = plc::genesis(&facts, &rotation, now);
            let second = plc::update(&genesis, &facts, &rotation, now);
            repos.insert((0, did.clone()), repo::Repo::new(&did, key.clone()).await);
            people.push(Person {
                handle: handle.to_owned(),
                did,
                rotation,
                generation: 0,
                key,
                host: 0,
                status: "active".into(),
                plc: vec![genesis, second],
            });
        }

        let (events, _) = broadcast::channel(4096);
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                seed: config.seed,
                hosts: hosts.clone(),
                people,
                repos,
                switches: Switches::default(),
                oauth: oauth::OAuthState::new(),
                events: Vec::new(),
                last_time_us: 0,
                seq: 0,
                rate: HashMap::new(),
                app_passwords: HashMap::new(),
                bearer: HashMap::new(),
            }),
            events,
            http,
            tarpit: Mutex::new(None),
            xrpc_counts: std::sync::Mutex::new(HashMap::new()),
        });
        let tasks = vec![
            server::spawn(main, server::main_router(shared.clone())),
            server::spawn(migration, server::pds_router(shared.clone(), 1)),
        ];
        Ok(Devnet {
            shared,
            url: hosts[0].clone(),
            tasks,
        })
    }

    /// The main origin: PLC directory, handle resolver, PDS, authorization server and
    /// controls (`TARTAN_PLC_URL` and `TARTAN_DEV_HANDLE_RESOLVER`).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Jetstream's v1 `/subscribe` (`TARTAN_JETSTREAM_URL`).
    pub fn jetstream_url(&self) -> String {
        format!("{}/subscribe", self.url.replacen("http://", "ws://", 1))
    }

    /// The second PDS host that [`Devnet::migrate`] moves people to.
    pub async fn migration_url(&self) -> String {
        self.shared.state.lock().await.hosts[1].clone()
    }

    /// The DID of the person with `handle` (or `None`).
    pub async fn did(&self, handle: &str) -> Option<String> {
        let state = self.shared.state.lock().await;
        state
            .person(handle)
            .map(|index| state.people[index].did.clone())
    }

    /// Every person.
    pub async fn people(&self) -> Vec<PersonInfo> {
        let state = self.shared.state.lock().await;
        state
            .people
            .iter()
            .map(|person| PersonInfo {
                handle: person.handle.clone(),
                did: person.did.clone(),
                pds: state.hosts[person.host].clone(),
                status: person.status.clone(),
                tombstoned: person.tombstoned(),
            })
            .collect()
    }

    /// The current switches.
    pub async fn switches(&self) -> Switches {
        self.shared.state.lock().await.switches.clone()
    }

    /// Changes the switches.
    pub async fn set_switches(&self, change: impl FnOnce(&mut Switches)) {
        change(&mut self.shared.state.lock().await.switches);
    }

    /// Merges the fields of a JSON object into the switches.
    pub async fn merge_switches(&self, patch: Value) -> Result<Switches, ControlError> {
        control::merge_switches(&self.shared, patch).await
    }

    /// Writes a record into `who`'s repository (their current host unless `host` is given:
    /// 0 for the main host, 1 for the migration host). A missing `rkey` becomes a fresh TID.
    pub async fn write_record(
        &self,
        who: &str,
        collection: &str,
        rkey: Option<&str>,
        value: Value,
        host: Option<usize>,
    ) -> Result<RecordInfo, ControlError> {
        write_record(&self.shared, who, collection, rkey, value, host).await
    }

    /// Writes an `academy.rawkode.tartan.signature` record signing `payload` with `salt`
    /// for `who` (design D12, *Tests*). `created_at` defaults to now.
    pub async fn write_signature(
        &self,
        who: &str,
        payload: &[u8],
        salt: &Salt,
        created_at: Option<String>,
        rkey: Option<&str>,
    ) -> Result<RecordInfo, ControlError> {
        let record =
            SignatureRecord::for_git(payload, salt, created_at.unwrap_or_else(created_at_now));
        write_record(&self.shared, who, COLLECTION, rkey, record.to_json(), None).await
    }

    /// Replaces an existing record.
    pub async fn replace_record(
        &self,
        who: &str,
        collection: &str,
        rkey: &str,
        value: Value,
        host: Option<usize>,
    ) -> Result<RecordInfo, ControlError> {
        let committed = self
            .shared
            .control_write(
                who,
                host,
                vec![repo::Write::Update {
                    collection: collection.to_owned(),
                    rkey: rkey.to_owned(),
                    value,
                }],
            )
            .await?;
        record_info(&self.shared, who, &committed).await
    }

    /// Deletes a record.
    pub async fn delete_record(
        &self,
        who: &str,
        collection: &str,
        rkey: &str,
        host: Option<usize>,
    ) -> Result<String, ControlError> {
        let committed = self
            .shared
            .control_write(
                who,
                host,
                vec![repo::Write::Delete {
                    collection: collection.to_owned(),
                    rkey: rkey.to_owned(),
                }],
            )
            .await?;
        Ok(committed.rev)
    }

    /// Sets `who`'s account status at their PDS and emits a Jetstream account event.
    pub async fn set_status(&self, who: &str, status: &str) -> Result<(), ControlError> {
        control::set_status(&self.shared, who, status).await
    }

    /// Changes `who`'s handle with a PLC operation and emits an identity event.
    pub async fn set_handle(&self, who: &str, handle: &str) -> Result<(), ControlError> {
        control::set_handle(&self.shared, who, handle).await
    }

    /// Tombstones `who`'s DID and emits identity and account events.
    pub async fn tombstone(&self, who: &str) -> Result<(), ControlError> {
        control::tombstone(&self.shared, who).await
    }

    /// Rotates `who`'s `#atproto` key with a signed PLC operation; the PDS keeps signing
    /// commits with the old key for the next `stale_answers` `getRecord` answers.
    pub async fn rotate_key(&self, who: &str, stale_answers: u32) -> Result<(), ControlError> {
        control::rotate_key(&self.shared, who, stale_answers).await
    }

    /// Moves `who` to the other PDS host with a new `#atproto` key: the repository is copied,
    /// a PLC operation names the new host and key, and the old host keeps serving its own copy
    /// signed with the old key. With `withhold_identity_event` Jetstream is not told.
    pub async fn migrate(
        &self,
        who: &str,
        withhold_identity_event: bool,
    ) -> Result<(), ControlError> {
        control::migrate(&self.shared, who, withhold_identity_event).await
    }

    /// Emits a Jetstream event of `kind` (`commit`, `identity` or `account`) for `did` with
    /// `body` as given, whatever the PDS says (a forged or disagreeing event).
    pub async fn emit_jetstream(
        &self,
        did: &str,
        kind: &str,
        body: Value,
    ) -> Result<(), ControlError> {
        control::emit(&self.shared, did, kind, body).await
    }

    /// Revokes every OAuth session `who` granted (to `client_id` only, when given), as an
    /// account's app-session settings do. Returns how many were revoked.
    pub async fn revoke_sessions(
        &self,
        who: &str,
        client_id: Option<&str>,
    ) -> Result<usize, ControlError> {
        control::revoke_sessions(&self.shared, who, client_id).await
    }

    /// Creates an app password for `who`; `com.atproto.server.createSession` exchanges it for
    /// a bearer token that may write any collection.
    pub async fn create_app_password(&self, who: &str) -> Result<String, ControlError> {
        control::create_app_password(&self.shared, who).await
    }

    /// The bodies of every PAR request received, as form pairs in order.
    pub async fn par_requests(&self) -> Vec<Vec<(String, String)>> {
        self.shared.state.lock().await.oauth.par_bodies.clone()
    }

    /// Replaces the authorization server's and the PDS's DPoP nonces, so a client's stored
    /// nonces are stale.
    pub async fn rotate_nonces(&self) {
        self.shared.state.lock().await.oauth.rotate_nonces();
    }

    /// Starts (or restarts) the tarpit listener with `hosts` did:web hosts.
    pub async fn start_tarpit(&self, hosts: usize) -> anyhow::Result<TarpitInfo> {
        tarpit::start(&self.shared, hosts).await
    }

    /// How many requests the PDS hosts received for the XRPC method `nsid` (such as
    /// `com.atproto.sync.getRecord`) since the devnet started.
    pub fn xrpc_count(&self, nsid: &str) -> u64 {
        self.shared
            .xrpc_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(nsid)
            .copied()
            .unwrap_or_default()
    }

    /// Connection and request counts per tarpit host.
    pub async fn tarpit_counts(&self) -> TarpitCounts {
        tarpit::counts(&self.shared).await
    }
}

pub(crate) async fn write_record(
    shared: &Arc<Shared>,
    who: &str,
    collection: &str,
    rkey: Option<&str>,
    value: Value,
    host: Option<usize>,
) -> Result<RecordInfo, ControlError> {
    let rkey = rkey.map_or_else(|| Tid::now().as_str().to_owned(), str::to_owned);
    let committed = shared
        .control_write(
            who,
            host,
            vec![repo::Write::Create {
                collection: collection.to_owned(),
                rkey,
                value,
            }],
        )
        .await?;
    record_info(shared, who, &committed).await
}

async fn record_info(
    shared: &Arc<Shared>,
    who: &str,
    committed: &repo::Committed,
) -> Result<RecordInfo, ControlError> {
    let state = shared.state.lock().await;
    let index = state
        .person(who)
        .ok_or_else(|| ControlError::UnknownPerson(who.to_owned()))?;
    let applied = &committed.applied[0];
    let record = applied
        .record
        .as_ref()
        .ok_or_else(|| ControlError::Invalid("no record was written".into()))?;
    Ok(RecordInfo {
        uri: format!(
            "at://{}/{}/{}",
            state.people[index].did, applied.collection, applied.rkey
        ),
        cid: record.cid.to_string(),
        rkey: applied.rkey.clone(),
        rev: committed.rev.clone(),
    })
}

/// Checks a handle the devnet may give a person.
pub(crate) fn valid_handle(handle: &str) -> bool {
    Handle::parse(handle, Mode::Development).is_ok()
}

#[cfg(test)]
mod tests;
