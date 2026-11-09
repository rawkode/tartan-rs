//! The PDS endpoints of each host, and the PLC directory's.
//!
//! Writes accept DPoP-bound access tokens from the devnet's authorization server, with the
//! PDS's own DPoP nonce, and enforce the token's granular `repo:` permissions per write. They
//! also accept bearer tokens made from app passwords, which may write anything.

// Helpers here fail with the HTTP answer to send; boxing every `Response` would only add noise
// to a test server.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tartan_atproto::signature::tid::Tid;

use crate::oauth::permits;
use crate::repo::{Committed, Write, WriteError};
use crate::server::Conn;
use crate::util::{self, xrpc_error};
use crate::{Shared, State as DevState, plc};

/// The most writes one `applyWrites` call may carry (the reference PDS's limit).
pub(crate) const MAX_WRITES: usize = 200;

/// A PDS host's handler context.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub shared: Arc<Shared>,
    pub host: usize,
}

pub(crate) fn router() -> Router<Ctx> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route("/xrpc/com.atproto.sync.getRecord", get(sync_get_record))
        .route("/xrpc/com.atproto.sync.getRepoStatus", get(get_repo_status))
        .route("/xrpc/com.atproto.repo.getRecord", get(get_record))
        .route("/xrpc/com.atproto.repo.listRecords", get(list_records))
        .route("/xrpc/com.atproto.repo.applyWrites", post(apply_writes))
        .route("/xrpc/com.atproto.repo.createRecord", post(create_record))
        .route("/xrpc/com.atproto.repo.deleteRecord", post(delete_record))
        .route(
            "/xrpc/com.atproto.identity.resolveHandle",
            get(resolve_handle),
        )
        .route(
            "/xrpc/com.atproto.server.createSession",
            post(create_session),
        )
}

type Params = Query<HashMap<String, String>>;

fn param<'a>(params: &'a HashMap<String, String>, name: &str) -> &'a str {
    params.get(name).map_or("", String::as_str)
}

/// The error a repository of an inactive account answers.
fn status_error(status: &str) -> Response {
    let (error, message) = match status {
        "deactivated" => ("RepoDeactivated", "Repo has been deactivated"),
        "takendown" => ("RepoTakendown", "Repo has been takendown"),
        "suspended" => ("RepoSuspended", "Repo has been suspended"),
        _ => ("RepoNotFound", "Could not find repo"),
    };
    xrpc_error(StatusCode::BAD_REQUEST, error, message)
}

fn repo_not_found(who: &str) -> Response {
    xrpc_error(
        StatusCode::BAD_REQUEST,
        "RepoNotFound",
        format!("Could not find repo for DID: {who}"),
    )
}

/// The person whose repository `who` (DID or handle) names on `host`, if it is served.
fn served_person(state: &DevState, host: usize, who: &str) -> Result<usize, Response> {
    let index = state.person(who).ok_or_else(|| repo_not_found(who))?;
    let person = &state.people[index];
    if person.tombstoned() || !state.repos.contains_key(&(host, person.did.clone())) {
        return Err(repo_not_found(who));
    }
    if !person.serving() {
        return Err(status_error(&person.status));
    }
    Ok(index)
}

fn valid_nsid(collection: &str) -> bool {
    collection.split('.').count() >= 3
        && collection.len() <= 317
        && collection.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

fn valid_rkey(rkey: &str) -> bool {
    !rkey.is_empty()
        && rkey.len() <= 512
        && rkey != "."
        && rkey != ".."
        && rkey
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:~-".contains(c))
}

async fn protected_resource(State(ctx): State<Ctx>, Extension(conn): Extension<Conn>) -> Response {
    let state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return conn.hang_up().await;
    }
    Json(json!({
        "resource": state.hosts[ctx.host],
        "authorization_servers": [Shared::issuer(&state)],
        "scopes_supported": [],
        "bearer_methods_supported": ["header"],
        "resource_documentation": "https://atproto.com",
    }))
    .into_response()
}

async fn sync_get_record(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    Query(params): Params,
) -> Response {
    let mut state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let (did, collection, rkey) = (
        param(&params, "did"),
        param(&params, "collection"),
        param(&params, "rkey"),
    );
    if !did.starts_with("did:") || !valid_nsid(collection) || !valid_rkey(rkey) {
        return xrpc_error(
            StatusCode::BAD_REQUEST,
            "InvalidRequest",
            "invalid did, collection or rkey",
        );
    }
    let index = match served_person(&state, ctx.host, did) {
        Ok(index) => index,
        Err(response) => return response,
    };
    let did = state.people[index].did.clone();
    let switches = state.switches.clone();
    let Some(repo) = state.repos.get_mut(&(ctx.host, did.clone())) else {
        return repo_not_found(&did);
    };
    let missing = repo.get(collection, rkey).is_none();
    if missing && switches.record_not_found_for_missing {
        return xrpc_error(
            StatusCode::BAD_REQUEST,
            "RecordNotFound",
            format!("Could not locate record: at://{did}/{collection}/{rkey}"),
        );
    }
    match repo
        .car(
            collection,
            rkey,
            missing && switches.drop_absence_path_block,
        )
        .await
    {
        Ok(car) => ([(header::CONTENT_TYPE, "application/vnd.ipld.car")], car).into_response(),
        Err(error) => xrpc_error(StatusCode::BAD_REQUEST, "InvalidRequest", error.to_string()),
    }
}

async fn get_repo_status(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    Query(params): Params,
) -> Response {
    let delay = ctx.shared.state.lock().await.switches.repo_status_delay_ms;
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let did = param(&params, "did");
    let Some(index) = state.person(did).filter(|i| state.people[*i].did == did) else {
        return repo_not_found(did);
    };
    let person = &state.people[index];
    let Some(repo) = state.repos.get(&(ctx.host, person.did.clone())) else {
        return repo_not_found(did);
    };
    if person.tombstoned() {
        return repo_not_found(did);
    }
    let mut body =
        json!({"did": person.did, "active": person.status == "active", "rev": repo.rev()});
    if person.status != "active" {
        body["status"] = json!(person.status);
    }
    Json(body).into_response()
}

async fn get_record(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    Query(params): Params,
) -> Response {
    let state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let (collection, rkey) = (param(&params, "collection"), param(&params, "rkey"));
    let index = match served_person(&state, ctx.host, param(&params, "repo")) {
        Ok(index) => index,
        Err(response) => return response,
    };
    let did = &state.people[index].did;
    let uri = format!("at://{did}/{collection}/{rkey}");
    let record = state
        .repos
        .get(&(ctx.host, did.clone()))
        .and_then(|repo| repo.get(collection, rkey))
        .filter(|record| {
            params
                .get("cid")
                .is_none_or(|cid| *cid == record.cid.to_string())
        });
    match record {
        Some(record) => Json(json!({
            "uri": uri,
            "cid": record.cid.to_string(),
            "value": record.json,
        }))
        .into_response(),
        None => xrpc_error(
            StatusCode::BAD_REQUEST,
            "RecordNotFound",
            format!("Could not locate record: {uri}"),
        ),
    }
}

async fn list_records(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    Query(params): Params,
) -> Response {
    let state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let collection = param(&params, "collection");
    let index = match served_person(&state, ctx.host, param(&params, "repo")) {
        Ok(index) => index,
        Err(response) => return response,
    };
    let did = state.people[index].did.clone();
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 100);
    let reverse = params.get("reverse").is_some_and(|r| r == "true");
    let cursor = params.get("cursor");
    let mut records = state
        .repos
        .get(&(ctx.host, did.clone()))
        .map(|repo| repo.list(collection))
        .unwrap_or_default();
    // The reference PDS lists newest record keys first unless `reverse` is set.
    if !reverse {
        records.reverse();
    }
    let page: Vec<_> = records
        .into_iter()
        .filter(|(rkey, _)| match cursor {
            Some(cursor) if reverse => rkey.as_str() > cursor.as_str(),
            Some(cursor) => rkey.as_str() < cursor.as_str(),
            None => true,
        })
        .take(limit)
        .collect();
    let next = (page.len() == limit)
        .then(|| page.last().map(|(rkey, _)| rkey.clone()))
        .flatten();
    let records: Vec<Value> = page
        .into_iter()
        .map(|(rkey, record)| {
            json!({
                "uri": format!("at://{did}/{collection}/{rkey}"),
                "cid": record.cid.to_string(),
                "value": record.json,
            })
        })
        .collect();
    let mut body = json!({"records": records});
    if let Some(next) = next {
        body["cursor"] = json!(next);
    }
    Json(body).into_response()
}

async fn resolve_handle(State(ctx): State<Ctx>, Query(params): Params) -> Response {
    let state = ctx.shared.state.lock().await;
    let handle = param(&params, "handle").to_ascii_lowercase();
    match state
        .people
        .iter()
        .find(|p| p.handle == handle && !p.tombstoned())
    {
        Some(person) => Json(json!({"did": person.did})).into_response(),
        None => xrpc_error(
            StatusCode::BAD_REQUEST,
            "HandleNotFound",
            "Unable to resolve handle",
        ),
    }
}

async fn create_session(State(ctx): State<Ctx>, Json(body): Json<Value>) -> Response {
    let mut state = ctx.shared.state.lock().await;
    let identifier = body
        .get("identifier")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let password = body
        .get("password")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let did = state
        .app_passwords
        .get(password)
        .filter(|did| {
            state
                .person(identifier)
                .is_some_and(|i| state.people[i].did == **did)
        })
        .cloned();
    let Some(did) = did else {
        return xrpc_error(
            StatusCode::UNAUTHORIZED,
            "AuthenticationRequired",
            "Invalid identifier or password",
        );
    };
    let token = util::random_token(32);
    state.bearer.insert(token.clone(), did.clone());
    let handle = state
        .person(&did)
        .map(|i| state.people[i].handle.clone())
        .unwrap_or_default();
    Json(json!({
        "did": did,
        "handle": handle,
        "accessJwt": token,
        "refreshJwt": util::random_token(32),
        "active": true,
    }))
    .into_response()
}

/// An authenticated writer.
struct Principal {
    did: String,
    scope: String,
}

/// Authenticates a PDS request: a DPoP-bound access token (with this host's DPoP nonce) or an
/// app-password bearer token.
fn authenticate(
    state: &mut DevState,
    host: usize,
    headers: &HeaderMap,
    method: &str,
    path: &str,
) -> Result<Principal, Response> {
    let nonce = state.oauth.rs_nonce.clone();
    let unauthorized = |error: &str, description: &str| {
        let mut response = xrpc_error(
            StatusCode::UNAUTHORIZED,
            if error == "use_dpop_nonce" {
                "UseDpopNonceError"
            } else {
                "InvalidToken"
            },
            description,
        );
        let challenge =
            format!("DPoP algs=\"ES256\", error=\"{error}\", error_description=\"{description}\"");
        if let Ok(value) = HeaderValue::from_str(&challenge) {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, value);
        }
        with_nonce(&mut response, &nonce);
        response
    };
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if let Some(token) = authorization.strip_prefix("Bearer ") {
        return state
            .bearer
            .get(token)
            .map(|did| Principal {
                did: did.clone(),
                scope: "atproto transition:generic".into(),
            })
            .ok_or_else(|| xrpc_error(StatusCode::UNAUTHORIZED, "InvalidToken", "Bad token"));
    }
    let Some(token) = authorization.strip_prefix("DPoP ") else {
        return Err(xrpc_error(
            StatusCode::UNAUTHORIZED,
            "AuthMissing",
            "Authentication Required",
        ));
    };
    let proof = headers
        .get("DPoP")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| unauthorized("invalid_dpop_proof", "DPoP proof required"))?;
    let url = format!("{}{path}", state.hosts[host]);
    let now = util::now_ms();
    let proof = crate::jwt::verify_dpop(proof, method, &url, (now / 1000) as i64)
        .map_err(|error| unauthorized("invalid_dpop_proof", &error))?;
    if proof.nonce.as_deref() != Some(nonce.as_str()) {
        return Err(unauthorized(
            "use_dpop_nonce",
            "Resource server requires nonce in DPoP proof",
        ));
    }
    if !state.oauth.fresh_jti(&proof.jti, now) {
        return Err(unauthorized("invalid_dpop_proof", "DPoP proof replayed"));
    }
    if proof.ath.as_deref() != Some(crate::jwt::ath(token).as_str()) {
        return Err(unauthorized(
            "invalid_dpop_proof",
            "DPoP ath does not match the token",
        ));
    }
    let (access, grant) = state.oauth.live_access(token, now).ok_or_else(|| {
        unauthorized(
            "invalid_token",
            "The access token is invalid, expired or revoked",
        )
    })?;
    if access.jkt != proof.jkt {
        return Err(unauthorized(
            "invalid_token",
            "The DPoP key does not match the token",
        ));
    }
    Ok(Principal {
        did: grant.did.clone(),
        scope: grant.scope.clone(),
    })
}

fn with_nonce(response: &mut Response, nonce: &str) {
    if let Ok(value) = HeaderValue::from_str(nonce) {
        response.headers_mut().insert("DPoP-Nonce", value);
    }
}

/// Authenticates, checks and commits `writes` to the repository `repo` names.
async fn commit_writes(
    ctx: &Ctx,
    conn: &Conn,
    headers: &HeaderMap,
    path: &str,
    repo: &str,
    writes: Vec<Write>,
) -> Result<(Committed, String, String), Response> {
    let mut state = ctx.shared.state.lock().await;
    if state.switches.pds_offline {
        drop(state);
        return Err(conn.hang_up().await);
    }
    let principal = authenticate(&mut state, ctx.host, headers, "POST", path)?;
    let nonce = state.oauth.rs_nonce.clone();
    let fail = |mut response: Response| {
        with_nonce(&mut response, &nonce);
        response
    };
    if state.person(repo).map(|i| &state.people[i].did) != Some(&principal.did) {
        return Err(fail(xrpc_error(
            StatusCode::FORBIDDEN,
            "InvalidRequest",
            "repo must be the authenticated account",
        )));
    }
    let index = served_person(&state, ctx.host, &principal.did).map_err(fail)?;
    if writes.len() > MAX_WRITES {
        return Err(fail(xrpc_error(
            StatusCode::BAD_REQUEST,
            "InvalidRequest",
            format!("Too many writes. Max: {MAX_WRITES}"),
        )));
    }
    for write in &writes {
        let (collection, action, value) = match write {
            Write::Create {
                collection, value, ..
            } => (collection, "create", Some(value)),
            Write::Update {
                collection, value, ..
            } => (collection, "update", Some(value)),
            Write::Delete { collection, .. } => (collection, "delete", None),
        };
        if !permits(&principal.scope, collection, action) {
            return Err(fail(xrpc_error(
                StatusCode::FORBIDDEN,
                "InsufficientScope",
                format!("Missing required scope \"repo:{collection}?action={action}\""),
            )));
        }
        if let Some(value) = value {
            let declared = value.get("$type").and_then(Value::as_str);
            if !value.is_object() || declared.is_some_and(|t| t != collection) {
                return Err(fail(xrpc_error(
                    StatusCode::BAD_REQUEST,
                    "InvalidRequest",
                    format!("Invalid $type: expected {collection}"),
                )));
            }
        }
    }
    if let Some(limit) = state.switches.rate_limit {
        let now = util::now_ms();
        let cost: u32 = writes.iter().map(Write::points).sum();
        let window_ms = limit.window_secs * 1000;
        let entry = state.rate.entry(principal.did.clone()).or_insert((now, 0));
        if now >= entry.0 + window_ms {
            *entry = (now, 0);
        }
        if entry.1 + cost > limit.points {
            let reset = (entry.0 + window_ms).div_ceil(1000);
            let remaining = limit.points.saturating_sub(entry.1);
            let mut response = fail(xrpc_error(
                StatusCode::TOO_MANY_REQUESTS,
                "RateLimitExceeded",
                "Rate Limit Exceeded",
            ));
            for (name, value) in [
                ("ratelimit-limit", limit.points.to_string()),
                ("ratelimit-remaining", remaining.to_string()),
                ("ratelimit-reset", reset.to_string()),
                (
                    "ratelimit-policy",
                    format!("{};w={}", limit.points, limit.window_secs),
                ),
            ] {
                if let Ok(value) = HeaderValue::from_str(&value) {
                    response.headers_mut().insert(name, value);
                }
            }
            return Err(response);
        }
        entry.1 += cost;
    }
    let did = state.people[index].did.clone();
    let Some(repository) = state.repos.get_mut(&(ctx.host, did.clone())) else {
        return Err(fail(repo_not_found(&did)));
    };
    let committed = match repository.apply(&writes).await {
        Ok(committed) => committed,
        Err(WriteError::Conflict(message)) => {
            return Err(fail(xrpc_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError",
                message,
            )));
        }
        Err(WriteError::Invalid(message)) => {
            return Err(fail(xrpc_error(
                StatusCode::BAD_REQUEST,
                "InvalidRequest",
                message,
            )));
        }
    };
    ctx.shared.emit_commit(&mut state, &did, &committed);
    if state.switches.drop_next_write {
        state.switches.drop_next_write = false;
        drop(state);
        return Err(conn.hang_up().await);
    }
    Ok((committed, did, nonce))
}

fn answer(body: Value, nonce: &str) -> Response {
    let mut response = Json(body).into_response();
    with_nonce(&mut response, nonce);
    response
}

fn bad_request(message: &str) -> Response {
    xrpc_error(StatusCode::BAD_REQUEST, "InvalidRequest", message)
}

fn parse_write(write: &Value) -> Result<Write, Response> {
    let text = |key: &str| write.get(key).and_then(Value::as_str).map(str::to_owned);
    let collection = text("collection")
        .filter(|c| valid_nsid(c))
        .ok_or_else(|| bad_request("each write needs a valid collection"))?;
    let rkey = |required: bool| -> Result<String, Response> {
        match text("rkey") {
            Some(rkey) if valid_rkey(&rkey) => Ok(rkey),
            Some(_) => Err(bad_request("invalid rkey")),
            None if required => Err(bad_request("rkey is required")),
            None => Ok(Tid::now().as_str().to_owned()),
        }
    };
    let value = || {
        write
            .get("value")
            .cloned()
            .ok_or_else(|| bad_request("value is required"))
    };
    match write.get("$type").and_then(Value::as_str) {
        Some("com.atproto.repo.applyWrites#create") => Ok(Write::Create {
            collection,
            rkey: rkey(false)?,
            value: value()?,
        }),
        Some("com.atproto.repo.applyWrites#update") => Ok(Write::Update {
            collection,
            rkey: rkey(true)?,
            value: value()?,
        }),
        Some("com.atproto.repo.applyWrites#delete") => Ok(Write::Delete {
            collection,
            rkey: rkey(true)?,
        }),
        _ => Err(bad_request("unknown write $type")),
    }
}

async fn apply_writes(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let repo = body
        .get("repo")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let writes = match body.get("writes").and_then(Value::as_array) {
        Some(writes) => writes
            .iter()
            .map(parse_write)
            .collect::<Result<Vec<_>, _>>(),
        None => Err(bad_request("writes is required")),
    };
    let writes = match writes {
        Ok(writes) => writes,
        Err(response) => return response,
    };
    let path = "/xrpc/com.atproto.repo.applyWrites";
    let (committed, did, nonce) =
        match commit_writes(&ctx, &conn, &headers, path, &repo, writes).await {
            Ok(done) => done,
            Err(response) => return response,
        };
    let results: Vec<Value> = committed
        .applied
        .iter()
        .map(|applied| match &applied.record {
            Some(record) => json!({
                "$type": format!("com.atproto.repo.applyWrites#{}Result", applied.operation),
                "uri": format!("at://{did}/{}/{}", applied.collection, applied.rkey),
                "cid": record.cid.to_string(),
                "validationStatus": "unknown",
            }),
            None => json!({"$type": "com.atproto.repo.applyWrites#deleteResult"}),
        })
        .collect();
    answer(
        json!({
            "commit": {"cid": committed.commit_cid.to_string(), "rev": committed.rev},
            "results": results,
        }),
        &nonce,
    )
}

async fn create_record(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut write = json!({
        "$type": "com.atproto.repo.applyWrites#create",
        "collection": body.get("collection"),
        "value": body.get("record"),
    });
    if let Some(rkey) = body.get("rkey") {
        write["rkey"] = rkey.clone();
    }
    let write = match parse_write(&write) {
        Ok(write) => write,
        Err(response) => return response,
    };
    let repo = body
        .get("repo")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let path = "/xrpc/com.atproto.repo.createRecord";
    let (committed, did, nonce) =
        match commit_writes(&ctx, &conn, &headers, path, &repo, vec![write]).await {
            Ok(done) => done,
            Err(response) => return response,
        };
    let applied = &committed.applied[0];
    answer(
        json!({
            "uri": format!("at://{did}/{}/{}", applied.collection, applied.rkey),
            "cid": applied.record.as_ref().map(|r| r.cid.to_string()),
            "commit": {"cid": committed.commit_cid.to_string(), "rev": committed.rev},
            "validationStatus": "unknown",
        }),
        &nonce,
    )
}

async fn delete_record(
    State(ctx): State<Ctx>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let write = json!({
        "$type": "com.atproto.repo.applyWrites#delete",
        "collection": body.get("collection"),
        "rkey": body.get("rkey"),
    });
    let write = match parse_write(&write) {
        Ok(write) => write,
        Err(response) => return response,
    };
    let repo = body
        .get("repo")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // The reference PDS answers a deletion of a missing record with success and no commit.
    let missing = {
        let state = ctx.shared.state.lock().await;
        let Write::Delete { collection, rkey } = &write else {
            unreachable!("parse_write of a delete is a delete");
        };
        state
            .person(&repo)
            .and_then(|i| state.repos.get(&(ctx.host, state.people[i].did.clone())))
            .is_some_and(|r| r.get(collection, rkey).is_none())
    };
    let path = "/xrpc/com.atproto.repo.deleteRecord";
    if missing {
        let mut state = ctx.shared.state.lock().await;
        if state.switches.pds_offline {
            drop(state);
            return conn.hang_up().await;
        }
        if let Err(response) = authenticate(&mut state, ctx.host, &headers, "POST", path) {
            return response;
        }
        let nonce = state.oauth.rs_nonce.clone();
        return answer(json!({}), &nonce);
    }
    match commit_writes(&ctx, &conn, &headers, path, &repo, vec![write]).await {
        Ok((committed, _, nonce)) => answer(
            json!({"commit": {"cid": committed.commit_cid.to_string(), "rev": committed.rev}}),
            &nonce,
        ),
        Err(response) => response,
    }
}

/// `GET /{did}`: the PLC directory's DID document.
pub(crate) async fn plc_document(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    Path(did): Path<String>,
) -> Response {
    let state = shared.state.lock().await;
    if state.switches.plc_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let document = state
        .people
        .iter()
        .find(|p| p.did == did)
        .and_then(|p| plc::document(&p.did, &p.plc));
    match document {
        Some(document) => Json(document).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("DID not available: {did}")})),
        )
            .into_response(),
    }
}

/// `GET /{did}/log/audit`.
pub(crate) async fn plc_audit_log(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    Path(did): Path<String>,
) -> Response {
    let state = shared.state.lock().await;
    if state.switches.plc_offline {
        drop(state);
        return conn.hang_up().await;
    }
    match state.people.iter().find(|p| p.did == did) {
        Some(person) => Json(plc::audit_log(&person.did, &person.plc)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("DID not registered: {did}")})),
        )
            .into_response(),
    }
}
