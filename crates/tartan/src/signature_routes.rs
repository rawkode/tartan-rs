//! Signature results, proof bundles, re-checks and configuration status (design D8, D11, D15).

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use object_store::ObjectStoreExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tartan_forge::{
    BranchSignatures, ConfigStatusQuery, ProofReferenced, RecheckOutcome, RecheckSignature,
    RefSignatures, SignatureQuery, repo_handle,
};

use crate::ApiError;
use crate::access::RequestPrincipal;
use crate::gateway::{GatewayState, resolve_repo};

pub(crate) fn routes() -> Router<GatewayState> {
    Router::new()
        .route(
            "/orgs/{org}/repos/{repo}/commits/{sha}/signature",
            get(signature),
        )
        .route(
            "/orgs/{org}/repos/{repo}/commits/{sha}/signature/recheck",
            post(recheck),
        )
        .route("/orgs/{org}/repos/{repo}/proofs/{sha256}", get(proof))
        .route("/orgs/{org}/repos/{repo}/signatures", get(ref_signatures))
        .route("/orgs/{org}/repos/{repo}/config/status", get(config_status))
}

/// Whether `text` is a full hex object id.
fn check_oid(text: &str) -> Result<String, ApiError> {
    let valid = matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit());
    if valid {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(ApiError::BadRequest(format!(
            "{text:?} is not a full object id"
        )))
    }
}

/// Adds the proof download URL to a stored result.
fn with_proof_url(org: &str, repo: &str, mut view: Value) -> Value {
    if let Some(sha) = view.get("proof_sha256").and_then(Value::as_str) {
        view["proof_url"] = json!(format!("/-/api/orgs/{org}/repos/{repo}/proofs/{sha}"));
    }
    view
}

async fn signature(
    State(state): State<GatewayState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let oid = check_oid(&sha)?;
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let view = repo_handle(&state.client, &info.id, info.pool)?
        .send(SignatureQuery { oid: oid.clone() })
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no signature result for {oid}")))?;
    Ok(Json(with_proof_url(&org, &repo, view)))
}

/// `POST …/commits/{sha}/signature/recheck`: an owner (`write`) re-queues an `unverifiable`
/// or unchecked result; anyone else may re-check only a result stored with their own DID, and
/// their call is counted by the per-DID `self` rate limit.
async fn recheck(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let principal = caller.0.as_ref().ok_or(ApiError::Unauthorized)?;
    let owner = principal.owner && principal.rights.write;
    if owner {
        return requeue(&state, &org, &repo, &sha, None).await;
    }
    state
        .access
        .self_calls
        .take(&principal.did)
        .map_err(|retry_after| ApiError::TooManyRequests {
            message: "too many signature re-checks for this account; try again later".to_owned(),
            retry_after,
        })?;
    // Anyone who signs in may call this route, including people who may not read the forge, so
    // every way of not finding their result (an unknown org or repository, a malformed slug or
    // object id, or no result of theirs) gets the same answer: it reveals nothing about which
    // orgs and repositories exist.
    let not_found =
        || ApiError::NotFound(format!("no unverifiable result signed by you for {sha}"));
    match requeue(&state, &org, &repo, &sha, Some(principal.did.clone())).await {
        Err(ApiError::NotFound(_) | ApiError::BadRequest(_)) => Err(not_found()),
        other => other,
    }
}

async fn requeue(
    state: &GatewayState,
    org: &str,
    repo: &str,
    sha: &str,
    signer: Option<String>,
) -> Result<Json<Value>, ApiError> {
    let oid = check_oid(sha)?;
    let info = resolve_repo(&state.client, org, repo).await?;
    let outcome = repo_handle(&state.client, &info.id, info.pool)?
        .send(RecheckSignature {
            oid: oid.clone(),
            signer,
        })
        .await?;
    match outcome {
        RecheckOutcome::Requeued { updates } => Ok(Json(json!({
            "oid": oid, "status": "pending", "updates": updates,
        }))),
        RecheckOutcome::Conflict => Err(ApiError::Conflict(format!(
            "{oid} has no result that can be checked again"
        ))),
        RecheckOutcome::NotFound => Err(ApiError::NotFound(format!(
            "no unverifiable result signed by you for {oid}"
        ))),
    }
}

async fn proof(
    State(state): State<GatewayState>,
    Path((org, repo, sha256)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let valid = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid {
        return Err(ApiError::BadRequest(format!(
            "{sha256:?} is not a SHA-256 digest"
        )));
    }
    let sha256 = sha256.to_ascii_lowercase();
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let referenced = repo_handle(&state.client, &info.id, info.pool)?
        .send(ProofReferenced {
            sha256: sha256.clone(),
        })
        .await?;
    let missing = || ApiError::NotFound(format!("proof bundle {sha256} in {org}/{repo}"));
    if !referenced {
        return Err(missing());
    }
    let path = object_store::path::Path::from(format!("proofs/sha256/{sha256}.json"));
    let bytes = match state.registry.objects().get(&path).await {
        Ok(found) => found
            .bytes()
            .await
            .map_err(|error| ApiError::Unavailable(error.into()))?,
        Err(object_store::Error::NotFound { .. }) => return Err(missing()),
        Err(error) => return Err(ApiError::Unavailable(error.into())),
    };
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

#[derive(Debug, Deserialize)]
struct RefQuery {
    #[serde(rename = "ref")]
    ref_name: Option<String>,
}

/// `GET …/signatures?ref=<ref>` answers one ref (404 when it was never checked); without
/// `ref`, it lists every branch head (design D15, *Answer shapes the web UI reads*).
async fn ref_signatures(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
    Query(query): Query<RefQuery>,
) -> Result<Json<Value>, ApiError> {
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let handle = repo_handle(&state.client, &info.id, info.pool)?;
    let Some(ref_name) = query.ref_name else {
        let branches = handle.send(BranchSignatures {}).await?;
        return Ok(Json(Value::Array(
            branches
                .into_iter()
                .map(|view| with_proof_url(&org, &repo, view))
                .collect(),
        )));
    };
    let ref_name = if ref_name.starts_with("refs/") {
        ref_name
    } else {
        format!("refs/heads/{ref_name}")
    };
    let view = handle
        .send(RefSignatures {
            ref_name: ref_name.clone(),
        })
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("no signature check of {ref_name}")))?;
    Ok(Json(with_proof_url(&org, &repo, view)))
}

async fn config_status(
    State(state): State<GatewayState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let info = resolve_repo(&state.client, &org, &repo).await?;
    let status = repo_handle(&state.client, &info.id, info.pool)?
        .send(ConfigStatusQuery {})
        .await?;
    Ok(Json(status))
}
