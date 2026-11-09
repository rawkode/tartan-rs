//! People and their signature records (design D9 *Caching and display*, D7 *Watching
//! records*, D15): `GET /-/api/people`, `POST /-/api/people/resolve` and
//! `POST /-/api/people/{did}/signatures/{rkey}/refresh`.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Extension, Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tartan_atproto::did::Did;
use tartan_atproto::handle::Handle;
use tartan_atproto::resolver::ResolveError;
use tartan_atproto::signature::tid::Tid;
use tartan_forge::identity_handle;
use tartan_identity_api::{
    People, RefreshIdentity, ReproveSignature, Reproved, SignatureRecordQuery, WithdrawSignature,
    Withdrawn,
};

use crate::ApiError;
use crate::access::{Principal, PrincipalKind, RequestPrincipal};
use crate::client_ip::{bucket, client_ip};
use crate::gateway::GatewayState;

/// The most DIDs one `GET /-/api/people` answers.
const MAX_PEOPLE: usize = 100;
/// A handle older than this is re-verified in the background when it is shown.
const HANDLE_STALE_MS: i64 = 24 * 60 * 60 * 1000;
/// How long a handle resolution for a person input may take.
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The people routes, relative to `/-/api`.
pub(crate) fn routes() -> Router<GatewayState> {
    Router::new()
        .route("/people", get(people))
        .route("/people/resolve", post(resolve))
        .route("/people/{did}/signatures/{rkey}/refresh", post(refresh))
}

fn principal(caller: &RequestPrincipal) -> Result<&Principal, ApiError> {
    caller.0.as_ref().ok_or(ApiError::Unauthorized)
}

/// The client address of a request (design D4, *Client address*).
fn address(
    state: &GatewayState,
    peer: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
) -> String {
    bucket(client_ip(
        peer.map(|info| info.0.ip()),
        headers,
        &state.access.config.trusted_proxies,
    ))
}

/// Starts a background refresh of `did`, unless one is already running in this process. It
/// never makes the DID known: the refresh adds at most a cached row.
fn look_up_later(state: &GatewayState, did: String) {
    {
        let mut running = state
            .access
            .lookups
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !running.insert(did.clone()) {
            return;
        }
    }
    let state = state.clone();
    tokio::spawn(async move {
        let refreshed = match identity_handle(&state.client) {
            Ok(handle) => handle
                .send(RefreshIdentity { did: did.clone() })
                .await
                .map(|_| ()),
            Err(error) => Err(error),
        };
        if let Err(error) = refreshed {
            tracing::info!(%did, %error, "a background identity lookup failed");
        }
        state
            .access
            .lookups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&did);
    });
}

/// `GET /-/api/people?did=…&did=…`: what the forge has cached for up to 100 DIDs, answered at
/// once (design D9, *Caching and display*). A known DID maps to `{handle, status}`, where
/// `handle` is the verified handle, `handle.invalid` or `null`; a DID the forge has never
/// resolved maps to `null`. Stale and unknown DIDs are looked up in the background.
async fn people(
    State(state): State<GatewayState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let mode = state.access.config.resolver.mode();
    let query = query.unwrap_or_default();
    let mut dids: Vec<String> = Vec::new();
    for (key, value) in form_pairs(&query) {
        if key != "did" {
            continue;
        }
        let did = Did::parse(&value, mode)
            .map_err(|error| ApiError::BadRequest(format!("{value:?} is not a DID: {error}")))?;
        let did = did.as_str().to_owned();
        if !dids.contains(&did) {
            dids.push(did);
        }
    }
    if dids.len() > MAX_PEOPLE {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_PEOPLE} DIDs per request"
        )));
    }
    let found = identity_handle(&state.client)?
        .send(People { dids: dids.clone() })
        .await?;
    let now = state.access.config.clock.now_ms();
    let client = address(&state, peer.as_ref().map(|peer| &peer.0), &headers);
    let mut answer = Map::new();
    for (did, person) in dids.into_iter().zip(found) {
        match person {
            Some(person) => {
                let stale = person
                    .handle_checked_at
                    .is_none_or(|checked| now - checked > HANDLE_STALE_MS);
                answer.insert(
                    did.clone(),
                    json!({ "handle": person.handle, "status": person.status }),
                );
                if stale {
                    look_up_later(&state, did);
                }
            }
            None => {
                answer.insert(did.clone(), Value::Null);
                // Lookups of DIDs the forge has never seen make it fetch, so they are limited
                // per client address; over the limit the DID simply stays `null` for now.
                if state.access.people_lookups.take(&client).is_ok() {
                    look_up_later(&state, did);
                }
            }
        }
    }
    Ok(Json(Value::Object(answer)))
}

/// The `key=value` pairs of a query string, percent-decoded.
fn form_pairs(query: &str) -> Vec<(String, String)> {
    let Ok(url) = reqwest::Url::parse(&format!("http://query.invalid/?{query}")) else {
        return Vec::new();
    };
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    handle: String,
}

/// `POST /-/api/people/resolve {handle}`: the DID a handle names, verified in both directions,
/// for person inputs. Rate-limited per client address; it does not make the DID known.
async fn resolve(
    State(state): State<GatewayState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<ResolveBody>,
) -> Result<Json<Value>, ApiError> {
    let resolver = state.access.config.resolver.clone();
    let text = body
        .handle
        .trim()
        .trim_start_matches('@')
        .to_ascii_lowercase();
    let handle = Handle::parse(&text, resolver.mode())
        .map_err(|_| ApiError::BadRequest(format!("{:?} is not a handle", body.handle)))?;
    let client = address(&state, peer.as_ref().map(|peer| &peer.0), &headers);
    if let Err(retry_after) = state.access.resolves.take(&client) {
        return Err(ApiError::TooManyRequests {
            message: "too many handle lookups; try again shortly".to_owned(),
            retry_after,
        });
    }
    let task = tokio::spawn(async move { resolver.resolve_verified_handle(&handle).await });
    let resolved = match tokio::time::timeout(RESOLVE_TIMEOUT, task).await {
        Ok(Ok(resolved)) => resolved,
        Ok(Err(_)) | Err(_) => {
            return Err(ApiError::Upstream(format!(
                "{text} could not be resolved now"
            )));
        }
    };
    match resolved {
        Ok(did) => Ok(Json(json!({ "did": did.as_str(), "handle": text }))),
        Err(ResolveError::Unavailable(reason)) => Err(ApiError::Upstream(reason)),
        Err(ResolveError::NotFound(_) | ResolveError::Invalid(_)) => Err(ApiError::NotFound(
            format!("a DID for {text} (verified in both directions)"),
        )),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshBody {
    #[serde(default)]
    withdraw: bool,
    cid: Option<String>,
}

/// `POST /-/api/people/{did}/signatures/{rkey}/refresh` (the `self` route class of design D4).
/// Without a body it re-proves a tracked record now (404 for one the forge does not track).
/// With `{"withdraw": true, "cid"}`, sent by the DID itself from a browser or CLI session, it
/// marks the record revoked as `withdrawn` without fetching, tracked or not.
async fn refresh(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Path((did, rkey)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&caller)?;
    let mode = state.access.config.resolver.mode();
    let did = Did::parse(&did, mode)
        .map_err(|error| ApiError::BadRequest(format!("{did:?} is not a DID: {error}")))?
        .as_str()
        .to_owned();
    if Tid::parse(&rkey).is_err() {
        return Err(ApiError::BadRequest(format!(
            "{rkey:?} is not a record key"
        )));
    }
    let body: RefreshBody = if body.iter().all(u8::is_ascii_whitespace) {
        RefreshBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|error| ApiError::BadRequest(format!("the body: {error}")))?
    };
    let is_self = principal.did == did;
    if !is_self && !principal.owner {
        return Err(ApiError::Forbidden(
            "only the record's own DID, or an owner, may refresh it".to_owned(),
        ));
    }
    let identity = identity_handle(&state.client)?;

    if body.withdraw {
        if !is_self {
            return Err(ApiError::Forbidden(
                "only the signer may withdraw their signature".to_owned(),
            ));
        }
        if matches!(principal.kind, PrincipalKind::Token { .. }) {
            return Err(ApiError::Forbidden(
                "an access token cannot withdraw a signature; sign in with `tt login`".to_owned(),
            ));
        }
        let cid = body.cid.ok_or_else(|| {
            ApiError::BadRequest("a withdrawal names the record's cid".to_owned())
        })?;
        let tracked = identity
            .send(SignatureRecordQuery {
                did: did.clone(),
                rkey: rkey.clone(),
            })
            .await?
            .is_some();
        // Withdrawing a tracked record only sets `revoked_at`, so only untracked inserts are
        // rate-limited.
        if !tracked {
            take_self_call(&state, &did)?;
        }
        let withdrawn = identity
            .send(WithdrawSignature {
                did: did.clone(),
                rkey: rkey.clone(),
                cid,
            })
            .await?;
        return match withdrawn {
            Withdrawn::Tracked | Withdrawn::PreWithdrawn => Ok(Json(json!({
                "did": did, "rkey": rkey, "revoked": true, "reason": "withdrawn",
                "outcome": if withdrawn == Withdrawn::Tracked { "withdrawn" } else { "pre_withdrawn" },
            }))),
            Withdrawn::Capped { message } => Err(ApiError::TooManyRequests {
                message,
                retry_after: 3_600,
            }),
            Withdrawn::Invalid { message } => Err(ApiError::BadRequest(message)),
        };
    }
    if body.cid.is_some() {
        return Err(ApiError::BadRequest(
            "`cid` is sent only with `withdraw`".to_owned(),
        ));
    }

    // A signer's own re-proof makes the forge fetch from a host the caller chooses, so it is
    // rate-limited before anything is fetched.
    if is_self {
        let tracked = identity
            .send(SignatureRecordQuery {
                did: did.clone(),
                rkey: rkey.clone(),
            })
            .await?;
        if tracked.is_none() {
            return Err(ApiError::NotFound(format!(
                "signature record at://{did}/academy.rawkode.tartan.signature/{rkey} on this forge"
            )));
        }
        take_self_call(&state, &did)?;
    }
    let reproved = identity
        .send(ReproveSignature {
            did: did.clone(),
            rkey: rkey.clone(),
        })
        .await?;
    let answer = match reproved {
        Reproved::Untracked => {
            return Err(ApiError::NotFound(format!(
                "signature record at://{did}/academy.rawkode.tartan.signature/{rkey} on this forge"
            )));
        }
        Reproved::Present => json!({"outcome": "present", "revoked": false}),
        Reproved::AlreadyRevoked { reason } => {
            json!({"outcome": "already_revoked", "revoked": true, "reason": reason})
        }
        Reproved::Revoked { reason } => {
            json!({"outcome": "revoked", "revoked": true, "reason": reason})
        }
        Reproved::Transient { reason } => {
            json!({"outcome": "transient", "revoked": false, "detail": reason})
        }
    };
    let mut answer = answer;
    answer["did"] = json!(did);
    answer["rkey"] = json!(rkey);
    Ok(Json(answer))
}

/// Takes one `self` call for `did`, or answers 429 with `Retry-After`.
fn take_self_call(state: &GatewayState, did: &str) -> Result<(), ApiError> {
    state
        .access
        .self_calls
        .take(did)
        .map_err(|retry_after| ApiError::TooManyRequests {
            message: "too many signature refreshes for this account; try again later".to_owned(),
            retry_after,
        })
}

#[cfg(test)]
mod tests {
    use super::form_pairs;

    #[test]
    fn repeated_query_keys() {
        assert_eq!(
            form_pairs("did=did%3Aplc%3Aa&did=did:web:b.example&x=1"),
            vec![
                ("did".to_owned(), "did:plc:a".to_owned()),
                ("did".to_owned(), "did:web:b.example".to_owned()),
                ("x".to_owned(), "1".to_owned()),
            ]
        );
    }
}
