//! The authorization server, following the ATProto OAuth profile as `@atproto/oauth-provider`
//! implements it: client metadata documents as client ids, PAR, PKCE S256, DPoP with server
//! nonces, `private_key_jwt` for confidential clients and `none` for public native clients.
//!
//! - A client's metadata is fetched on every PAR and token request (no cache), so a forge that
//!   stops serving it is noticed at once, as the reference server notices it after its cache.
//! - The `http://localhost` client id of the ATProto profile is accepted: its metadata comes
//!   from its query (`redirect_uri`, `scope`), as a public native client.
//! - Requested scope tokens must each appear literally in the client's declared `scope`.
//! - Loopback redirect URIs (`http://127.0.0.1`, `http://[::1]`) registered without a port
//!   match any port (RFC 8252 §7.3).
//! - Refresh tokens are single-use; presenting a used one revokes the whole session.
//! - The authorize page acts as the person named by `login_hint` (or `as`) without asking, so
//!   following the authorization URL with a plain HTTP client completes the flow; with neither
//!   it lists the people, and `deny=1` refuses.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::jwt::{self, DpopProof};
use crate::server::Conn;
use crate::util::{self, all, first, form_pairs, random_token};
use crate::{Shared, State as DevState};

const CLIENT_ASSERTION_TYPE: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";
const REQUEST_LIFETIME_MS: u64 = 300_000;
const CODE_LIFETIME_MS: u64 = 60_000;

/// A pushed authorization request.
#[derive(Debug, Clone)]
struct Pending {
    client_id: String,
    redirect_uri: String,
    state: String,
    scope: String,
    challenge: String,
    jkt: String,
    login_hint: Option<String>,
    refresh: bool,
    public: bool,
    expires_ms: u64,
}

#[derive(Debug, Clone)]
struct Code {
    pending: Pending,
    did: String,
    expires_ms: u64,
}

/// A session a person granted a client.
#[derive(Debug, Clone)]
pub(crate) struct Grant {
    pub did: String,
    pub client_id: String,
    pub scope: String,
    jkt: String,
    public: bool,
    created_ms: u64,
    pub revoked: bool,
}

/// An issued access token.
#[derive(Debug, Clone)]
pub(crate) struct Access {
    pub grant: u64,
    pub jkt: String,
    pub expires_ms: u64,
}

/// The authorization server's state.
pub(crate) struct OAuthState {
    pub as_nonce: String,
    pub rs_nonce: String,
    jtis: HashMap<String, u64>,
    requests: HashMap<String, Pending>,
    codes: HashMap<String, Code>,
    pub grants: HashMap<u64, Grant>,
    next_grant: u64,
    pub access: HashMap<String, Access>,
    refresh: HashMap<String, (u64, bool)>,
    pub par_bodies: Vec<Vec<(String, String)>>,
}

impl OAuthState {
    pub(crate) fn new() -> Self {
        OAuthState {
            as_nonce: random_token(16),
            rs_nonce: random_token(16),
            jtis: HashMap::new(),
            requests: HashMap::new(),
            codes: HashMap::new(),
            grants: HashMap::new(),
            next_grant: 1,
            access: HashMap::new(),
            refresh: HashMap::new(),
            par_bodies: Vec::new(),
        }
    }

    pub(crate) fn rotate_nonces(&mut self) {
        self.as_nonce = random_token(16);
        self.rs_nonce = random_token(16);
    }

    /// Records `jti` as used; false when it was already used.
    pub(crate) fn fresh_jti(&mut self, jti: &str, now_ms: u64) -> bool {
        self.jtis.retain(|_, expires| *expires > now_ms);
        self.jtis
            .insert(jti.to_owned(), now_ms + 10 * 60_000)
            .is_none()
    }

    /// The grant behind a live access token.
    pub(crate) fn live_access(&self, token: &str, now_ms: u64) -> Option<(&Access, &Grant)> {
        let access = self.access.get(token)?;
        let grant = self.grants.get(&access.grant)?;
        (access.expires_ms > now_ms && !grant.revoked).then_some((access, grant))
    }

    fn issue(&mut self, grant: u64, jkt: &str, lifetime_ms: u64, now_ms: u64) -> (String, u64) {
        let token = random_token(32);
        self.access.insert(
            token.clone(),
            Access {
                grant,
                jkt: jkt.to_owned(),
                expires_ms: now_ms + lifetime_ms,
            },
        );
        (token, lifetime_ms / 1000)
    }
}

/// An OAuth error answer.
struct OAuthError {
    status: StatusCode,
    error: String,
    description: String,
}

impl OAuthError {
    fn new(error: &str, description: impl Into<String>) -> Self {
        OAuthError {
            status: StatusCode::BAD_REQUEST,
            error: error.to_owned(),
            description: description.into(),
        }
    }

    fn client(description: impl Into<String>) -> Self {
        let mut error = OAuthError::new("invalid_client", description);
        error.status = StatusCode::UNAUTHORIZED;
        error
    }

    fn into_response(self, nonce: &str) -> Response {
        let mut response = (
            self.status,
            Json(json!({"error": self.error, "error_description": self.description})),
        )
            .into_response();
        add_nonce(&mut response, nonce);
        response
    }
}

fn add_nonce(response: &mut Response, nonce: &str) {
    if let Ok(value) = HeaderValue::from_str(nonce) {
        response.headers_mut().insert("DPoP-Nonce", value);
    }
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("DPoP-Nonce"),
    );
}

fn endpoint(issuer: &str, path: &str) -> String {
    format!("{issuer}{path}")
}

/// `GET /.well-known/oauth-authorization-server`.
pub(crate) async fn metadata(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
) -> Response {
    let state = shared.state.lock().await;
    if state.switches.auth_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let issuer = Shared::issuer(&state).to_owned();
    let mut metadata = json!({
        "issuer": issuer,
        "authorization_endpoint": endpoint(&issuer, "/oauth/authorize"),
        "token_endpoint": endpoint(&issuer, "/oauth/token"),
        "pushed_authorization_request_endpoint": endpoint(&issuer, "/oauth/par"),
        "revocation_endpoint": endpoint(&issuer, "/oauth/revoke"),
        "require_pushed_authorization_requests": true,
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none", "private_key_jwt"],
        "token_endpoint_auth_signing_alg_values_supported": ["ES256"],
        "dpop_signing_alg_values_supported": ["ES256"],
        "scopes_supported": ["atproto", "transition:generic"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
        "request_parameter_supported": false,
        "request_uri_parameter_supported": true,
    });
    if state.switches.omit_iss_parameter_supported
        && let Some(map) = metadata.as_object_mut()
    {
        map.remove("authorization_response_iss_parameter_supported");
    }
    Json(metadata).into_response()
}

/// Checks a request's DPoP proof against the authorization server's nonce.
fn check_as_dpop(
    state: &mut DevState,
    headers: &HeaderMap,
    url: &str,
) -> Result<DpopProof, OAuthError> {
    let proof = headers
        .get("DPoP")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| OAuthError::new("invalid_dpop_proof", "DPoP proof required"))?;
    let proof = jwt::verify_dpop(proof, "POST", url, (util::now_ms() / 1000) as i64)
        .map_err(|error| OAuthError::new("invalid_dpop_proof", error))?;
    if proof.nonce.as_deref() != Some(state.oauth.as_nonce.as_str()) {
        return Err(OAuthError::new(
            "use_dpop_nonce",
            "Authorization server requires nonce in DPoP proof",
        ));
    }
    if !state.oauth.fresh_jti(&proof.jti, util::now_ms()) {
        return Err(OAuthError::new("invalid_dpop_proof", "DPoP proof replayed"));
    }
    Ok(proof)
}

/// What the server knows of a client after authenticating it.
#[derive(Debug, Clone)]
struct Client {
    id: String,
    redirect_uris: Vec<String>,
    scope: Vec<String>,
    refresh: bool,
    public: bool,
}

/// Authenticates the client a token-endpoint request names.
async fn authenticate_client(
    shared: &Arc<Shared>,
    pairs: &[(String, String)],
    issuer: &str,
) -> Result<Client, OAuthError> {
    let client_id = first(pairs, "client_id")
        .ok_or_else(|| OAuthError::new("invalid_request", "client_id is required"))?
        .to_owned();
    let failing = {
        let state = shared.state.lock().await;
        state
            .switches
            .fail_client_metadata
            .iter()
            .any(|id| id == "*" || *id == client_id)
    };
    if failing {
        return Err(OAuthError::new(
            "invalid_client_metadata",
            format!("Unable to obtain client metadata for \"{client_id}\""),
        ));
    }
    let metadata = client_metadata(shared, &client_id).await?;
    let client = validate_metadata(&client_id, &metadata)
        .map_err(|error| OAuthError::new("invalid_client_metadata", error))?;
    let assertion_type = first(pairs, "client_assertion_type");
    let assertion = first(pairs, "client_assertion");
    if client.public {
        if assertion.is_some() || assertion_type.is_some() {
            return Err(OAuthError::client(
                "a client using token_endpoint_auth_method none sent a client assertion",
            ));
        }
        return Ok(client);
    }
    if assertion_type != Some(CLIENT_ASSERTION_TYPE) {
        return Err(OAuthError::client("a client assertion is required"));
    }
    let assertion =
        assertion.ok_or_else(|| OAuthError::client("a client assertion is required"))?;
    let keys = client_keys(shared, &metadata).await?;
    verify_assertion(shared, assertion, &client_id, issuer, &keys).await?;
    Ok(client)
}

/// The metadata document behind `client_id`: the localhost client's, or a fetched document.
async fn client_metadata(shared: &Arc<Shared>, client_id: &str) -> Result<Value, OAuthError> {
    let invalid = |message: String| OAuthError::new("invalid_client_metadata", message);
    let url = url::Url::parse(client_id)
        .map_err(|_| invalid(format!("client_id {client_id} is not a URL")))?;
    if url.scheme() == "http" && url.host_str() == Some("localhost") {
        return localhost_metadata(client_id, &url).map_err(invalid);
    }
    if url.fragment().is_some() || url.path() == "/" {
        return Err(invalid(format!(
            "client_id {client_id} is not a metadata document URL"
        )));
    }
    let response = shared
        .http
        .get(url)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| invalid(format!("Unable to obtain client metadata: {error}")))?;
    if !response.status().is_success() {
        return Err(invalid(format!(
            "Unable to obtain client metadata: {} answered {}",
            client_id,
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| invalid(format!("Unable to obtain client metadata: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("Invalid client metadata: {error}")))
}

/// The ATProto profile's development client: `http://localhost` with no port and no path,
/// whose `redirect_uri` and `scope` query parameters are its metadata.
fn localhost_metadata(client_id: &str, url: &url::Url) -> Result<Value, String> {
    if url.port().is_some() || url.path() != "/" || url.fragment().is_some() {
        return Err("a localhost client_id has no port and no path".into());
    }
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut redirect_uris: Vec<&str> = all(&pairs, "redirect_uri");
    if redirect_uris.is_empty() {
        redirect_uris = vec!["http://127.0.0.1/", "http://[::1]/"];
    }
    Ok(json!({
        "client_id": client_id,
        "client_name": "Development client",
        "application_type": "native",
        "redirect_uris": redirect_uris,
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": first(&pairs, "scope").unwrap_or("atproto"),
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    }))
}

fn is_loopback_ip(url: &url::Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1") | Some("[::1]"))
}

/// Validates a client metadata document.
fn validate_metadata(client_id: &str, metadata: &Value) -> Result<Client, String> {
    let strings = |key: &str| -> Vec<String> {
        metadata
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |key: &str| metadata.get(key).and_then(Value::as_str);
    if text("client_id") != Some(client_id) {
        return Err("client_id of the metadata does not match its URL".into());
    }
    let application_type = text("application_type").unwrap_or("web");
    if !matches!(application_type, "web" | "native") {
        return Err(format!("unknown application_type {application_type}"));
    }
    let redirect_uris = strings("redirect_uris");
    if redirect_uris.is_empty() {
        return Err("redirect_uris is empty".into());
    }
    for uri in &redirect_uris {
        let url = url::Url::parse(uri).map_err(|_| format!("invalid redirect URI {uri}"))?;
        let allowed = match url.scheme() {
            "https" => true,
            "http" => is_loopback_ip(&url),
            // A native client may use a private-use scheme (RFC 8252 §7.1).
            _ => application_type == "native" && url.scheme().contains('.'),
        };
        if !allowed {
            return Err(format!(
                "redirect URI {uri} must be https or an http loopback IP address"
            ));
        }
    }
    if !strings("response_types").iter().any(|t| t == "code") {
        return Err("response_types must include code".into());
    }
    let grant_types = strings("grant_types");
    if !grant_types.iter().any(|g| g == "authorization_code") {
        return Err("grant_types must include authorization_code".into());
    }
    if metadata
        .get("dpop_bound_access_tokens")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("dpop_bound_access_tokens must be true".into());
    }
    let scope: Vec<String> = text("scope")
        .unwrap_or_default()
        .split(' ')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect();
    if !scope.iter().any(|token| token == "atproto") {
        return Err("scope must include atproto".into());
    }
    let public = match text("token_endpoint_auth_method") {
        Some("none") => true,
        Some("private_key_jwt") => {
            if metadata.get("jwks").is_none() && metadata.get("jwks_uri").is_none() {
                return Err("private_key_jwt needs jwks or jwks_uri".into());
            }
            if text("token_endpoint_auth_signing_alg").is_some_and(|alg| alg != "ES256") {
                return Err("token_endpoint_auth_signing_alg must be ES256".into());
            }
            false
        }
        other => {
            return Err(format!(
                "unsupported token_endpoint_auth_method {}",
                other.unwrap_or("(absent)")
            ));
        }
    };
    Ok(Client {
        id: client_id.to_owned(),
        redirect_uris,
        scope,
        refresh: grant_types.iter().any(|g| g == "refresh_token"),
        public,
    })
}

/// The client's signing keys from `jwks` or `jwks_uri`.
async fn client_keys(
    shared: &Arc<Shared>,
    metadata: &Value,
) -> Result<Vec<(Option<String>, p256::PublicKey)>, OAuthError> {
    let jwks = match (
        metadata.get("jwks"),
        metadata.get("jwks_uri").and_then(Value::as_str),
    ) {
        (Some(jwks), _) => jwks.clone(),
        (None, Some(uri)) => {
            let fetched = async {
                let response = shared.http.get(uri).send().await.ok()?;
                if !response.status().is_success() {
                    return None;
                }
                serde_json::from_slice::<Value>(&response.bytes().await.ok()?).ok()
            }
            .await;
            fetched.ok_or_else(|| {
                OAuthError::new(
                    "invalid_client_metadata",
                    format!("Unable to obtain the client's jwks_uri {uri}"),
                )
            })?
        }
        (None, None) => Value::Null,
    };
    Ok(jwks
        .get("keys")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(|jwk| {
                    let kid = jwk.get("kid").and_then(Value::as_str).map(str::to_owned);
                    jwt::public_key(jwk).map(|key| (kid, key))
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn verify_assertion(
    shared: &Arc<Shared>,
    assertion: &str,
    client_id: &str,
    issuer: &str,
    keys: &[(Option<String>, p256::PublicKey)],
) -> Result<(), OAuthError> {
    let jwt = jwt::decode(assertion)
        .ok_or_else(|| OAuthError::client("the client assertion is not a JWT"))?;
    if !jwt.is_es256() {
        return Err(OAuthError::client("the client assertion is not ES256"));
    }
    let kid = jwt.header.get("kid").and_then(Value::as_str);
    let verified = keys
        .iter()
        .filter(|(key_id, _)| kid.is_none() || key_id.as_deref() == kid)
        .any(|(_, key)| jwt.verify(key));
    if !verified {
        return Err(OAuthError::client(
            "the client assertion is not signed by a key of the client's JWKS",
        ));
    }
    if jwt.claim_str("iss") != Some(client_id) || jwt.claim_str("sub") != Some(client_id) {
        return Err(OAuthError::client(
            "the client assertion's iss and sub must be the client_id",
        ));
    }
    let audience_ok = match jwt.claims.get("aud") {
        Some(Value::String(aud)) => aud == issuer,
        Some(Value::Array(auds)) => auds.iter().any(|aud| aud.as_str() == Some(issuer)),
        _ => false,
    };
    if !audience_ok {
        return Err(OAuthError::client(
            "the client assertion's aud is not the issuer",
        ));
    }
    let now = (util::now_ms() / 1000) as i64;
    if jwt.claim_i64("exp").is_some_and(|exp| exp < now) {
        return Err(OAuthError::client("the client assertion has expired"));
    }
    if jwt
        .claim_i64("iat")
        .is_none_or(|iat| (now - iat).abs() > 300)
    {
        return Err(OAuthError::client(
            "the client assertion's iat is missing or too far from now",
        ));
    }
    let jti = jwt
        .claim_str("jti")
        .ok_or_else(|| OAuthError::client("the client assertion has no jti"))?;
    if !shared
        .state
        .lock()
        .await
        .oauth
        .fresh_jti(&format!("assertion:{jti}"), util::now_ms())
    {
        return Err(OAuthError::client("the client assertion was replayed"));
    }
    Ok(())
}

/// Whether `requested` is one of `registered`, loopback IP redirects registered without a port
/// matching any port.
fn redirect_allowed(registered: &[String], requested: &str) -> bool {
    let Ok(requested_url) = url::Url::parse(requested) else {
        return false;
    };
    registered.iter().any(|candidate| {
        if candidate == requested {
            return true;
        }
        let Ok(candidate) = url::Url::parse(candidate) else {
            return false;
        };
        is_loopback_ip(&candidate)
            && is_loopback_ip(&requested_url)
            && candidate.host_str() == requested_url.host_str()
            && candidate.path() == requested_url.path()
            && candidate.query() == requested_url.query()
            && (candidate.port().is_none() || candidate.port() == requested_url.port())
    })
}

/// `POST /oauth/par`.
pub(crate) async fn par(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let pairs = form_pairs(&body);
    let (issuer, nonce) = {
        let mut state = shared.state.lock().await;
        if state.switches.auth_offline {
            drop(state);
            return conn.hang_up().await;
        }
        state.oauth.par_bodies.push(pairs.clone());
        let issuer = Shared::issuer(&state).to_owned();
        let nonce = state.oauth.as_nonce.clone();
        if let Err(error) = check_as_dpop(&mut state, &headers, &endpoint(&issuer, "/oauth/par")) {
            return error.into_response(&nonce);
        }
        if let Some(error) = state.switches.par_error.clone() {
            return OAuthError::new(&error, "refused by a devnet test switch")
                .into_response(&nonce);
        }
        (issuer, nonce)
    };
    match push(&shared, &headers, &pairs, &issuer).await {
        Ok(request_uri) => {
            let mut response = (
                StatusCode::CREATED,
                Json(json!({"request_uri": request_uri, "expires_in": REQUEST_LIFETIME_MS / 1000 - 1})),
            )
                .into_response();
            add_nonce(&mut response, &nonce);
            response
        }
        Err(error) => error.into_response(&nonce),
    }
}

async fn push(
    shared: &Arc<Shared>,
    headers: &HeaderMap,
    pairs: &[(String, String)],
    issuer: &str,
) -> Result<String, OAuthError> {
    let client = authenticate_client(shared, pairs, issuer).await?;
    if first(pairs, "response_type") != Some("code") {
        return Err(OAuthError::new(
            "unsupported_response_type",
            "response_type must be code",
        ));
    }
    if first(pairs, "code_challenge_method") != Some("S256") {
        return Err(OAuthError::new(
            "invalid_request",
            "code_challenge_method must be S256",
        ));
    }
    let challenge = first(pairs, "code_challenge")
        .filter(|c| (43..=128).contains(&c.len()))
        .ok_or_else(|| OAuthError::new("invalid_request", "a code_challenge is required"))?;
    let state_param = first(pairs, "state")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::new("invalid_request", "state is required"))?;
    let redirect_uri = first(pairs, "redirect_uri")
        .ok_or_else(|| OAuthError::new("invalid_request", "redirect_uri is required"))?;
    if !redirect_allowed(&client.redirect_uris, redirect_uri) {
        return Err(OAuthError::new(
            "invalid_redirect_uri",
            format!("redirect_uri {redirect_uri} is not registered for this client"),
        ));
    }
    let scope = first(pairs, "scope").unwrap_or_default();
    let tokens: Vec<&str> = scope.split(' ').filter(|t| !t.is_empty()).collect();
    if let Some(token) = tokens
        .iter()
        .find(|t| !client.scope.iter().any(|s| s == *t))
    {
        return Err(OAuthError::new(
            "invalid_scope",
            format!("Scope \"{token}\" is not declared in the client metadata"),
        ));
    }
    if !tokens.contains(&"atproto") {
        return Err(OAuthError::new(
            "invalid_scope",
            "The atproto scope is required",
        ));
    }
    // The DPoP proof was verified before; its key binds the request.
    let proof = headers
        .get("DPoP")
        .and_then(|v| v.to_str().ok())
        .and_then(jwt::decode)
        .and_then(|jwt| jwt.header.get("jwk").and_then(jwt::thumbprint))
        .ok_or_else(|| OAuthError::new("invalid_dpop_proof", "DPoP proof required"))?;
    let request_uri = format!("urn:ietf:params:oauth:request_uri:req-{}", random_token(16));
    let pending = Pending {
        client_id: client.id,
        redirect_uri: redirect_uri.to_owned(),
        state: state_param.to_owned(),
        scope: tokens.join(" "),
        challenge: challenge.to_owned(),
        jkt: proof,
        login_hint: first(pairs, "login_hint").map(str::to_owned),
        refresh: client.refresh,
        public: client.public,
        expires_ms: util::now_ms() + REQUEST_LIFETIME_MS,
    };
    let mut state = shared.state.lock().await;
    let now = util::now_ms();
    state.oauth.requests.retain(|_, p| p.expires_ms > now);
    state.oauth.requests.insert(request_uri.clone(), pending);
    Ok(request_uri)
}

/// `GET /oauth/authorize?client_id&request_uri[&as][&deny]`.
pub(crate) async fn authorize(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let mut state = shared.state.lock().await;
    if state.switches.auth_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let request_uri = query.get("request_uri").cloned().unwrap_or_default();
    let now = util::now_ms();
    let pending = match state.oauth.requests.get(&request_uri) {
        Some(p) if p.expires_ms > now && Some(&p.client_id) == query.get("client_id") => p.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Unknown or expired authorization request",
            )
                .into_response();
        }
    };
    let who = query
        .get("as")
        .or(pending.login_hint.as_ref())
        .and_then(|who| state.person(who.trim().trim_start_matches("at://")));
    let issuer = Shared::issuer(&state).to_owned();
    let deny = query.get("deny").is_some_and(|v| v == "1" || v == "true");
    let person = match (deny, who) {
        (true, _) => None,
        (false, Some(index)) => Some(index),
        (false, None) => return Html(picker(&state, &query, &pending)).into_response(),
    };
    state.oauth.requests.remove(&request_uri);
    let Ok(mut target) = url::Url::parse(&pending.redirect_uri) else {
        return (StatusCode::BAD_REQUEST, "invalid redirect_uri").into_response();
    };
    {
        let mut params = target.query_pairs_mut();
        if let Some(index) = person {
            let code = random_token(32);
            params.append_pair("code", &code);
            let did = state.people[index].did.clone();
            state.oauth.codes.retain(|_, c| c.expires_ms > now);
            state.oauth.codes.insert(
                code,
                Code {
                    pending: pending.clone(),
                    did,
                    expires_ms: now + CODE_LIFETIME_MS,
                },
            );
        } else {
            params.append_pair("error", "access_denied");
            params.append_pair("error_description", "Access denied");
        }
        params.append_pair("state", &pending.state);
        if !state.switches.callback_without_iss {
            params.append_pair("iss", &issuer);
        }
    }
    Redirect::to(target.as_str()).into_response()
}

fn picker(state: &DevState, query: &HashMap<String, String>, pending: &Pending) -> String {
    let base = format!(
        "/oauth/authorize?client_id={}&request_uri={}",
        url::form_urlencoded::byte_serialize(
            query.get("client_id").map_or("", String::as_str).as_bytes()
        )
        .collect::<String>(),
        url::form_urlencoded::byte_serialize(
            query
                .get("request_uri")
                .map_or("", String::as_str)
                .as_bytes()
        )
        .collect::<String>(),
    );
    let people: String = state
        .people
        .iter()
        .map(|p| {
            format!(
                "<li><a href=\"{base}&amp;as={}\">Sign in as @{}</a> <small>{}</small></li>",
                p.did, p.handle, p.did
            )
        })
        .collect();
    format!(
        "<!doctype html><meta charset=utf-8><title>tartan-devnet</title>\
         <h1>Authorize {}</h1><p>Scope: <code>{}</code></p><ul>{people}</ul>\
         <p><a href=\"{base}&amp;deny=1\">Deny</a></p>",
        html_escape(&pending.client_id),
        html_escape(&pending.scope)
    )
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `POST /oauth/token`.
pub(crate) async fn token(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let pairs = form_pairs(&body);
    let (issuer, nonce, proof) = {
        let mut state = shared.state.lock().await;
        if state.switches.auth_offline {
            drop(state);
            return conn.hang_up().await;
        }
        let issuer = Shared::issuer(&state).to_owned();
        let nonce = state.oauth.as_nonce.clone();
        let proof = match check_as_dpop(&mut state, &headers, &endpoint(&issuer, "/oauth/token")) {
            Ok(proof) => proof,
            Err(error) => return error.into_response(&nonce),
        };
        if let Some(error) = state.switches.token_error.clone() {
            return OAuthError::new(&error, "refused by a devnet test switch")
                .into_response(&nonce);
        }
        (issuer, nonce, proof)
    };
    let client = match authenticate_client(&shared, &pairs, &issuer).await {
        Ok(client) => client,
        Err(error) => return error.into_response(&nonce),
    };
    let result = match first(&pairs, "grant_type") {
        Some("authorization_code") => exchange(&shared, &pairs, &client, &proof).await,
        Some("refresh_token") => refresh(&shared, &pairs, &client, &proof).await,
        _ => Err(OAuthError::new(
            "unsupported_grant_type",
            "unsupported grant_type",
        )),
    };
    match result {
        Ok(body) => {
            let mut response = Json(body).into_response();
            add_nonce(&mut response, &nonce);
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => error.into_response(&nonce),
    }
}

async fn exchange(
    shared: &Arc<Shared>,
    pairs: &[(String, String)],
    client: &Client,
    proof: &DpopProof,
) -> Result<Value, OAuthError> {
    let mut state = shared.state.lock().await;
    let now = util::now_ms();
    let code = first(pairs, "code").unwrap_or_default();
    let grant = state
        .oauth
        .codes
        .remove(code)
        .filter(|c| c.expires_ms > now)
        .ok_or_else(|| OAuthError::new("invalid_grant", "Invalid or expired code"))?;
    let pending = &grant.pending;
    if pending.client_id != client.id {
        return Err(OAuthError::new(
            "invalid_grant",
            "The code was issued to another client",
        ));
    }
    if first(pairs, "redirect_uri") != Some(pending.redirect_uri.as_str()) {
        return Err(OAuthError::new(
            "invalid_grant",
            "redirect_uri does not match the request",
        ));
    }
    let verifier = first(pairs, "code_verifier").unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != pending.challenge {
        return Err(OAuthError::new("invalid_grant", "Invalid code_verifier"));
    }
    if proof.jkt != pending.jkt {
        return Err(OAuthError::new(
            "invalid_dpop_proof",
            "DPoP key does not match the request's",
        ));
    }
    let scope = state
        .switches
        .narrow_scope
        .clone()
        .unwrap_or_else(|| pending.scope.clone());
    let id = state.oauth.next_grant;
    state.oauth.next_grant += 1;
    state.oauth.grants.insert(
        id,
        Grant {
            did: grant.did.clone(),
            client_id: client.id.clone(),
            scope: scope.clone(),
            jkt: proof.jkt.clone(),
            public: pending.public,
            created_ms: now,
            revoked: false,
        },
    );
    let lifetime = state.switches.access_token_secs * 1000;
    let (access, expires_in) = state.oauth.issue(id, &proof.jkt, lifetime, now);
    let mut body = json!({
        "access_token": access,
        "token_type": "DPoP",
        "expires_in": expires_in,
        "scope": scope,
        "sub": grant.did,
    });
    if pending.refresh {
        let token = random_token(32);
        state.oauth.refresh.insert(token.clone(), (id, false));
        body["refresh_token"] = json!(token);
    }
    Ok(body)
}

async fn refresh(
    shared: &Arc<Shared>,
    pairs: &[(String, String)],
    client: &Client,
    proof: &DpopProof,
) -> Result<Value, OAuthError> {
    let mut state = shared.state.lock().await;
    let now = util::now_ms();
    let token = first(pairs, "refresh_token").unwrap_or_default();
    let (id, used) = *state
        .oauth
        .refresh
        .get(token)
        .ok_or_else(|| OAuthError::new("invalid_grant", "Invalid refresh token"))?;
    if used {
        if let Some(grant) = state.oauth.grants.get_mut(&id) {
            grant.revoked = true;
        }
        return Err(OAuthError::new("invalid_grant", "Refresh token replayed"));
    }
    let session_lifetime = state.switches.public_session_secs * 1000;
    let grant = state
        .oauth
        .grants
        .get(&id)
        .cloned()
        .ok_or_else(|| OAuthError::new("invalid_grant", "Invalid refresh token"))?;
    if grant.revoked {
        return Err(OAuthError::new("invalid_grant", "The session was revoked"));
    }
    if grant.client_id != client.id {
        return Err(OAuthError::new(
            "invalid_grant",
            "The refresh token belongs to another client",
        ));
    }
    if grant.jkt != proof.jkt {
        return Err(OAuthError::new(
            "invalid_dpop_proof",
            "DPoP key does not match the session's",
        ));
    }
    if grant.public && now > grant.created_ms + session_lifetime {
        return Err(OAuthError::new("invalid_grant", "The session has expired"));
    }
    state.oauth.refresh.insert(token.to_owned(), (id, true));
    let mut lifetime = state.switches.access_token_secs * 1000;
    if grant.public {
        lifetime = lifetime.min((grant.created_ms + session_lifetime).saturating_sub(now));
    }
    let (access, expires_in) = state.oauth.issue(id, &grant.jkt, lifetime, now);
    let next = random_token(32);
    state.oauth.refresh.insert(next.clone(), (id, false));
    Ok(json!({
        "access_token": access,
        "token_type": "DPoP",
        "expires_in": expires_in,
        "refresh_token": next,
        "scope": grant.scope,
        "sub": grant.did,
    }))
}

/// `POST /oauth/revoke`: revokes the session behind an access or refresh token.
pub(crate) async fn revoke(
    State(shared): State<Arc<Shared>>,
    Extension(conn): Extension<Conn>,
    body: Bytes,
) -> Response {
    let pairs = form_pairs(&body);
    let mut state = shared.state.lock().await;
    if state.switches.auth_offline {
        drop(state);
        return conn.hang_up().await;
    }
    let nonce = state.oauth.as_nonce.clone();
    let token = first(&pairs, "token").unwrap_or_default();
    let grant = state
        .oauth
        .access
        .get(token)
        .map(|access| access.grant)
        .or_else(|| state.oauth.refresh.get(token).map(|(id, _)| *id));
    if let Some(grant) = grant.and_then(|id| state.oauth.grants.get_mut(&id)) {
        grant.revoked = true;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    add_nonce(&mut response, &nonce);
    response
}

/// Revokes `did`'s sessions (with `client_id` only, when given).
pub(crate) fn revoke_sessions(state: &mut DevState, did: &str, client_id: Option<&str>) -> usize {
    let mut count = 0;
    for grant in state.oauth.grants.values_mut() {
        if grant.did == did && !grant.revoked && client_id.is_none_or(|id| id == grant.client_id) {
            grant.revoked = true;
            count += 1;
        }
    }
    count
}

/// Whether `scope` permits `action` on `collection`: `transition:generic`, or a granular
/// `repo:<collection>[?action=…]` token (any action when none is named; `*` for any
/// collection).
pub(crate) fn permits(scope: &str, collection: &str, action: &str) -> bool {
    scope.split(' ').any(|token| {
        if token == "transition:generic" {
            return true;
        }
        let Some(rest) = token.strip_prefix("repo:") else {
            return false;
        };
        let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
        let pairs = form_pairs(query.as_bytes());
        let actions = all(&pairs, "action");
        (target == "*" || target == collection) && (actions.is_empty() || actions.contains(&action))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granular_scopes() {
        let create = "atproto repo:academy.rawkode.tartan.signature?action=create";
        assert!(permits(
            create,
            "academy.rawkode.tartan.signature",
            "create"
        ));
        assert!(!permits(
            create,
            "academy.rawkode.tartan.signature",
            "delete"
        ));
        assert!(!permits(create, "app.bsky.feed.post", "create"));
        assert!(permits(
            "atproto transition:generic",
            "app.bsky.feed.post",
            "delete"
        ));
        assert!(permits(
            "atproto repo:app.bsky.feed.post",
            "app.bsky.feed.post",
            "update"
        ));
        assert!(permits("atproto repo:*?action=delete", "x.y.z", "delete"));
        assert!(!permits("atproto", "x.y.z", "create"));
    }

    #[test]
    fn loopback_redirects_ignore_an_unregistered_port() {
        let registered = vec![
            "http://127.0.0.1/callback".to_owned(),
            "http://[::1]/callback".to_owned(),
            "https://forge.example/-/auth/callback".to_owned(),
        ];
        assert!(redirect_allowed(
            &registered,
            "http://127.0.0.1:49152/callback"
        ));
        assert!(redirect_allowed(&registered, "http://[::1]:8080/callback"));
        assert!(redirect_allowed(
            &registered,
            "https://forge.example/-/auth/callback"
        ));
        assert!(!redirect_allowed(&registered, "http://127.0.0.1:1/other"));
        assert!(!redirect_allowed(
            &registered,
            "http://localhost:1/callback"
        ));
        assert!(!redirect_allowed(
            &registered,
            "http://evil.example/callback"
        ));
        assert!(!redirect_allowed(
            &registered,
            "https://forge.example:444/-/auth/callback"
        ));
        let with_port = vec!["http://127.0.0.1:4321/-/auth/callback".to_owned()];
        assert!(redirect_allowed(
            &with_port,
            "http://127.0.0.1:4321/-/auth/callback"
        ));
        assert!(!redirect_allowed(
            &with_port,
            "http://127.0.0.1:4322/-/auth/callback"
        ));
    }

    #[test]
    fn metadata_refuses_non_loopback_http_redirects() {
        let metadata = |redirect: &str| {
            json!({
                "client_id": "https://forge.example/m.json",
                "application_type": "native",
                "redirect_uris": [redirect],
                "response_types": ["code"],
                "grant_types": ["authorization_code"],
                "scope": "atproto",
                "token_endpoint_auth_method": "none",
                "dpop_bound_access_tokens": true,
            })
        };
        let id = "https://forge.example/m.json";
        assert!(validate_metadata(id, &metadata("http://127.0.0.1/callback")).is_ok());
        assert!(validate_metadata(id, &metadata("http://evil.example/callback")).is_err());
        assert!(validate_metadata(id, &metadata("http://localhost/callback")).is_err());
        assert!(
            validate_metadata(
                "https://other.example/m.json",
                &metadata("http://127.0.0.1/c")
            )
            .is_err()
        );
    }

    #[test]
    fn localhost_client_metadata_comes_from_the_query() {
        let id = "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%3A4321%2F-%2Fauth%2Fcallback&scope=atproto";
        let url = url::Url::parse(id).expect("url");
        let metadata = localhost_metadata(id, &url).expect("metadata");
        let client = validate_metadata(id, &metadata).expect("valid");
        assert!(client.public);
        assert_eq!(
            client.redirect_uris,
            vec!["http://127.0.0.1:4321/-/auth/callback"]
        );
        assert_eq!(client.scope, vec!["atproto"]);
        let with_port = url::Url::parse("http://localhost:8080/").expect("url");
        assert!(localhost_metadata("http://localhost:8080/", &with_port).is_err());
    }
}
