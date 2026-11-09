//! `tt`'s ATProto OAuth client for signing (design D7, *tt's OAuth client*).
//!
//! `tt` is a public native client. Its `client_id` is the configured forge's
//! `/-/auth/tt/client-metadata.json`; with `--client localhost`, ATProto's `http://localhost`
//! development client id carrying both loopback redirect URIs and the declared scope; or, in
//! `dev-identity` builds, with `--client-metadata-url`, a document the developer hosts at that
//! `https` URL (for authorization servers that refuse the localhost client). It sends a
//! PAR with PKCE S256, DPoP from the first request and `login_hint`, receives the code on its own
//! loopback listener, checks `state`, `iss` and `sub`, and keeps the session (tokens and the DPoP
//! key) as one secret in the credential store.
//!
//! jacquard-oauth's `par`, token-endpoint and revocation functions do the protocol work; `tt`
//! builds the client metadata itself and checks the subject and issuer against the identity it
//! resolved before the PAR, which is what jacquard's issuer verification would do.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_oauth::request::{
    OAuthMetadata, OAuthRequest, RequestError, RequestErrorKind, oauth_request, par, revoke,
};
use jacquard_oauth::resolver::OAuthResolver as _;
use jacquard_oauth::scopes::{Scope, Scopes};
use jacquard_oauth::session::DpopClientData;
use jacquard_oauth::types::{
    OAuthAuthorizationServerMetadata, OAuthClientMetadata, OAuthTokenResponse,
    RefreshRequestParameters, TokenGrantType, TokenRequestParameters,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tartan_atproto::did::Did;
use tartan_atproto::handle::Handle;
use tartan_atproto::resolver::{ResolveError, VerifyingResolver};
use tartan_atproto::signature::COLLECTION;

use crate::ctx::Ctx;
use crate::loopback::{self, Loopback};
use crate::paths;
use crate::state::{self, SigningEntry};

/// The scope signing asks for.
pub const CREATE_SCOPE: &str = "atproto repo:academy.rawkode.tartan.signature?action=create";
/// The scope `tt sign revoke` asks for, once.
pub const DELETE_SCOPE: &str = "atproto repo:academy.rawkode.tartan.signature?action=delete";
/// The scope the client metadata declares: both permissions, verbatim, because authorization
/// servers compare requested scope tokens literally with the declared ones.
pub const DECLARED_SCOPE: &str = "atproto repo:academy.rawkode.tartan.signature?action=create \
                                  repo:academy.rawkode.tartan.signature?action=delete";

/// How long a single OAuth step may take.
const STEP: Duration = Duration::from_secs(30);

/// Which OAuth client a session uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientSpec {
    /// The forge-hosted client metadata.
    Forge {
        /// The forge's origin.
        forge: String,
    },
    /// ATProto's `http://localhost` development client.
    Localhost,
    /// A development client whose metadata document (see [`hosted_document`]) the developer
    /// hosts at the `https` URL `client_id`.
    Hosted {
        /// The document's URL.
        client_id: String,
    },
}

/// The client metadata document for a hosted `tt` client at `client_id`: the forge's `tt`
/// document with that client id.
pub fn hosted_document(client_id: &str) -> Value {
    json!({
        "client_id": client_id,
        "client_name": "tt (development)",
        "application_type": "native",
        "redirect_uris": [
            format!("http://127.0.0.1{CALLBACK}"),
            format!("http://[::1]{CALLBACK}"),
        ],
        "response_types": ["code"],
        "grant_types": ["authorization_code", "refresh_token"],
        "scope": DECLARED_SCOPE,
        "token_endpoint_auth_method": "none",
        "dpop_bound_access_tokens": true,
    })
}

impl ClientSpec {
    /// The `client_id`.
    pub fn client_id(&self) -> String {
        match self {
            ClientSpec::Forge { forge } => format!("{forge}/-/auth/tt/client-metadata.json"),
            ClientSpec::Localhost => format!(
                "http://localhost?redirect_uri={}&redirect_uri={}&scope={}",
                percent(&format!("http://127.0.0.1{CALLBACK}")),
                percent(&format!("http://[::1]{CALLBACK}")),
                percent(DECLARED_SCOPE)
            ),
            ClientSpec::Hosted { client_id } => client_id.clone(),
        }
    }

    /// The forge whose metadata it uses.
    pub fn forge(&self) -> Option<&str> {
        match self {
            ClientSpec::Forge { forge } => Some(forge),
            ClientSpec::Localhost | ClientSpec::Hosted { .. } => None,
        }
    }

    /// The metadata `tt` holds for itself: the redirect carries the listening port and the
    /// scope is what this request asks for.
    fn metadata(&self, port: u16, scope: &str) -> Result<OAuthClientMetadata> {
        let value = json!({
            "client_id": self.client_id(),
            "application_type": "native",
            "redirect_uris": [format!("http://127.0.0.1:{port}{CALLBACK}")],
            "response_types": ["code"],
            "grant_types": ["authorization_code", "refresh_token"],
            "scope": scope,
            "token_endpoint_auth_method": "none",
            "dpop_bound_access_tokens": true,
        });
        serde_json::from_value(value).context("building tt's client metadata")
    }
}

/// The path `tt` listens on for the authorization server's redirect.
pub const CALLBACK: &str = "/callback";

fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A signing session, stored as one secret under its DID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningSession {
    /// The account.
    pub did: String,
    /// The client it belongs to.
    pub client: ClientSpec,
    /// The PDS.
    pub pds: String,
    /// The authorization server's issuer.
    pub issuer: String,
    /// When the session was created (Unix milliseconds).
    pub created_at: i64,
    /// The granted scope.
    pub scope: String,
    /// The access token.
    pub access_token: String,
    /// The refresh token.
    pub refresh_token: Option<String>,
    /// When the access token expires (Unix milliseconds), when the server said.
    pub expires_at: Option<i64>,
    /// The DPoP key and the per-server nonces.
    pub dpop: DpopClientData,
}

impl SigningSession {
    /// Reads the stored session of `did`.
    pub fn load(ctx: &Ctx, did: &str) -> Result<Option<Self>> {
        let Some(text) = ctx.store.get(did)? else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .with_context(|| format!("the stored signing session of {did} is unreadable"))
    }

    /// Stores the session and indexes it.
    pub fn save(&self, ctx: &Ctx) -> Result<crate::credentials::Written> {
        let text = serde_json::to_string(self).context("encoding the signing session")?;
        let written = ctx.store.set(&self.did, &text)?;
        let entry = SigningEntry {
            client_id: self.client.client_id(),
            forge: self.client.forge().map(str::to_owned),
            issuer: self.issuer.clone(),
            pds: self.pds.clone(),
            handle: None,
            created_at: self.created_at,
        };
        state::update_index(&ctx.dirs, |index| {
            let handle = index
                .signing
                .get(&self.did)
                .and_then(|old| old.handle.clone());
            index.signing.insert(
                self.did.clone(),
                SigningEntry {
                    handle,
                    ..entry.clone()
                },
            );
        })?;
        Ok(written)
    }

    /// Whether the access token is usable for at least another minute.
    pub fn fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expires| expires > state::now_ms() + 60_000)
    }
}

/// Deletes the stored session of `did` and its index entry.
pub fn forget(ctx: &Ctx, did: &str) -> Result<()> {
    ctx.store.delete(did)?;
    state::update_index(&ctx.dirs, |index| {
        index.signing.remove(did);
    })
}

/// Who to sign in as.
#[derive(Debug, Clone)]
pub enum Who {
    /// A DID (it is also the `login_hint`).
    Did(String),
    /// A handle, verified in both directions first.
    Handle(String),
}

/// A sign-in request.
pub struct LoginRequest<'a> {
    /// The context.
    pub ctx: &'a Ctx,
    /// The account.
    pub who: Who,
    /// The client.
    pub client: ClientSpec,
    /// The scope to ask for.
    pub scope: &'static str,
    /// `--port`.
    pub port: Option<u16>,
    /// `--no-open`.
    pub no_open: bool,
}

/// The identity a login resolved before contacting the authorization server.
struct Account {
    did: Did,
    pds: String,
    server: OAuthAuthorizationServerMetadata,
}

async fn resolve_account(
    ctx: &Ctx,
    resolver: &VerifyingResolver,
    who: &Who,
) -> Result<(Account, String)> {
    let (did, hint) = match who {
        Who::Did(did) => {
            let parsed = Did::parse(did, ctx.mode).map_err(|error| anyhow!("{did}: {error}"))?;
            (parsed, did.clone())
        }
        Who::Handle(handle) => {
            let text = handle.trim().trim_start_matches('@').to_ascii_lowercase();
            let parsed = Handle::parse(&text, ctx.mode)
                .map_err(|_| anyhow!("{handle:?} is not a handle"))?;
            let did = resolver
                .resolve_verified_handle(&parsed)
                .await
                .map_err(|error| anyhow!("resolving @{text}: {error}"))?;
            (did, text)
        }
    };
    let resolved = resolver
        .resolve_did(&did)
        .await
        .map_err(|error| anyhow!("resolving {did}: {error}"))?;
    let identity = resolved
        .identity
        .ok_or_else(|| anyhow!("{did} is deactivated (its DID is tombstoned)"))?;
    let pds = identity
        .pds
        .ok_or_else(|| anyhow!("{did} names no PDS in its DID document"))?;
    let server = guarded({
        let resolver = resolver.clone();
        let pds = pds.clone();
        async move { resolver.get_resource_server_metadata(&pds).await }
    })
    .await
    .and_then(|result| result.map_err(|error| anyhow!("{error}")))
    .map_err(|error| anyhow!("finding the authorization server of {pds}: {error}"))?;
    check_server(ctx, &server)?;
    Ok((Account { did, pds, server }, hint))
}

/// The ATProto OAuth profile's requirements on the authorization server.
fn check_server(ctx: &Ctx, server: &OAuthAuthorizationServerMetadata) -> Result<()> {
    let https = |url: &str| ctx.mode.is_development() || url.starts_with("https://");
    if server.authorization_response_iss_parameter_supported != Some(true) {
        bail!(
            "the authorization server {} does not support the `iss` response parameter that \
             ATProto OAuth requires",
            server.issuer
        );
    }
    let Some(par) = server.pushed_authorization_request_endpoint.as_deref() else {
        bail!(
            "the authorization server {} offers no pushed authorization requests",
            server.issuer
        );
    };
    if !https(par) || !https(&server.authorization_endpoint) || !https(&server.token_endpoint) {
        bail!(
            "the authorization server {} has endpoints that are not https",
            server.issuer
        );
    }
    Ok(())
}

/// Runs `future` in its own task with a deadline, so a panic or a hang in client code becomes
/// an error.
async fn guarded<T: Send + 'static>(
    future: impl std::future::Future<Output = T> + Send + 'static,
) -> Result<T> {
    match tokio::time::timeout(STEP, tokio::spawn(future)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => bail!("the OAuth client failed unexpectedly"),
        Err(_) => bail!("the authorization server did not answer in time"),
    }
}

/// An OAuth error a server answered (`error`, `error_description`).
fn oauth_error(error: &RequestError) -> Option<(String, String)> {
    match error.kind() {
        RequestErrorKind::HttpStatusWithBody { status, body } if status.is_client_error() => {
            let code = body.get("error")?.as_str()?.to_owned();
            let description = body
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Some((code, description))
        }
        _ => None,
    }
}

/// Checks that the forge's client metadata can be loaded, so a forge that is down is named as
/// such before the authorization server is asked.
async fn check_client_metadata(ctx: &Ctx, client: &ClientSpec) -> Result<()> {
    if let ClientSpec::Hosted { client_id } = client {
        return check_hosted(ctx, client_id).await;
    }
    let ClientSpec::Forge { forge } = client else {
        return Ok(());
    };
    if !ctx.forge_serves_client_metadata(forge) {
        bail!(
            "the forge {forge} is not on https, so authorization servers cannot load tt's client \
             metadata from it; use `tt sign --login --client localhost`, or with a dev-identity \
             build `--client-metadata-url <https URL>`"
        );
    }
    let url = client.client_id();
    let fetch = ctx.fetch(Duration::from_secs(10), None)?;
    let fetched = fetch
        .get(&url, 64 * 1024)
        .await
        .map_err(|error| anyhow!("the client metadata at {url} could not be loaded: {error}"))?;
    if !fetched.status.is_success() {
        bail!(
            "the client metadata at {url} could not be loaded: the forge answered {}",
            fetched.status
        );
    }
    let document: Value = serde_json::from_slice(&fetched.body)
        .map_err(|_| anyhow!("the client metadata at {url} is not JSON"))?;
    if document.get("client_id").and_then(Value::as_str) != Some(url.as_str()) {
        bail!("the client metadata at {url} names another client_id");
    }
    Ok(())
}

/// Checks that the hosted client document at `url` is the one `tt` needs, naming the document
/// to host when it is not.
async fn check_hosted(ctx: &Ctx, url: &str) -> Result<()> {
    let expected = hosted_document(url);
    let host_it = || {
        format!(
            "Host exactly this JSON at {url} (served as application/json over public https):\n{}",
            serde_json::to_string_pretty(&expected).unwrap_or_default()
        )
    };
    let fetch = ctx.fetch(Duration::from_secs(10), None)?;
    let fetched = match fetch.get(url, 64 * 1024).await {
        Ok(fetched) if fetched.status.is_success() => fetched,
        Ok(fetched) => bail!(
            "the client metadata at {url} could not be loaded (answer {}). {}",
            fetched.status,
            host_it()
        ),
        Err(error) => bail!(
            "the client metadata at {url} could not be loaded: {error}. {}",
            host_it()
        ),
    };
    let document: Value = serde_json::from_slice(&fetched.body)
        .map_err(|_| anyhow!("the client metadata at {url} is not JSON. {}", host_it()))?;
    let same = |key: &str| document.get(key) == expected.get(key);
    let ok = [
        "client_id",
        "redirect_uris",
        "scope",
        "token_endpoint_auth_method",
    ]
    .iter()
    .all(|key| same(key))
        && document.get("dpop_bound_access_tokens") == Some(&Value::Bool(true))
        && document.get("application_type").and_then(Value::as_str) == Some("native");
    if !ok {
        bail!(
            "the client metadata at {url} is not tt's development client. {}",
            host_it()
        );
    }
    Ok(())
}

/// Parses `--client-metadata-url`: an `https` URL with a path (plain `http` too under
/// `TARTAN_DEV_IDENTITY`, for the devnet). Only `dev-identity` builds offer the hosted
/// development client.
pub fn hosted_client(url: &str, mode: tartan_atproto::Mode) -> Result<ClientSpec> {
    if !cfg!(feature = "dev-identity") {
        bail!(
            "--client-metadata-url is a development option, and this tt is a release build \
             without it; build tt with `--features dev-identity`"
        );
    }
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|_| anyhow!("{url:?} is not a URL for --client-metadata-url"))?;
    let scheme_ok =
        parsed.scheme() == "https" || (mode.is_development() && parsed.scheme() == "http");
    if !scheme_ok || parsed.host_str().is_none() || parsed.path() == "/" {
        bail!(
            "--client-metadata-url must be an https URL with a path, such as \
             https://example.com/tt-client-metadata.json (got {url})"
        );
    }
    Ok(ClientSpec::Hosted {
        client_id: parsed.to_string(),
    })
}

/// Signs in for signing: PAR, the browser, the loopback callback and the code exchange.
pub async fn login(request: LoginRequest<'_>) -> Result<SigningSession> {
    let ctx = request.ctx;
    let resolver = ctx.resolver()?;
    let (account, hint) = resolve_account(ctx, &resolver, &request.who).await?;
    check_client_metadata(ctx, &request.client).await?;
    let listener = Loopback::bind(request.port).await?;
    let client_metadata = request.client.metadata(listener.port(), request.scope)?;
    let issuer = account.server.issuer.to_string();
    let authorization_endpoint = account.server.authorization_endpoint.to_string();
    let mut metadata = OAuthMetadata {
        server_metadata: account.server.clone(),
        client_metadata,
        keyset: None,
    };
    let pushed = {
        let resolver = resolver.clone();
        let mut metadata = metadata.clone();
        let hint = SmolStr::from(hint);
        guarded(async move { par(&resolver, Some(hint), None, &mut metadata, None).await }).await?
    };
    let auth = match pushed {
        Ok(auth) => auth,
        Err(error) => return Err(par_refused(&request.client, &issuer, &error)),
    };
    // The PAR may have updated the server metadata's DPoP algorithms; keep the copy in sync.
    metadata.server_metadata = account.server.clone();

    let location = format!(
        "{authorization_endpoint}{}client_id={}&request_uri={}",
        if authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        },
        percent(&request.client.client_id()),
        percent(&auth.request_uri)
    );
    loopback::present(
        &location,
        &if request.scope == DELETE_SCOPE {
            format!("let tt delete a signature record of {}", account.did)
        } else {
            format!("let tt sign Git objects as {}", account.did)
        },
        request.no_open,
    );
    let query = listener
        .wait(CALLBACK, &loopback::page("tt has your answer."))
        .await?;
    if query.get("state").map(String::as_str) != Some(auth.state.as_str()) {
        bail!("the authorization server's redirect carried the wrong state; nothing was stored");
    }
    if let Some(error) = query.get("error") {
        let description = query
            .get("error_description")
            .map(String::as_str)
            .unwrap_or_default();
        if error == "access_denied" {
            bail!(
                "the authorization was cancelled or refused ({error}{})",
                suffix(description)
            );
        }
        bail!(
            "the authorization server refused: {error}{}",
            suffix(description)
        );
    }
    if query.get("iss").map(String::as_str) != Some(issuer.as_str()) {
        bail!("the authorization server's redirect did not name its issuer {issuer}");
    }
    let code = query
        .get("code")
        .cloned()
        .ok_or_else(|| anyhow!("the authorization server's redirect carried no code"))?;

    let mut dpop = DpopClientData {
        dpop_key: auth.dpop_data.dpop_key.clone(),
        dpop_authserver_nonce: auth
            .dpop_data
            .dpop_authserver_nonce
            .clone()
            .unwrap_or_default(),
        dpop_host_nonce: SmolStr::default(),
    };
    let exchanged = {
        let resolver = resolver.clone();
        let metadata = metadata.clone();
        let verifier = auth.pkce_verifier.to_string();
        let mut dpop_copy = dpop.clone();
        guarded(async move {
            let redirect = metadata.client_metadata.redirect_uris[0].to_string();
            let result = oauth_request::<OAuthTokenResponse, _, _, _>(
                &resolver,
                &mut dpop_copy,
                OAuthRequest::Token(TokenRequestParameters {
                    grant_type: TokenGrantType::AuthorizationCode,
                    code: code.as_str(),
                    redirect_uri: redirect.as_str(),
                    code_verifier: verifier.as_str(),
                }),
                &metadata,
            )
            .await;
            (result, dpop_copy)
        })
        .await?
    };
    let tokens = match exchanged {
        (Ok(tokens), used) => {
            dpop = used;
            tokens
        }
        (Err(error), _) => {
            return Err(match oauth_error(&error) {
                Some((code, description)) => anyhow!(
                    "the authorization server refused the code exchange: {code}{}",
                    suffix(&description)
                ),
                None => anyhow!("the code exchange failed: {error}"),
            });
        }
    };
    let sub = tokens.sub.as_deref().unwrap_or_default();
    if sub != account.did.as_str() {
        bail!(
            "the authorization server signed in {} instead of {}; nothing was stored",
            if sub.is_empty() { "nobody" } else { sub },
            account.did
        );
    }
    let granted = tokens.scope.as_deref().unwrap_or_default().to_owned();
    let needed = if request.scope == DELETE_SCOPE {
        Scope::<SmolStr>::repo_delete(COLLECTION)
    } else {
        Scope::<SmolStr>::repo_create(COLLECTION)
    }
    .map_err(|_| anyhow!("the signature collection is not a valid scope"))?;
    let grants = Scopes::new(SmolStr::from(granted.as_str())).is_ok_and(|s| s.grants(&needed));
    if !grants {
        bail!(
            "the authorization server granted `{granted}`, which does not include \
             `{needed}`; your account's server does not support the granular repo: scope tt \
             needs, and tt never asks for broader access"
        );
    }
    let now = state::now_ms();
    Ok(SigningSession {
        did: account.did.as_str().to_owned(),
        client: request.client.clone(),
        pds: account.pds,
        issuer,
        created_at: now,
        scope: granted,
        access_token: tokens.access_token.to_string(),
        refresh_token: tokens.refresh_token.map(|token| token.to_string()),
        expires_at: tokens
            .expires_in
            .map(|seconds| now.saturating_add(seconds.saturating_mul(1000))),
        dpop,
    })
}

fn suffix(description: &str) -> String {
    if description.is_empty() {
        String::new()
    } else {
        format!(": {description}")
    }
}

fn par_refused(client: &ClientSpec, issuer: &str, error: &RequestError) -> anyhow::Error {
    let Some((code, description)) = oauth_error(error) else {
        return anyhow!("the pushed authorization request to {issuer} failed: {error}");
    };
    let detail = suffix(&description);
    match code.as_str() {
        "invalid_scope" => anyhow!(
            "{issuer} refused the scope `{CREATE_SCOPE}` ({code}{detail}). Your account's server \
             does not support the granular repo: scopes tt needs, so tt cannot sign with it; tt \
             never asks for broader access such as transition:generic"
        ),
        "invalid_redirect_uri" | "invalid_client_metadata" | "invalid_client"
            if matches!(client, ClientSpec::Forge { .. }) =>
        {
            anyhow!(
                "{issuer} refused tt's client {} ({code}{detail}). If your forge is not reachable \
                 over public https, or the server does not accept loopback redirects for it, try \
                 `tt sign --login --client localhost`",
                client.client_id()
            )
        }
        "invalid_client" | "invalid_client_metadata" if *client == ClientSpec::Localhost => {
            anyhow!(
                "{issuer} refused the authorization request: {code}{detail}. This server does not \
                 accept ATProto's http://localhost client; sign in through a forge on public \
                 https, or, with a dev-identity build of tt, host tt's client metadata and pass \
                 `--client-metadata-url <https URL>`"
            )
        }
        _ => anyhow!("{issuer} refused the authorization request: {code}{detail}"),
    }
}

/// Why a refresh failed (design D7, *Refresh failures are split*).
#[derive(Debug, Clone)]
pub enum RefreshFailure {
    /// The session is over: `invalid_grant`, refused or revoked, or the account moved.
    Authorization(String),
    /// Worth retrying later: the authorization server is unreachable, answered 5xx, or could
    /// not load the client metadata.
    Transient(String),
}

impl std::fmt::Display for RefreshFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshFailure::Authorization(message) | RefreshFailure::Transient(message) => {
                f.write_str(message)
            }
        }
    }
}

fn relogin(did: &str, cause: &str) -> RefreshFailure {
    RefreshFailure::Authorization(format!(
        "the signing session of {did} can no longer be used ({cause}). It ended, was revoked at \
         your authorization server, or was refreshed by a copy of it on another machine (signing \
         sessions cannot be shared between machines). Run `tt sign --login` here"
    ))
}

/// Returns a session whose access token is fresh, refreshing it under the refresh lock when it
/// is not. The lock is held around the refresh, and the stored session is re-read after taking
/// it, so parallel signings refresh once.
pub async fn fresh_session(
    ctx: &Ctx,
    session: SigningSession,
) -> Result<SigningSession, RefreshFailure> {
    if session.fresh() {
        return Ok(session);
    }
    let did = session.did.clone();
    let lock_path = ctx
        .dirs
        .locks()
        .join(format!("{}.lock", paths::file_name(&did)));
    let lock = paths::lock_file(&lock_path)
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?;
    tokio::task::block_in_place(|| lock.lock())
        .map_err(|error| RefreshFailure::Transient(format!("locking {did}'s session: {error}")))?;
    let current = SigningSession::load(ctx, &did)
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?
        .ok_or_else(|| relogin(&did, "no stored session"))?;
    if current.fresh() {
        return Ok(current);
    }
    let refreshed = refresh(ctx, current).await?;
    refreshed
        .save(ctx)
        .map_err(|error| RefreshFailure::Transient(format!("storing the session: {error}")))?;
    drop(lock);
    Ok(refreshed)
}

/// Forces a refresh (after the PDS refused the access token), under the refresh lock.
pub async fn force_refresh(
    ctx: &Ctx,
    session: SigningSession,
) -> Result<SigningSession, RefreshFailure> {
    let mut stale = session;
    stale.expires_at = Some(0);
    let did = stale.did.clone();
    let lock_path = ctx
        .dirs
        .locks()
        .join(format!("{}.lock", paths::file_name(&did)));
    let lock = paths::lock_file(&lock_path)
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?;
    tokio::task::block_in_place(|| lock.lock())
        .map_err(|error| RefreshFailure::Transient(format!("locking {did}'s session: {error}")))?;
    let current = SigningSession::load(ctx, &did)
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?
        .ok_or_else(|| relogin(&did, "no stored session"))?;
    // Another process may have refreshed since this one read the session.
    if current.access_token != stale.access_token && current.fresh() {
        return Ok(current);
    }
    let refreshed = refresh(ctx, current).await?;
    refreshed
        .save(ctx)
        .map_err(|error| RefreshFailure::Transient(format!("storing the session: {error}")))?;
    drop(lock);
    Ok(refreshed)
}

async fn refresh(ctx: &Ctx, mut session: SigningSession) -> Result<SigningSession, RefreshFailure> {
    let did = session.did.clone();
    let Some(refresh_token) = session.refresh_token.clone() else {
        return Err(relogin(&did, "the session has no refresh token"));
    };
    let resolver = ctx
        .resolver()
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?;
    // The account must still be served by the authorization server the session belongs to.
    let parsed = Did::parse(&did, ctx.mode).map_err(|error| relogin(&did, &error.to_string()))?;
    let identity = match resolver.resolve_did(&parsed).await {
        Ok(resolved) => resolved.identity,
        Err(ResolveError::Unavailable(reason)) => {
            return Err(RefreshFailure::Transient(format!(
                "resolving {did} failed: {reason}"
            )));
        }
        Err(error) => return Err(relogin(&did, &error.to_string())),
    };
    let pds = identity
        .and_then(|identity| identity.pds)
        .ok_or_else(|| relogin(&did, "the DID names no PDS"))?;
    if pds != session.pds {
        let moved = guarded({
            let resolver = resolver.clone();
            let pds = pds.clone();
            async move { resolver.get_resource_server_metadata(&pds).await }
        })
        .await
        .ok()
        .and_then(Result::ok);
        match moved {
            Some(server) if server.issuer.as_str() == session.issuer => session.pds = pds,
            Some(_) => {
                return Err(RefreshFailure::Authorization(format!(
                    "{did} has moved to another PDS ({pds}); run `tt sign --login`"
                )));
            }
            None => {
                return Err(RefreshFailure::Transient(format!(
                    "the new PDS {pds} of {did} could not be reached"
                )));
            }
        }
    }
    let server = guarded({
        let resolver = resolver.clone();
        let issuer = session.issuer.clone();
        async move { resolver.get_authorization_server_metadata(&issuer).await }
    })
    .await
    .ok()
    .and_then(Result::ok)
    .ok_or_else(|| {
        RefreshFailure::Transient(format!(
            "the authorization server {} could not be reached",
            session.issuer
        ))
    })?;
    let metadata = OAuthMetadata {
        server_metadata: server,
        client_metadata: session
            .client
            .metadata(0, CREATE_SCOPE)
            .map_err(|error| RefreshFailure::Transient(error.to_string()))?,
        keyset: None,
    };
    let outcome = {
        let resolver = resolver.clone();
        let mut dpop = session.dpop.clone();
        guarded(async move {
            let result = oauth_request::<OAuthTokenResponse, _, _, _>(
                &resolver,
                &mut dpop,
                OAuthRequest::Refresh(RefreshRequestParameters {
                    grant_type: TokenGrantType::RefreshToken,
                    refresh_token: refresh_token.as_str(),
                    scope: None,
                }),
                &metadata,
            )
            .await;
            (result, dpop)
        })
        .await
        .map_err(|error| RefreshFailure::Transient(error.to_string()))?
    };
    let client_id = session.client.client_id();
    let tokens = match outcome {
        (Ok(tokens), dpop) => {
            session.dpop = dpop;
            tokens
        }
        (Err(error), dpop) => {
            session.dpop = dpop;
            return Err(classify_refresh(&did, &client_id, &session.issuer, &error));
        }
    };
    if tokens.sub.as_deref() != Some(did.as_str()) {
        return Err(relogin(
            &did,
            "the authorization server answered for another account",
        ));
    }
    let now = state::now_ms();
    session.access_token = tokens.access_token.to_string();
    if let Some(next) = tokens.refresh_token {
        session.refresh_token = Some(next.to_string());
    }
    if let Some(scope) = tokens.scope {
        session.scope = scope.to_string();
    }
    session.expires_at = tokens
        .expires_in
        .map(|seconds| now.saturating_add(seconds.saturating_mul(1000)));
    Ok(session)
}

fn classify_refresh(
    did: &str,
    client_id: &str,
    issuer: &str,
    error: &RequestError,
) -> RefreshFailure {
    if let Some((code, description)) = oauth_error(error) {
        return match code.as_str() {
            "invalid_client_metadata" | "invalid_client" => RefreshFailure::Transient(format!(
                "the client metadata at {client_id} could not be loaded by your authorization \
                 server {issuer} ({code}{}); is the forge down?",
                suffix(&description)
            )),
            "invalid_grant" => relogin(did, &format!("{code}{}", suffix(&description))),
            _ => relogin(
                did,
                &format!("{issuer} refused it: {code}{}", suffix(&description)),
            ),
        };
    }
    match error.kind() {
        RequestErrorKind::HttpStatus(status)
            if status.as_u16() == 429 || status.is_server_error() =>
        {
            RefreshFailure::Transient(format!(
                "the authorization server {issuer} answered {status}"
            ))
        }
        RequestErrorKind::HttpStatus(status) => {
            relogin(did, &format!("{issuer} answered {status}"))
        }
        _ => RefreshFailure::Transient(format!(
            "the authorization server {issuer} could not be reached: {error}"
        )),
    }
}

/// Revokes the session's tokens at the authorization server's `revocation_endpoint`, when it
/// lists one. Failures are reported, not fatal: the caller drops the tokens anyway.
pub async fn revoke_session(ctx: &Ctx, session: &SigningSession) -> Result<bool> {
    let resolver = ctx.resolver()?;
    let server = guarded({
        let resolver = resolver.clone();
        let issuer = session.issuer.clone();
        async move { resolver.get_authorization_server_metadata(&issuer).await }
    })
    .await?
    .map_err(|error| anyhow!("{error}"))?;
    if server.revocation_endpoint.is_none() {
        return Ok(false);
    }
    let metadata = OAuthMetadata {
        server_metadata: server,
        client_metadata: session.client.metadata(0, CREATE_SCOPE)?,
        keyset: None,
    };
    let mut tokens = vec![session.access_token.clone()];
    tokens.extend(session.refresh_token.clone());
    for token in tokens {
        let resolver = resolver.clone();
        let metadata = metadata.clone();
        let mut dpop = session.dpop.clone();
        guarded(async move { revoke(&resolver, &mut dpop, &token, &metadata).await })
            .await?
            .map_err(|error| anyhow!("revoking at the authorization server: {error}"))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_localhost_client_id_declares_both_redirects_and_scopes() {
        let id = ClientSpec::Localhost.client_id();
        let url = reqwest::Url::parse(&id).expect("url");
        assert_eq!(url.host_str(), Some("localhost"));
        assert_eq!(url.port(), None);
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let redirects: Vec<&str> = pairs
            .iter()
            .filter(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            redirects,
            ["http://127.0.0.1/callback", "http://[::1]/callback"]
        );
        let scope = &pairs.iter().find(|(k, _)| k == "scope").expect("scope").1;
        let tokens: Vec<&str> = scope.split(' ').collect();
        assert_eq!(
            tokens,
            [
                "atproto",
                "repo:academy.rawkode.tartan.signature?action=create",
                "repo:academy.rawkode.tartan.signature?action=delete"
            ]
        );
    }

    #[test]
    fn the_hosted_client_document_matches_the_forge_one() {
        let url = "https://example.com/tt.json";
        let document = hosted_document(url);
        assert_eq!(document["client_id"], url);
        assert_eq!(document["scope"], DECLARED_SCOPE);
        assert_eq!(
            document["redirect_uris"],
            json!(["http://127.0.0.1/callback", "http://[::1]/callback"])
        );
        assert_eq!(document["token_endpoint_auth_method"], "none");
        let spec = ClientSpec::Hosted {
            client_id: url.to_owned(),
        };
        assert_eq!(spec.client_id(), url);
        assert_eq!(spec.forge(), None);
        let metadata = spec.metadata(29100, CREATE_SCOPE).expect("metadata");
        assert_eq!(metadata.client_id.as_str(), url);
    }

    #[test]
    fn hosted_clients_need_https_and_a_dev_build() {
        let production = tartan_atproto::Mode::Production;
        let parsed = hosted_client("https://example.com/tt.json", production);
        if cfg!(feature = "dev-identity") {
            assert_eq!(
                parsed.expect("accepted"),
                ClientSpec::Hosted {
                    client_id: "https://example.com/tt.json".to_owned()
                }
            );
            assert!(hosted_client("http://example.com/tt.json", production).is_err());
            assert!(hosted_client("https://example.com/", production).is_err());
        } else {
            assert!(parsed.is_err());
        }
    }

    #[test]
    fn the_forge_client_id_is_its_metadata_url() {
        let spec = ClientSpec::Forge {
            forge: "https://forge.example".into(),
        };
        assert_eq!(
            spec.client_id(),
            "https://forge.example/-/auth/tt/client-metadata.json"
        );
        let metadata = spec.metadata(4321, CREATE_SCOPE).expect("metadata");
        assert_eq!(
            metadata.redirect_uris[0].as_str(),
            "http://127.0.0.1:4321/callback"
        );
        assert_eq!(metadata.scope.as_deref(), Some(CREATE_SCOPE));
    }
}
