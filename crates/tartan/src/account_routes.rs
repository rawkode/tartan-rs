//! The caller's own account: `GET /-/api/me`, sessions and access tokens (design D15).

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde_json::{Value, json};
use tartan_forge::identity_handle;
use tartan_identity_api::{
    CreateToken, EndSession, ListSessions, ListTokens, NewToken, People, RevokeToken, TokenInfo,
    TokenRefusal, TokenRequest,
};

use crate::ApiError;
use crate::access::{Principal, RequestPrincipal};
use crate::gateway::GatewayState;

/// The account routes, relative to `/-/api`.
pub(crate) fn routes() -> Router<GatewayState> {
    Router::new()
        .route("/me", get(me))
        .route("/sessions", get(list_sessions))
        .route("/sessions/current", delete(end_current_session))
        .route("/sessions/{id}", delete(end_session))
        .route("/tokens", get(list_tokens).post(create_token))
        .route("/tokens/{id}", delete(revoke_token))
}

fn principal(caller: &RequestPrincipal) -> Result<&Principal, ApiError> {
    caller.0.as_ref().ok_or(ApiError::Unauthorized)
}

async fn me(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
) -> Result<Json<Value>, ApiError> {
    let dev_identity = state.access.config.resolver.mode().is_development();
    let Some(principal) = caller.0 else {
        return Ok(Json(
            json!({"anonymous": true, "dev_identity": dev_identity}),
        ));
    };
    let handle = identity_handle(&state.client)?
        .send(People {
            dids: vec![principal.did.clone()],
        })
        .await?
        .into_iter()
        .flatten()
        .next()
        .and_then(|person| person.handle);
    Ok(Json(json!({
        "did": principal.did,
        "handle": handle,
        "owner": principal.owner,
        "kind": principal.kind_name(),
        "rights": principal.rights.names(),
        "dev_identity": dev_identity,
    })))
}

async fn list_sessions(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&caller)?;
    let current = principal.session_id().unwrap_or_default();
    let sessions = identity_handle(&state.client)?
        .send(ListSessions {
            did: principal.did.clone(),
        })
        .await?;
    let sessions: Vec<Value> = sessions
        .into_iter()
        .map(|session| {
            let mut value = serde_json::to_value(&session).unwrap_or_default();
            value["current"] = json!(session.id == current);
            value
        })
        .collect();
    Ok(Json(Value::Array(sessions)))
}

async fn end(
    state: &GatewayState,
    principal: &Principal,
    id: &str,
) -> Result<StatusCode, ApiError> {
    let ended = identity_handle(&state.client)?
        .send(EndSession {
            did: principal.did.clone(),
            id: id.to_owned(),
        })
        .await?;
    if !ended {
        return Err(ApiError::NotFound(format!("session {id}")));
    }
    state.access.forget(id);
    Ok(StatusCode::NO_CONTENT)
}

async fn end_current_session(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
) -> Result<StatusCode, ApiError> {
    let principal = principal(&caller)?;
    let id = principal
        .session_id()
        .ok_or_else(|| ApiError::Forbidden("an access token has no session".to_owned()))?
        .to_owned();
    end(&state, principal, &id).await
}

async fn end_session(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    end(&state, principal(&caller)?, &id).await
}

async fn list_tokens(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
) -> Result<Json<Vec<TokenInfo>>, ApiError> {
    let principal = principal(&caller)?;
    Ok(Json(
        identity_handle(&state.client)?
            .send(ListTokens {
                did: principal.did.clone(),
            })
            .await?,
    ))
}

fn refused(refusal: TokenRefusal) -> ApiError {
    match refusal {
        TokenRefusal::Invalid { message } => ApiError::BadRequest(message),
        TokenRefusal::NoSession => ApiError::Unauthorized,
        TokenRefusal::TooMany => ApiError::Conflict(refusal.to_string()),
        TokenRefusal::NotFound => ApiError::NotFound("token".to_owned()),
        TokenRefusal::NotOwner { .. } => ApiError::Forbidden(refusal.to_string()),
    }
}

async fn create_token(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Json(request): Json<TokenRequest>,
) -> Result<(StatusCode, Json<NewToken>), ApiError> {
    let principal = principal(&caller)?;
    let session_id = principal
        .session_id()
        .ok_or_else(|| ApiError::Forbidden("tokens never create tokens".to_owned()))?;
    let created = identity_handle(&state.client)?
        .send(CreateToken {
            session_id: session_id.to_owned(),
            request,
        })
        .await?
        .map_err(refused)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn revoke_token(
    State(state): State<GatewayState>,
    Extension(caller): Extension<RequestPrincipal>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let principal = principal(&caller)?;
    identity_handle(&state.client)?
        .send(RevokeToken {
            by: principal.did.clone(),
            id: id.clone(),
        })
        .await?
        .map_err(refused)?;
    state.access.forget(&id);
    Ok(StatusCode::NO_CONTENT)
}
