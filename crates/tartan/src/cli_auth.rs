//! What `tt` needs from the forge outside `/-/api` (design D7 *tt's OAuth client*, D16 *Forge
//! login hand-off*): `tt`'s OAuth client metadata, and the two steps that hand a CLI session to
//! `tt login`.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tartan_forge::identity_handle;
use tartan_identity_api::{
    CliRefusal, CredentialKind, IssueCliCode, MAX_DEVICE_CHARS, RedeemCliCode,
};

use crate::ApiError;
use crate::access::Access;
use crate::client_ip::{bucket, client_ip};
use crate::gateway::GatewayState;

/// The scope `tt`'s client declares: `atproto` and both signature-record permissions,
/// verbatim, because authorization servers compare scope tokens literally (design D7).
pub const TT_SCOPE: &str = "atproto repo:academy.rawkode.tartan.signature?action=create \
                            repo:academy.rawkode.tartan.signature?action=delete";

/// The routes, at the root.
pub(crate) fn routes() -> Router<GatewayState> {
    Router::new()
        .route("/-/auth/tt/client-metadata.json", get(tt_client_metadata))
        .route("/-/auth/cli/approve", post(approve))
        .route("/-/auth/cli/token", post(token))
}

/// `tt`'s public native client metadata (design D7, *tt's OAuth client*). It is written by
/// hand: jacquard-oauth's metadata helper gives every non-localhost client id
/// `application_type` `web`, and loopback redirects are accepted only for `native` clients.
pub fn tt_client_metadata_document(access: &Access) -> Value {
    let public = &access.config.public_url;
    json!({
        "client_id": public.join("/-/auth/tt/client-metadata.json"),
        "client_name": format!("tt ({})", public.host()),
        "client_uri": public.origin(),
        "application_type": "native",
        "redirect_uris": ["http://127.0.0.1/callback", "http://[::1]/callback"],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": TT_SCOPE,
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

async fn tt_client_metadata(State(state): State<GatewayState>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        Json(tt_client_metadata_document(&state.access)),
    )
        .into_response()
}

/// Whether `value` is `len` base64url characters.
fn is_base64url_of(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    port: u64,
    state: String,
    challenge: String,
    device: String,
}

/// `POST /-/auth/cli/approve {port, state, challenge, device}` from the CLI approval page:
/// cookie-authenticated and Origin-checked. It issues a one-time code and answers the loopback
/// redirect for `tt`, always on `127.0.0.1` with the requested port.
async fn approve(
    State(state): State<GatewayState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let access = &state.access;
    if let Some(refused) = access.check_origin(&headers) {
        return refused;
    }
    let Some(secret) = access.session_secret(&headers) else {
        return ApiError::Unauthorized.into_response();
    };
    let authenticated = match access
        .lookup(&state.client, secret, CredentialKind::BrowserSession)
        .await
    {
        Ok(Some(authenticated)) => authenticated,
        Ok(None) => return ApiError::Unauthorized.into_response(),
        Err(error) => return error.into_response(),
    };
    let body: ApproveBody = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(error) => {
            return ApiError::BadRequest(format!(
                "an approval is {{port, state, challenge, device}}: {error}"
            ))
            .into_response();
        }
    };
    if !(1024..=65535).contains(&body.port) {
        return ApiError::BadRequest("port must be an integer from 1024 to 65535".to_owned())
            .into_response();
    }
    if !is_base64url_of(&body.state, 43) {
        return ApiError::BadRequest("state must be 43 base64url characters".to_owned())
            .into_response();
    }
    if !is_base64url_of(&body.challenge, 43) {
        return ApiError::BadRequest(
            "challenge must be an S256 challenge: 43 base64url characters".to_owned(),
        )
        .into_response();
    }
    if body.device.chars().count() > MAX_DEVICE_CHARS {
        return ApiError::BadRequest(format!(
            "the device name is at most {MAX_DEVICE_CHARS} characters"
        ))
        .into_response();
    }
    let client = bucket(client_ip(
        peer.map(|info| info.0.0.ip()),
        &headers,
        &access.config.trusted_proxies,
    ));
    let issued = match identity_handle(&state.client) {
        Ok(identity) => {
            identity
                .send(IssueCliCode {
                    did: authenticated.did.clone(),
                    challenge: body.challenge,
                    device: body.device,
                    client_ip: client,
                })
                .await
        }
        Err(error) => Err(error),
    };
    let code = match issued {
        Ok(Ok(code)) => code,
        Ok(Err(CliRefusal::TooMany)) => {
            return ApiError::TooManyRequests {
                message: CliRefusal::TooMany.to_string(),
                retry_after: 60,
            }
            .into_response();
        }
        Ok(Err(refusal)) => return ApiError::BadRequest(refusal.to_string()).into_response(),
        Err(error) => return ApiError::Unavailable(error).into_response(),
    };
    let redirect = format!(
        "http://127.0.0.1:{}/forge?code={code}&state={}",
        body.port, body.state
    );
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "redirect": redirect })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenBody {
    code: String,
    code_verifier: String,
}

/// `POST /-/auth/cli/token {code, code_verifier}`: exchanges a CLI code for a `tartan_cli_`
/// session, answered once. Unauthenticated and rate-limited per client address.
async fn token(
    State(state): State<GatewayState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let client = bucket(client_ip(
        peer.map(|info| info.0.0.ip()),
        &headers,
        &state.access.config.trusted_proxies,
    ));
    if let Err(retry_after) = state.access.cli_tokens.take(&client) {
        return ApiError::TooManyRequests {
            message: "too many CLI code exchanges; try again shortly".to_owned(),
            retry_after,
        }
        .into_response();
    }
    let body: TokenBody = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(error) => {
            return ApiError::BadRequest(format!(
                "a code exchange is {{code, code_verifier}}: {error}"
            ))
            .into_response();
        }
    };
    let redeemed = match identity_handle(&state.client) {
        Ok(identity) => {
            identity
                .send(RedeemCliCode {
                    code: body.code,
                    verifier: body.code_verifier,
                })
                .await
        }
        Err(error) => Err(error),
    };
    match redeemed {
        Ok(Ok(session)) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({
                "session": session.secret,
                "id": session.id,
                "did": session.did,
                "expires_at": session.expires_at,
            })),
        )
            .into_response(),
        Ok(Err(refusal)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant", "message": refusal.to_string() })),
        )
            .into_response(),
        Err(error) => ApiError::Unavailable(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_lengths() {
        assert!(is_base64url_of(&"a".repeat(43), 43));
        assert!(!is_base64url_of(&"a".repeat(42), 43));
        assert!(!is_base64url_of(&format!("{}=", "a".repeat(42)), 43));
        assert!(!is_base64url_of(&format!("{}/", "a".repeat(42)), 43));
    }

    #[test]
    fn approvals_carry_only_known_fields() {
        let body = r#"{"port":8080,"state":"s","challenge":"c","device":"d","redirect_uri":"http://evil.example:8080"}"#;
        assert!(serde_json::from_str::<ApproveBody>(body).is_err());
        let body = r#"{"port":"8080","state":"s","challenge":"c","device":"d"}"#;
        assert!(serde_json::from_str::<ApproveBody>(body).is_err());
    }
}
