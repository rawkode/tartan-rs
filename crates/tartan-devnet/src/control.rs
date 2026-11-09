//! Test controls under `/devnet/`. Each takes and answers JSON; `who` is a handle or DID, and
//! `host` (0 for the main host, 1 for the migration host) defaults to the person's current
//! host. Errors answer 400 with `{"error": …}`.
//!
//! | Route | Body | Effect |
//! | --- | --- | --- |
//! | `GET /devnet/people` | | every person, with handle, DID, PDS and status |
//! | `GET`/`POST /devnet/switches` | fields of [`Switches`] | read or merge the test switches |
//! | `POST /devnet/records` | `who, collection, rkey?, value, host?` | create a record |
//! | `POST /devnet/records/replace` | `who, collection, rkey, value, host?` | replace a record |
//! | `POST /devnet/records/delete` | `who, collection, rkey, host?` | delete a record |
//! | `POST /devnet/signatures` | `who, payload, salt, created_at?, rkey?` | write a signature record for a payload (standard base64) and salt (base64url) |
//! | `POST /devnet/status` | `who, status` | set the account status at the PDS |
//! | `POST /devnet/handle` | `who, handle` | change the handle (PLC operation, identity event) |
//! | `POST /devnet/tombstone` | `who` | tombstone the DID |
//! | `POST /devnet/rotate-key` | `who, stale_answers` | rotate `#atproto`; the PDS signs the next `stale_answers` answers with the old key |
//! | `POST /devnet/migrate` | `who, withhold_identity_event?` | move to the other PDS host with a new key |
//! | `POST /devnet/jetstream` | `did, kind, body` | emit a Jetstream event as given |
//! | `POST /devnet/oauth/revoke-sessions` | `who, client_id?` | revoke the person's OAuth sessions |
//! | `POST /devnet/app-passwords` | `who` | create an app password |
//! | `GET /devnet/oauth/par-requests` | | every PAR body received |
//! | `POST /devnet/oauth/rotate-nonces` | | make every client's DPoP nonces stale |
//! | `POST /devnet/tarpit` | `hosts` | start the tarpit |
//! | `GET /devnet/tarpit` | | the tarpit's counts per host |

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use tartan_atproto::signature::COLLECTION;
use tartan_atproto::signature::record::{Salt, SignatureRecord, created_at_now};

use crate::repo::{Repo, Write};
use crate::{ControlError, STATUSES, Shared, Switches, keys, oauth, tarpit, util, valid_handle};

pub(crate) fn router() -> Router<Arc<Shared>> {
    Router::new()
        .route("/devnet/people", get(people))
        .route("/devnet/switches", get(get_switches).post(post_switches))
        .route("/devnet/records", post(create_record))
        .route("/devnet/records/replace", post(replace_record))
        .route("/devnet/records/delete", post(delete_record))
        .route("/devnet/signatures", post(signature))
        .route("/devnet/status", post(status))
        .route("/devnet/handle", post(handle))
        .route("/devnet/tombstone", post(tombstone_route))
        .route("/devnet/rotate-key", post(rotate_key_route))
        .route("/devnet/migrate", post(migrate_route))
        .route("/devnet/jetstream", post(jetstream))
        .route("/devnet/oauth/revoke-sessions", post(revoke_sessions_route))
        .route("/devnet/app-passwords", post(app_password))
        .route("/devnet/oauth/par-requests", get(par_requests))
        .route("/devnet/oauth/rotate-nonces", post(rotate_nonces))
        .route("/devnet/tarpit", get(tarpit_counts).post(start_tarpit))
}

fn reply<T: serde::Serialize>(result: Result<T, ControlError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn person_index(state: &crate::State, who: &str) -> Result<usize, ControlError> {
    state
        .person(who)
        .ok_or_else(|| ControlError::UnknownPerson(who.to_owned()))
}

pub(crate) async fn merge_switches(
    shared: &Arc<Shared>,
    patch: Value,
) -> Result<Switches, ControlError> {
    let Value::Object(patch) = patch else {
        return Err(ControlError::Invalid(
            "switches must be a JSON object".into(),
        ));
    };
    let mut state = shared.state.lock().await;
    let mut current = serde_json::to_value(&state.switches)
        .map_err(|error| ControlError::Invalid(error.to_string()))?;
    for (key, value) in patch {
        current[key] = value;
    }
    let switches: Switches = serde_json::from_value(current)
        .map_err(|error| ControlError::Invalid(error.to_string()))?;
    state.switches = switches.clone();
    Ok(switches)
}

pub(crate) async fn set_status(
    shared: &Arc<Shared>,
    who: &str,
    status: &str,
) -> Result<(), ControlError> {
    if !STATUSES.contains(&status) {
        return Err(ControlError::Invalid(format!(
            "unknown status {status} (one of {})",
            STATUSES.join(", ")
        )));
    }
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    state.people[index].status = status.to_owned();
    shared.emit_account(&mut state, index);
    Ok(())
}

pub(crate) async fn set_handle(
    shared: &Arc<Shared>,
    who: &str,
    handle: &str,
) -> Result<(), ControlError> {
    let handle = handle.trim_start_matches('@').to_ascii_lowercase();
    if !valid_handle(&handle) {
        return Err(ControlError::Invalid(format!(
            "{handle} is not a valid handle"
        )));
    }
    let mut state = shared.state.lock().await;
    if state.person(&handle).is_some() {
        return Err(ControlError::Invalid(format!("{handle} is taken")));
    }
    let index = person_index(&state, who)?;
    state.people[index].handle = handle;
    shared.plc_update(&mut state, index);
    shared.emit_identity(&mut state, index);
    Ok(())
}

pub(crate) async fn tombstone(shared: &Arc<Shared>, who: &str) -> Result<(), ControlError> {
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    let person = &mut state.people[index];
    if person.tombstoned() {
        return Err(ControlError::Invalid(format!(
            "{} is already tombstoned",
            person.did
        )));
    }
    let prev = person
        .plc
        .last()
        .expect("every person has a genesis operation");
    let operation = crate::plc::tombstone(prev, &person.rotation, util::now_ms());
    person.plc.push(operation);
    shared.emit_identity(&mut state, index);
    shared.emit_account(&mut state, index);
    Ok(())
}

fn next_key(state: &crate::State, index: usize) -> (u32, k256::ecdsa::SigningKey) {
    let person = &state.people[index];
    let generation = person.generation + 1;
    (
        generation,
        keys::derive(&state.seed, &format!("{}/atproto/{generation}", person.did)),
    )
}

pub(crate) async fn rotate_key(
    shared: &Arc<Shared>,
    who: &str,
    stale_answers: u32,
) -> Result<(), ControlError> {
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    if state.people[index].tombstoned() {
        return Err(ControlError::Invalid("the DID is tombstoned".into()));
    }
    let (generation, key) = next_key(&state, index);
    let (host, did) = (state.people[index].host, state.people[index].did.clone());
    let old = std::mem::replace(&mut state.people[index].key, key.clone());
    state.people[index].generation = generation;
    if let Some(repo) = state.repos.get_mut(&(host, did)) {
        repo.key = key;
        repo.stale = (stale_answers > 0).then_some((old, stale_answers));
    }
    shared.plc_update(&mut state, index);
    shared.emit_identity(&mut state, index);
    Ok(())
}

pub(crate) async fn migrate(
    shared: &Arc<Shared>,
    who: &str,
    withhold_identity_event: bool,
) -> Result<(), ControlError> {
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    if state.people[index].tombstoned() {
        return Err(ControlError::Invalid("the DID is tombstoned".into()));
    }
    let (generation, key) = next_key(&state, index);
    let (from, did) = (state.people[index].host, state.people[index].did.clone());
    let to = 1 - from;
    let source = state
        .repos
        .get(&(from, did.clone()))
        .ok_or_else(|| ControlError::Invalid(format!("{did} has no repository on host {from}")))?;
    let copy: Repo = source
        .copy_with_key(key.clone())
        .await
        .map_err(|error| ControlError::Invalid(error.to_string()))?;
    state.repos.insert((to, did), copy);
    let person = &mut state.people[index];
    person.key = key;
    person.generation = generation;
    person.host = to;
    shared.plc_update(&mut state, index);
    if !withhold_identity_event {
        shared.emit_identity(&mut state, index);
    }
    Ok(())
}

pub(crate) async fn emit(
    shared: &Arc<Shared>,
    did: &str,
    kind: &str,
    body: Value,
) -> Result<(), ControlError> {
    if !matches!(kind, "commit" | "identity" | "account") {
        return Err(ControlError::Invalid(format!("unknown event kind {kind}")));
    }
    if !did.starts_with("did:") {
        return Err(ControlError::Invalid(format!("{did} is not a DID")));
    }
    let mut state = shared.state.lock().await;
    state.seq += 1;
    shared.emit(&mut state, did, kind, body);
    Ok(())
}

pub(crate) async fn revoke_sessions(
    shared: &Arc<Shared>,
    who: &str,
    client_id: Option<&str>,
) -> Result<usize, ControlError> {
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    let did = state.people[index].did.clone();
    Ok(oauth::revoke_sessions(&mut state, &did, client_id))
}

pub(crate) async fn create_app_password(
    shared: &Arc<Shared>,
    who: &str,
) -> Result<String, ControlError> {
    let mut state = shared.state.lock().await;
    let index = person_index(&state, who)?;
    let password = util::random_token(12);
    let did = state.people[index].did.clone();
    state.app_passwords.insert(password.clone(), did);
    Ok(password)
}

async fn people(State(shared): State<Arc<Shared>>) -> Response {
    let state = shared.state.lock().await;
    let people: Vec<Value> = state
        .people
        .iter()
        .map(|p| {
            json!({
                "handle": p.handle,
                "did": p.did,
                "pds": state.hosts[p.host],
                "status": p.status,
                "tombstoned": p.tombstoned(),
            })
        })
        .collect();
    Json(people).into_response()
}

async fn get_switches(State(shared): State<Arc<Shared>>) -> Response {
    Json(shared.state.lock().await.switches.clone()).into_response()
}

async fn post_switches(State(shared): State<Arc<Shared>>, Json(patch): Json<Value>) -> Response {
    reply(merge_switches(&shared, patch).await)
}

#[derive(Deserialize)]
struct RecordBody {
    who: String,
    collection: String,
    rkey: Option<String>,
    value: Option<Value>,
    host: Option<usize>,
}

async fn create_record(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<RecordBody>,
) -> Response {
    let Some(value) = body.value else {
        return reply::<()>(Err(ControlError::Invalid("value is required".into())));
    };
    reply(
        crate::write_record(
            &shared,
            &body.who,
            &body.collection,
            body.rkey.as_deref(),
            value,
            body.host,
        )
        .await,
    )
}

async fn replace_record(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<RecordBody>,
) -> Response {
    let (Some(rkey), Some(value)) = (body.rkey, body.value) else {
        return reply::<()>(Err(ControlError::Invalid(
            "rkey and value are required".into(),
        )));
    };
    let result = shared
        .control_write(
            &body.who,
            body.host,
            vec![Write::Update {
                collection: body.collection,
                rkey,
                value,
            }],
        )
        .await
        .map(|committed| {
            let applied = &committed.applied[0];
            json!({
                "cid": applied.record.as_ref().map(|r| r.cid.to_string()),
                "rkey": applied.rkey,
                "rev": committed.rev,
            })
        });
    reply(result)
}

async fn delete_record(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<RecordBody>,
) -> Response {
    let Some(rkey) = body.rkey else {
        return reply::<()>(Err(ControlError::Invalid("rkey is required".into())));
    };
    let result = shared
        .control_write(
            &body.who,
            body.host,
            vec![Write::Delete {
                collection: body.collection,
                rkey,
            }],
        )
        .await
        .map(|committed| json!({"rev": committed.rev}));
    reply(result)
}

#[derive(Deserialize)]
struct SignatureBody {
    who: String,
    payload: String,
    salt: String,
    created_at: Option<String>,
    rkey: Option<String>,
}

async fn signature(State(shared): State<Arc<Shared>>, Json(body): Json<SignatureBody>) -> Response {
    let Ok(payload) = STANDARD.decode(&body.payload) else {
        return reply::<()>(Err(ControlError::Invalid(
            "payload is not standard base64".into(),
        )));
    };
    let Some(salt) = Salt::from_base64url(&body.salt) else {
        return reply::<()>(Err(ControlError::Invalid(
            "salt is not 43 characters of unpadded base64url".into(),
        )));
    };
    let record = SignatureRecord::for_git(
        &payload,
        &salt,
        body.created_at.unwrap_or_else(created_at_now),
    );
    reply(
        crate::write_record(
            &shared,
            &body.who,
            COLLECTION,
            body.rkey.as_deref(),
            record.to_json(),
            None,
        )
        .await,
    )
}

#[derive(Deserialize)]
struct WhoBody {
    who: String,
    status: Option<String>,
    handle: Option<String>,
    stale_answers: Option<u32>,
    #[serde(default)]
    withhold_identity_event: bool,
    client_id: Option<String>,
}

async fn status(State(shared): State<Arc<Shared>>, Json(body): Json<WhoBody>) -> Response {
    let status = body.status.unwrap_or_default();
    reply(set_status(&shared, &body.who, &status).await)
}

async fn handle(State(shared): State<Arc<Shared>>, Json(body): Json<WhoBody>) -> Response {
    let handle = body.handle.unwrap_or_default();
    reply(set_handle(&shared, &body.who, &handle).await)
}

async fn tombstone_route(State(shared): State<Arc<Shared>>, Json(body): Json<WhoBody>) -> Response {
    reply(tombstone(&shared, &body.who).await)
}

async fn rotate_key_route(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<WhoBody>,
) -> Response {
    reply(rotate_key(&shared, &body.who, body.stale_answers.unwrap_or(0)).await)
}

async fn migrate_route(State(shared): State<Arc<Shared>>, Json(body): Json<WhoBody>) -> Response {
    reply(migrate(&shared, &body.who, body.withhold_identity_event).await)
}

async fn revoke_sessions_route(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<WhoBody>,
) -> Response {
    reply(
        revoke_sessions(&shared, &body.who, body.client_id.as_deref())
            .await
            .map(|revoked| json!({"revoked": revoked})),
    )
}

async fn app_password(State(shared): State<Arc<Shared>>, Json(body): Json<WhoBody>) -> Response {
    reply(
        create_app_password(&shared, &body.who)
            .await
            .map(|password| json!({"password": password})),
    )
}

#[derive(Deserialize)]
struct EventBody {
    did: String,
    kind: String,
    body: Value,
}

async fn jetstream(State(shared): State<Arc<Shared>>, Json(body): Json<EventBody>) -> Response {
    reply(emit(&shared, &body.did, &body.kind, body.body).await)
}

async fn par_requests(State(shared): State<Arc<Shared>>) -> Response {
    let bodies: Vec<Value> = shared
        .state
        .lock()
        .await
        .oauth
        .par_bodies
        .iter()
        .map(|pairs| Value::Array(pairs.iter().map(|(k, v)| json!([k, v])).collect()))
        .collect();
    Json(bodies).into_response()
}

async fn rotate_nonces(State(shared): State<Arc<Shared>>) -> Response {
    shared.state.lock().await.oauth.rotate_nonces();
    Json(json!({})).into_response()
}

#[derive(Deserialize)]
struct TarpitBody {
    hosts: usize,
}

async fn start_tarpit(State(shared): State<Arc<Shared>>, Json(body): Json<TarpitBody>) -> Response {
    match tarpit::start(&shared, body.hosts).await {
        Ok(info) => Json(info).into_response(),
        Err(error) => reply::<()>(Err(ControlError::Invalid(error.to_string()))),
    }
}

async fn tarpit_counts(State(shared): State<Arc<Shared>>) -> Response {
    Json(tarpit::counts(&shared).await).into_response()
}
