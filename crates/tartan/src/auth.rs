//! Web sign-in with ATProto OAuth (design D1, spec sign-in): the client documents, starting a
//! sign-in, the callback, signing out and reading a failure's details.
//!
//! The forge is a confidential backend-for-frontend client that asks only for `atproto`: the
//! sign-in proves the DID, the tokens are revoked and dropped at once, and the browser gets the
//! forge's own session cookie. Development forges can instead be ATProto's `http://localhost`
//! public client ([`WebClient::Localhost`]).
//!
//! Every failure answers 303 to `/#/-/sign-in?problem=<code>` with only fixed-list values in
//! the URL; hosts and server messages go to an outcome record only the browser that ran the
//! flow can read once.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jacquard_common::BosStr;
use jacquard_common::deps::fluent_uri::Uri;
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_common::http_client::HttpClient;
use jacquard_common::types::did::Did as JDid;
use jacquard_common::types::string::Handle as JHandle;
use jacquard_identity::resolver::{
    DidDocResponse, IdentityError, IdentityResolver, ResolverOptions,
};
use jacquard_oauth::request::{
    OAuthMetadata, RequestError, RequestErrorKind, exchange_code, par, revoke,
};
use jacquard_oauth::resolver::{OAuthResolver, ResolverError};
use jacquard_oauth::session::AuthRequestData;
use jacquard_oauth::types::{OAuthAuthorizationServerMetadata, OAuthClientMetadata};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tartan_atproto::did::Did;
use tartan_atproto::handle::Handle;
use tartan_atproto::resolver::{ResolveError, VerifyingResolver};
use tartan_forge::identity_handle;
use tartan_identity_api::{
    BeginAuth, CompleteAuth, CreateSession, Credential, CredentialKind, EndSession, PendingSignIn,
    PutOutcome, RefreshIdentity, SessionKind, SignInCapacity, SignInRefusal, SignedIn, TakeOutcome,
    status_is_inactive,
};

use crate::access::{Access, cookie_value};
use crate::client_ip::client_address;
use crate::gateway::GatewayState;
use crate::settings::WebClient;

/// The scope the web client declares and requests (design D1, *Scope*).
const SCOPE: &str = "atproto";
/// How long the pre-auth and outcome cookies last, in seconds.
const FLOW_COOKIE_SECS: u64 = 600;
/// How long the session cookie lasts, in seconds: the session's absolute lifetime.
const SESSION_COOKIE_SECS: u64 = 30 * 24 * 3600;
/// The longest a sign-in step waits on another server.
const STEP_TIMEOUT: Duration = Duration::from_secs(25);
/// Account statuses the sign-in page has words for (design D10).
const KNOWN_STATUSES: [&str; 6] = [
    "deactivated",
    "takendown",
    "suspended",
    "deleted",
    "throttled",
    "desynchronized",
];

/// The `/-/auth` routes.
pub(crate) fn routes() -> axum::Router<GatewayState> {
    axum::Router::new()
        .route("/-/auth/client-metadata.json", get(client_metadata))
        .route("/-/auth/jwks.json", get(jwks))
        .route("/-/auth/login", post(login))
        .route("/-/auth/callback", get(callback))
        .route("/-/auth/logout", post(logout))
        .route("/-/auth/outcome", post(outcome))
}

// Client documents --------------------------------------------------------------------------

/// The confidential client's metadata document (design D1, *Client metadata*).
pub fn confidential_metadata(access: &Access) -> Value {
    let public = &access.config.public_url;
    json!({
        "client_id": public.join("/-/auth/client-metadata.json"),
        "client_name": format!("Tartan ({})", public.host()),
        "client_uri": public.origin(),
        "application_type": "web",
        "redirect_uris": [public.join("/-/auth/callback")],
        "response_types": ["code"],
        "grant_types": ["authorization_code"],
        "scope": SCOPE,
        "token_endpoint_auth_method": "private_key_jwt",
        "token_endpoint_auth_signing_alg": "ES256",
        "jwks_uri": public.join("/-/auth/jwks.json"),
        "dpop_bound_access_tokens": true,
    })
}

/// The metadata of ATProto's `http://localhost` development client: the client id carries the
/// redirect URI and scope, and the authorization server derives the rest itself.
pub fn localhost_metadata(access: &Access) -> Value {
    let redirect = access.config.public_url.join("/-/auth/callback");
    json!({
        "client_id": format!(
            "http://localhost?redirect_uri={}&scope={}",
            percent_encode(&redirect),
            percent_encode(SCOPE)
        ),
        "application_type": "native",
        "redirect_uris": [redirect],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": SCOPE,
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

/// The metadata of a hosted native development client ([`WebClient::Native`]).
pub fn native_metadata(access: &Access, client_id: &str) -> Value {
    let public = &access.config.public_url;
    json!({
        "client_id": client_id,
        "client_name": format!("Tartan development ({})", public.host()),
        "application_type": "native",
        "redirect_uris": [public.join("/-/auth/callback")],
        "response_types": ["code"],
        "grant_types": ["authorization_code"],
        "scope": SCOPE,
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

fn client_document(access: &Access) -> (Value, Option<jacquard_oauth::keyset::Keyset>) {
    match &access.config.client {
        WebClient::Confidential(keyset) => (confidential_metadata(access), Some(keyset.clone())),
        WebClient::Localhost => (localhost_metadata(access), None),
        WebClient::Native { client_id } => (native_metadata(access, client_id), None),
    }
}

async fn client_metadata(State(state): State<GatewayState>) -> Response {
    match &state.access.config.client {
        WebClient::Confidential(_) => Json(confidential_metadata(&state.access)).into_response(),
        // The document to host at the client id.
        WebClient::Native { client_id } => {
            Json(native_metadata(&state.access, client_id)).into_response()
        }
        WebClient::Localhost => (
            StatusCode::NOT_FOUND,
            "this forge signs in as the http://localhost development client\n",
        )
            .into_response(),
    }
}

async fn jwks(State(state): State<GatewayState>) -> Response {
    match &state.access.config.client {
        WebClient::Confidential(keyset) => Json(keyset.public_jwks()).into_response(),
        WebClient::Localhost | WebClient::Native { .. } => {
            Json(json!({"keys": []})).into_response()
        }
    }
}

// Problems ----------------------------------------------------------------------------------

/// A sign-in failure: a problem code from design D1's fixed list and what goes with it.
struct Failure {
    code: &'static str,
    status: Option<String>,
    details: BTreeMap<String, String>,
    retry_after: Option<u64>,
}

impl Failure {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            status: None,
            details: BTreeMap::new(),
            retry_after: None,
        }
    }

    fn host(mut self, url_or_host: &str) -> Self {
        self.details.insert("host".to_owned(), host_of(url_or_host));
        self
    }

    fn detail(mut self, name: &str, value: Option<&str>) -> Self {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            self.details.insert(name.to_owned(), value.to_owned());
        }
        self
    }
}

/// The host of `url`, or `url` itself when it is not a URL.
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            })
        })
        .unwrap_or_else(|| url.to_owned())
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The `return_to` a sign-in returns to (design D15): a path-absolute reference on
/// `TARTAN_PUBLIC_URL`, or `/`.
pub fn valid_return_to(text: &str) -> String {
    let acceptable = text.starts_with('/')
        && !text.starts_with("//")
        && !text.starts_with("/\\")
        && !text.chars().any(char::is_control)
        && reqwest::Url::parse(&format!("http://forge.invalid{text}"))
            .is_ok_and(|url| url.host_str() == Some("forge.invalid"));
    if acceptable {
        text.to_owned()
    } else {
        "/".to_owned()
    }
}

fn random_secret() -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

fn sha256_hex(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// The 303 to the sign-in page for `failure`, keeping `return_to`, with the outcome cookie set
/// when the failure has details and the pre-auth cookie cleared when `clear_oauth`.
async fn problem(
    state: &GatewayState,
    client_ip: &str,
    failure: Failure,
    return_to: &str,
    clear_oauth: bool,
) -> Response {
    let access = &state.access;
    let mut query = format!("problem={}", failure.code);
    if let Some(status) = &failure.status {
        let status = if KNOWN_STATUSES.contains(&status.as_str()) {
            status.as_str()
        } else {
            "inactive"
        };
        query.push_str(&format!("&status={status}"));
    }
    let mut headers = HeaderMap::new();
    if !failure.details.is_empty()
        && let Some(secret) = random_secret()
    {
        let stored = async {
            identity_handle(&state.client)?
                .send(PutOutcome {
                    code: failure.code.to_owned(),
                    details: failure.details.clone(),
                    browser_sha256: sha256_hex(&secret),
                    client_ip: client_ip.to_owned(),
                })
                .await
        }
        .await;
        match stored {
            Ok(id) => {
                query.push_str(&format!("&outcome={}", percent_encode(&id)));
                headers.append(
                    header::SET_COOKIE,
                    access.cookie(access.outcome_cookie(), &secret, Some(FLOW_COOKIE_SECS)),
                );
            }
            Err(error) => tracing::warn!(%error, "could not keep a sign-in failure's details"),
        }
    }
    query.push_str(&format!("&return_to={}", percent_encode(return_to)));
    if clear_oauth {
        headers.append(
            header::SET_COOKIE,
            access.clear_cookie(access.oauth_cookie()),
        );
    }
    if let Some(seconds) = failure.retry_after {
        headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    }
    see_other(
        &access
            .config
            .public_url
            .join(&format!("/#/-/sign-in?{query}")),
        headers,
    )
}

fn see_other(location: &str, mut headers: HeaderMap) -> Response {
    if let Ok(location) = HeaderValue::from_str(location) {
        headers.insert(header::LOCATION, location);
    }
    (StatusCode::SEE_OTHER, headers).into_response()
}

fn refusal_failure(refusal: SignInRefusal) -> Failure {
    let mut failure = match refusal {
        SignInRefusal::RateLimited => Failure::new("rate_limited"),
        SignInRefusal::Busy => Failure::new("busy"),
    };
    failure.retry_after = Some(60);
    failure
}

/// An OAuth error a server answered (`error`, `error_description`), from a jacquard error.
fn oauth_error(error: &RequestError) -> Option<(String, Option<String>)> {
    match error.kind() {
        RequestErrorKind::HttpStatusWithBody { status, body } if status.is_client_error() => {
            let code = body.get("error")?.as_str()?.to_owned();
            let description = body
                .get("error_description")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some((code, description))
        }
        _ => None,
    }
}

/// Runs `future` in its own task with a deadline, so a panic or a hang in client code becomes
/// an error rather than a dropped connection.
async fn guarded<T: Send + 'static>(
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> Option<T> {
    match tokio::time::timeout(STEP_TIMEOUT, tokio::spawn(future)).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "a sign-in step panicked");
            None
        }
        Err(_) => None,
    }
}

// Starting a sign-in ------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginForm {
    handle: String,
    #[serde(default)]
    return_to: String,
}

/// What the person typed, as a DID or a handle (design D1, *Flow*).
enum Typed {
    Did(Did),
    Handle(Handle),
}

fn parse_typed(text: &str, mode: tartan_atproto::Mode) -> Option<Typed> {
    let text = text.trim();
    let text = text
        .strip_prefix("at://")
        .or_else(|| text.strip_prefix('@'))
        .unwrap_or(text);
    if text.starts_with("did:") {
        Did::parse(text, mode).ok().map(Typed::Did)
    } else {
        Handle::parse(&text.to_ascii_lowercase(), mode)
            .ok()
            .map(Typed::Handle)
    }
}

/// Checks the authorization server metadata the ATProto profile requires (design D1, login
/// step 2 and 3): the `iss` response parameter, PAR, and `https` endpoints outside development.
fn check_server(metadata: &OAuthAuthorizationServerMetadata, development: bool) -> bool {
    let https = |url: &str| development || url.starts_with("https://");
    metadata.authorization_response_iss_parameter_supported == Some(true)
        && metadata
            .pushed_authorization_request_endpoint
            .as_deref()
            .is_some_and(https)
        && https(&metadata.authorization_endpoint)
        && https(&metadata.token_endpoint)
}

async fn login(State(state): State<GatewayState>, request: Request) -> Response {
    let access = state.access.clone();
    if let Some(response) = access.check_origin(request.headers()) {
        return response;
    }
    let client_ip = client_address(&request, &access.config.trusted_proxies);
    let form = match axum::Form::<LoginForm>::from_request(request, &state).await {
        Ok(axum::Form(form)) => form,
        Err(_) => LoginForm {
            handle: String::new(),
            return_to: String::new(),
        },
    };
    let return_to = valid_return_to(&form.return_to);
    match start(&state, &client_ip, &form.handle, &return_to).await {
        Ok(response) => response,
        Err(failure) => problem(&state, &client_ip, failure, &return_to, false).await,
    }
}

use axum::extract::FromRequest as _;

async fn start(
    state: &GatewayState,
    client_ip: &str,
    typed: &str,
    return_to: &str,
) -> Result<Response, Failure> {
    let access = &state.access;
    if let Err(seconds) = access.sign_ins.take(client_ip) {
        let mut failure = Failure::new("rate_limited");
        failure.retry_after = Some(seconds);
        return Err(failure);
    }
    let parsed = parse_typed(typed, access.config.resolver.mode())
        .ok_or_else(|| Failure::new("handle_not_found"))?;
    let identity = identity_handle(&state.client).map_err(|_| Failure::new("busy"))?;
    identity
        .send(SignInCapacity {
            client_ip: client_ip.to_owned(),
        })
        .await
        .map_err(|_| Failure::new("busy"))?
        .map_err(refusal_failure)?;
    let pushed = push(access, parsed).await?;

    // 4. Only now is the pending request stored, bound to this browser.
    let secret = random_secret().ok_or_else(|| Failure::new("busy"))?;
    let auth_request = serde_json::to_string(&pushed.request).map_err(|_| Failure::new("busy"))?;
    let stored = identity
        .send(BeginAuth {
            pending: PendingSignIn {
                state: pushed.request.state.to_string(),
                did: pushed.did,
                handle: pushed.handle,
                issuer: pushed.issuer,
                pds: pushed.pds,
                auth_request,
                return_to: return_to.to_owned(),
                client_ip: client_ip.to_owned(),
                browser_sha256: sha256_hex(&secret),
            },
        })
        .await
        .map_err(|_| Failure::new("busy"))?;
    stored.map_err(refusal_failure)?;

    // 5. To the authorization server, with only the client id and request URI.
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        access.cookie(access.oauth_cookie(), &secret, Some(FLOW_COOKIE_SECS)),
    );
    Ok(see_other(&pushed.location, headers))
}

/// A pushed authorization request, ready for the browser.
struct Pushed {
    request: AuthRequestData,
    did: String,
    handle: Option<String>,
    issuer: String,
    pds: String,
    /// The authorization URL: the endpoint with only `client_id` and `request_uri`.
    location: String,
}

/// Login steps 1 to 3 of design D1: resolves the person, their PDS and its authorization
/// server, and pushes the authorization request. Stores nothing.
async fn push(access: &Access, typed: Typed) -> Result<Pushed, Failure> {
    let resolver = access.config.resolver.clone();
    let mode = resolver.mode();

    // 1. The DID and its PDS, verified (design D9).
    let (did, handle) = match &typed {
        Typed::Did(did) => (did.clone(), None),
        Typed::Handle(handle) => {
            let resolved = {
                let resolver = resolver.clone();
                let handle = handle.clone();
                guarded(async move { resolver.resolve_verified_handle(&handle).await }).await
            };
            match resolved {
                Some(Ok(did)) => (did, Some(handle.as_str().to_owned())),
                Some(Err(ResolveError::Unavailable(_))) | None => {
                    return Err(Failure::new("server_unreachable").host(handle.as_str()));
                }
                Some(Err(_)) => return Err(Failure::new("handle_not_found")),
            }
        }
    };
    let resolved = {
        let resolver = resolver.clone();
        let did = did.clone();
        guarded(async move { resolver.resolve_did(&did).await }).await
    };
    let identity_facts = match resolved {
        Some(Ok(resolved)) => resolved.identity,
        Some(Err(ResolveError::Unavailable(reason))) => {
            return Err(Failure::new("server_unreachable").detail("message", Some(&reason)));
        }
        None => return Err(Failure::new("server_unreachable")),
        Some(Err(_)) => return Err(Failure::new("handle_not_found")),
    };
    let Some(facts) = identity_facts else {
        let mut failure = Failure::new("account_inactive");
        failure.status = Some("deleted".to_owned());
        return Err(failure);
    };
    let Some(pds) = facts.pds.clone() else {
        return Err(Failure::new("handle_not_found"));
    };

    // 2. The PDS's authorization server, with its issuer checked.
    let server = {
        let resolver = resolver.clone();
        let pds = pds.clone();
        guarded(async move { resolver.get_resource_server_metadata(&pds).await }).await
    };
    let server_metadata = match server {
        Some(Ok(metadata)) => metadata,
        _ => return Err(Failure::new("server_unreachable").host(&pds)),
    };
    let issuer = server_metadata.issuer.to_string();
    if !check_server(&server_metadata, mode.is_development()) {
        return Err(Failure::new("server_unreachable").host(&issuer));
    }

    // 3. PAR with scope `atproto`.
    let (client, keyset) = client_document(access);
    let client_metadata: OAuthClientMetadata = serde_json::from_value(client)
        .map_err(|_| Failure::new("client_rejected").host(&issuer))?;
    let authorization_endpoint = server_metadata.authorization_endpoint.to_string();
    let client_id = client_metadata.client_id.to_string();
    let metadata = OAuthMetadata {
        server_metadata,
        client_metadata,
        keyset,
    };
    let hint = SmolStr::from(handle.clone().unwrap_or_else(|| did.as_str().to_owned()));
    let pushed = {
        let resolver = resolver.clone();
        let mut metadata = metadata;
        guarded(async move { par(&resolver, Some(hint), None, &mut metadata, None).await }).await
    };
    let request = match pushed {
        Some(Ok(request)) => request,
        Some(Err(error)) => {
            return Err(match oauth_error(&error) {
                Some((code, description)) => Failure::new("client_rejected")
                    .host(&issuer)
                    .detail("error", Some(&code))
                    .detail("error_description", description.as_deref()),
                None => Failure::new("server_unreachable").host(&issuer),
            });
        }
        None => return Err(Failure::new("server_unreachable").host(&issuer)),
    };

    let separator = if authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let location = format!(
        "{authorization_endpoint}{separator}client_id={}&request_uri={}",
        percent_encode(&client_id),
        percent_encode(&request.request_uri)
    );
    Ok(Pushed {
        request,
        did: did.as_str().to_owned(),
        handle,
        issuer,
        pds,
        location,
    })
}

// The callback ------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The resolver jacquard's code exchange runs with: everything goes to the verifying resolver,
/// except that `verify_issuer` answers from the pending request's own evidence, resolved when
/// the sign-in started (design D1, callback step 6). It accepts only the DID and issuer the
/// sign-in started with, so a second resolution cannot fail a sign-in that otherwise worked.
struct CallbackResolver {
    inner: VerifyingResolver,
    did: String,
    issuer: String,
    pds: String,
}

impl HttpClient for CallbackResolver {
    type Error = <VerifyingResolver as HttpClient>::Error;

    async fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, Self::Error> {
        self.inner.send_http(request).await
    }
}

impl IdentityResolver for CallbackResolver {
    fn options(&self) -> &ResolverOptions {
        self.inner.options()
    }

    async fn resolve_handle<S: BosStr + Sync>(
        &self,
        handle: &JHandle<S>,
    ) -> Result<JDid, IdentityError> {
        IdentityResolver::resolve_handle(&self.inner, handle).await
    }

    async fn resolve_did_doc<S: BosStr + Sync>(
        &self,
        did: &JDid<S>,
    ) -> Result<DidDocResponse, IdentityError> {
        IdentityResolver::resolve_did_doc(&self.inner, did).await
    }
}

impl OAuthResolver for CallbackResolver {
    async fn verify_issuer<S: BosStr + Sync>(
        &self,
        server_metadata: &OAuthAuthorizationServerMetadata,
        sub: &JDid<S>,
    ) -> Result<Uri<String>, ResolverError> {
        if sub.as_str() != self.did || server_metadata.issuer.as_str() != self.issuer {
            return Err(ResolverError::authorization_server_metadata(
                "the token's subject is not the DID the sign-in started with",
            ));
        }
        Uri::parse(self.pds.clone())
            .map_err(|_| ResolverError::did_document("the PDS endpoint is not a URI"))
    }
}

impl jacquard_oauth::dpop::DpopExt for CallbackResolver {}

/// What the code exchange decided.
enum Exchanged {
    Tokens {
        sub: String,
        scope: Option<String>,
        access_token: String,
        refresh_token: Option<String>,
    },
    Failed(Failure),
}

async fn callback(
    State(state): State<GatewayState>,
    Query(query): Query<CallbackQuery>,
    request: Request,
) -> Response {
    let access = state.access.clone();
    let client_ip = client_address(&request, &access.config.trusted_proxies);
    let headers = request.headers().clone();
    // 1. The state is consumed, whatever happens next.
    let pending = match &query.state {
        Some(state_key) => match identity_handle(&state.client) {
            Ok(identity) => identity
                .send(CompleteAuth {
                    state: state_key.clone(),
                })
                .await
                .ok()
                .flatten(),
            Err(_) => None,
        },
        None => None,
    };
    let Some(pending) = pending else {
        return problem(&state, &client_ip, Failure::new("expired"), "/", true).await;
    };
    let return_to = pending.return_to.clone();
    let fail = |failure: Failure| {
        let state = state.clone();
        let client_ip = client_ip.clone();
        let return_to = return_to.clone();
        async move { problem(&state, &client_ip, failure, &return_to, true).await }
    };
    // 2. Only the browser that started it.
    let browser = cookie_value(&headers, access.oauth_cookie()).map(sha256_hex);
    let same_browser = browser
        .is_some_and(|hash| constant_time_eq(hash.as_bytes(), pending.browser_sha256.as_bytes()));
    if !same_browser {
        return fail(Failure::new("browser_mismatch")).await;
    }
    // 3. Cancelled or refused at the authorization server.
    if query.error.is_some() || query.code.is_none() {
        let failure =
            Failure::new("denied").detail("error_description", query.error_description.as_deref());
        return fail(failure).await;
    }
    // 4. The issuer, which must say who it is.
    let resolver = access.config.resolver.clone();
    let server = {
        let resolver = resolver.clone();
        let issuer = pending.issuer.clone();
        guarded(async move { resolver.get_authorization_server_metadata(&issuer).await }).await
    };
    let server_metadata = match server {
        Some(Ok(metadata)) if check_server(&metadata, resolver.mode().is_development()) => metadata,
        _ => return fail(Failure::new("server_unreachable").host(&pending.issuer)).await,
    };
    if query.iss.as_deref() != Some(pending.issuer.as_str()) {
        return fail(Failure::new("wrong_issuer")).await;
    }
    let Ok(mut request_data) = serde_json::from_str::<AuthRequestData>(&pending.auth_request)
    else {
        return fail(Failure::new("expired")).await;
    };
    let (client, keyset) = client_document(&access);
    let Ok(client_metadata) = serde_json::from_value::<OAuthClientMetadata>(client) else {
        return fail(Failure::new("client_rejected").host(&pending.issuer)).await;
    };
    let metadata = OAuthMetadata {
        server_metadata,
        client_metadata,
        keyset,
    };

    // 5 and 6. The code exchange, with the subject checked against the pending DID.
    let exchanger = Arc::new(CallbackResolver {
        inner: resolver.clone(),
        did: pending.did.clone(),
        issuer: pending.issuer.clone(),
        pds: pending.pds.clone(),
    });
    let code = query.code.clone().unwrap_or_default();
    let exchanged = {
        let exchanger = exchanger.clone();
        let metadata = metadata.clone();
        let issuer = pending.issuer.clone();
        let mut dpop = request_data.dpop_data.clone();
        let verifier = request_data.pkce_verifier.to_string();
        let outcome = guarded(async move {
            let result = exchange_code(&*exchanger, &mut dpop, &code, &verifier, &metadata).await;
            (result, dpop)
        })
        .await;
        match outcome {
            Some((Ok(tokens), dpop)) => {
                request_data.dpop_data = dpop;
                Exchanged::Tokens {
                    sub: tokens.sub.as_str().to_owned(),
                    scope: tokens.scope.map(|scope| scope.to_string()),
                    access_token: tokens.access_token.to_string(),
                    refresh_token: tokens.refresh_token.map(|token| token.to_string()),
                }
            }
            Some((Err(error), _)) => Exchanged::Failed(match error.kind() {
                RequestErrorKind::Resolver
                | RequestErrorKind::TokenVerification
                | RequestErrorKind::InvalidDid => Failure::new("wrong_subject"),
                _ => match oauth_error(&error) {
                    Some((code, description)) => Failure::new("client_rejected")
                        .host(&issuer)
                        .detail("error", Some(&code))
                        .detail("error_description", description.as_deref()),
                    None => Failure::new("server_unreachable").host(&issuer),
                },
            }),
            None => Exchanged::Failed(Failure::new("server_unreachable").host(&issuer)),
        }
    };
    let (access_token, refresh_token, decided) = match exchanged {
        Exchanged::Failed(failure) => return fail(failure).await,
        Exchanged::Tokens {
            sub,
            scope,
            access_token,
            refresh_token,
        } => {
            let decided = if sub != pending.did {
                Err(Failure::new("wrong_subject"))
            } else if !scope
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .any(|token| token == SCOPE)
            {
                Err(Failure::new("missing_scope"))
            } else {
                Ok(())
            };
            (access_token, refresh_token, decided)
        }
    };

    // 7. The account's status, refreshed before any session exists.
    let decided = match decided {
        Err(failure) => Err(failure),
        Ok(()) => match identity_handle(&state.client) {
            Ok(identity) => match identity
                .send(RefreshIdentity {
                    did: pending.did.clone(),
                })
                .await
            {
                Ok(refreshed) => {
                    if refreshed
                        .pds
                        .as_ref()
                        .is_some_and(|pds| *pds != pending.pds)
                    {
                        Err(Failure::new("wrong_issuer"))
                    } else if status_is_inactive(&refreshed.status) {
                        let mut failure = Failure::new("account_inactive").host(&pending.pds);
                        failure.status = Some(refreshed.status);
                        Err(failure)
                    } else {
                        Ok(())
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "the sign-in status refresh failed");
                    Err(Failure::new("server_unreachable").host(&pending.pds))
                }
            },
            Err(_) => Err(Failure::new("busy")),
        },
    };

    // 8. The tokens are revoked and dropped, whatever step 7 decided.
    if request_data.authserver_revocation_endpoint.is_some() {
        let exchanger = exchanger.clone();
        let mut dpop = request_data.dpop_data.clone();
        let tokens: Vec<String> = refresh_token.into_iter().chain([access_token]).collect();
        let revoked = guarded(async move {
            for token in tokens {
                if let Err(error) = revoke(&*exchanger, &mut dpop, &token, &metadata).await {
                    tracing::debug!(%error, "token revocation failed");
                }
            }
        })
        .await;
        if revoked.is_none() {
            tracing::debug!("token revocation did not finish");
        }
    }

    if let Err(failure) = decided {
        return fail(failure).await;
    }

    // A fresh session; a session cookie the browser presented is never adopted, and is ended.
    let Ok(identity) = identity_handle(&state.client) else {
        return fail(Failure::new("busy")).await;
    };
    if let Some(old) = access.session_secret(&headers)
        && let Ok(Some(found)) = access
            .lookup(&state.client, old, CredentialKind::BrowserSession)
            .await
        && let Credential::Session { id, .. } = &found.credential
    {
        let _ = identity
            .send(EndSession {
                did: found.did.clone(),
                id: id.clone(),
            })
            .await;
        access.forget(id);
    }
    let device = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let session = match identity
        .send(CreateSession {
            did: pending.did.clone(),
            kind: SessionKind::Browser,
            device,
        })
        .await
    {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "could not create a session");
            return fail(Failure::new("busy")).await;
        }
    };
    if let Err(error) = identity
        .send(SignedIn {
            did: pending.did.clone(),
            handle: pending.handle.clone(),
        })
        .await
    {
        tracing::warn!(%error, "could not record a sign-in");
    }
    let mut response_headers = HeaderMap::new();
    response_headers.append(
        header::SET_COOKIE,
        access.cookie(
            access.session_cookie(),
            &session.secret,
            Some(SESSION_COOKIE_SECS),
        ),
    );
    response_headers.append(
        header::SET_COOKIE,
        access.clear_cookie(access.oauth_cookie()),
    );
    see_other(&access.config.public_url.join(&return_to), response_headers)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// Signing out and failure details -------------------------------------------------------------

async fn logout(State(state): State<GatewayState>, headers: HeaderMap) -> Response {
    let access = state.access.clone();
    if let Some(response) = access.check_origin(&headers) {
        return response;
    }
    if let Some(secret) = access.session_secret(&headers) {
        if let Ok(Some(found)) = access
            .lookup(&state.client, secret, CredentialKind::BrowserSession)
            .await
            && let Credential::Session { id, .. } = &found.credential
        {
            let ended = async {
                identity_handle(&state.client)?
                    .send(EndSession {
                        did: found.did.clone(),
                        id: id.clone(),
                    })
                    .await
            }
            .await;
            if let Err(error) = ended {
                return crate::ApiError::Unavailable(error).into_response();
            }
            access.forget(id);
        }
        access.forget_secret(secret, CredentialKind::BrowserSession);
    }
    let mut response_headers = HeaderMap::new();
    response_headers.append(
        header::SET_COOKIE,
        access.clear_cookie(access.session_cookie()),
    );
    (StatusCode::NO_CONTENT, response_headers).into_response()
}

#[derive(Deserialize)]
struct OutcomeRequest {
    id: String,
    code: String,
}

async fn outcome(State(state): State<GatewayState>, headers: HeaderMap, body: Bytes) -> Response {
    let access = state.access.clone();
    if let Some(response) = access.check_origin(&headers) {
        return response;
    }
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such outcome"})),
        )
            .into_response()
    };
    let Ok(read) = serde_json::from_slice::<OutcomeRequest>(&body) else {
        return not_found();
    };
    let Some(secret) = cookie_value(&headers, access.outcome_cookie()) else {
        return not_found();
    };
    let taken = async {
        identity_handle(&state.client)?
            .send(TakeOutcome {
                id: read.id,
                code: read.code,
                browser_sha256: sha256_hex(secret),
            })
            .await
    }
    .await;
    match taken {
        Ok(Some(details)) => {
            let mut response_headers = HeaderMap::new();
            response_headers.append(
                header::SET_COOKIE,
                access.clear_cookie(access.outcome_cookie()),
            );
            (StatusCode::OK, response_headers, Json(details)).into_response()
        }
        Ok(None) => not_found(),
        Err(error) => crate::ApiError::Unavailable(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_values() {
        assert_eq!(valid_return_to("/#/acme/api/issues"), "/#/acme/api/issues");
        assert_eq!(
            valid_return_to("/#/acme/api/issues?x=1"),
            "/#/acme/api/issues?x=1"
        );
        for bad in [
            "//evil.example/",
            "/\\evil.example",
            "https://evil.example",
            "",
            "x",
            "/a\nb",
        ] {
            assert_eq!(valid_return_to(bad), "/", "{bad:?}");
        }
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(
            percent_encode("/#/acme/api/issues?x=1"),
            "%2F%23%2Facme%2Fapi%2Fissues%3Fx%3D1"
        );
    }

    #[test]
    fn typed_identities() {
        let mode = tartan_atproto::Mode::Production;
        assert!(
            matches!(parse_typed(" @Alice.Bsky.Social ", mode), Some(Typed::Handle(h)) if h.as_str() == "alice.bsky.social")
        );
        assert!(matches!(
            parse_typed("at://alice.bsky.social", mode),
            Some(Typed::Handle(_))
        ));
        assert!(matches!(
            parse_typed("did:web:rawkode.dev", mode),
            Some(Typed::Did(_))
        ));
        assert!(parse_typed("not a handle", mode).is_none());
    }

    /// Pushes an authorization request for the forge owner's real identity to its real
    /// authorization server as the `http://localhost` development client, as a forge at
    /// `http://127.0.0.1:4321` started with `TARTAN_DEV_LOCALHOST_CLIENT=1` does. It stops before
    /// the browser step, which needs a person.
    #[tokio::test]
    #[ignore = "reaches rawkode.dev and its authorization server over the public network"]
    async fn a_real_authorization_server_accepts_the_localhost_client() {
        use tartan_atproto::Mode;
        use tartan_atproto::fetch::{FetchClient, FetchConfig};
        use tartan_atproto::resolver::ResolverConfig;

        use crate::access::AccessConfig;
        use crate::settings::{PublicUrl, PublicUrlRules};

        let fetch = FetchClient::new(FetchConfig::new(Mode::Production)).expect("client");
        let resolver = VerifyingResolver::new(fetch, ResolverConfig::production().expect("dns"));
        let access = Access::new(AccessConfig {
            public_url: PublicUrl::parse("http://127.0.0.1:4321", PublicUrlRules::Loopback)
                .expect("url"),
            client: WebClient::Localhost,
            resolver,
            owners: Vec::new(),
            readers: Default::default(),
            trusted_proxies: Vec::new(),
            clock: Arc::new(tartan_forge::SystemClock),
        });
        let only = std::env::var("TARTAN_REAL_PAR_HANDLE").ok();
        let wanted: Vec<String> = match &only {
            Some(handle) => vec![handle.clone()],
            None => vec!["did:web:rawkode.dev".to_owned(), "rawkode.dev".to_owned()],
        };
        for typed in wanted.iter().map(String::as_str) {
            let parsed = parse_typed(typed, Mode::Production).expect("parses");
            let pushed = match push(&access, parsed).await {
                Ok(pushed) => pushed,
                Err(failure) => panic!("{typed}: {} {:?}", failure.code, failure.details),
            };
            if only.is_none() {
                assert_eq!(pushed.did, "did:web:rawkode.dev");
            }
            assert!(
                pushed.location.starts_with("https://"),
                "{}",
                pushed.location
            );
            assert!(
                pushed
                    .location
                    .contains("client_id=http%3A%2F%2Flocalhost%3F")
            );
            assert!(pushed.location.contains("request_uri="));
            println!(
                "{typed}: issuer {} pds {} -> {}",
                pushed.issuer, pushed.pds, pushed.location
            );
        }
    }

    /// The scope the panicking-scope sign-in test grants is one jacquard's own callback would
    /// `expect` to parse; the forge reads the granted scope itself, so it is `missing_scope`.
    #[test]
    fn the_test_scope_is_one_jacquard_cannot_parse() {
        assert!(
            jacquard_oauth::scopes::Scopes::new(SmolStr::from("atproto-not bogus:%%")).is_err()
        );
    }

    #[test]
    fn hosts() {
        assert_eq!(host_of("https://bsky.social/oauth"), "bsky.social");
        assert_eq!(host_of("http://127.0.0.1:9400"), "127.0.0.1:9400");
        assert_eq!(host_of("alice.example"), "alice.example");
    }
}
