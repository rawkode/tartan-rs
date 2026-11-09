//! Who is calling and what they may do (design D4, spec forge-access).
//!
//! [`authenticate`] runs on every `/-/api` route. It resolves the request to a principal: a
//! bearer `tartan_pat_` access token (an agent acting for a DID) or `tartan_cli_` CLI session,
//! else the session cookie, else anonymous. It then checks the route's class against the
//! principal's rights, which are computed per request from the deployment configuration's
//! `owners` and `readers`, and, for cookie-authenticated writes, the `Origin` header.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{MatchedPath, RawPathParams, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;
use rivetkit::client::Client;
use sha2::{Digest, Sha256};
use tartan_atproto::resolver::VerifyingResolver;
use tartan_config::deployment::Readers;
use tartan_forge::{Caller, Clock, identity_handle};
use tartan_identity_api::{
    Authenticate, Authenticated, CLI_SESSION_PREFIX, Credential, CredentialKind, SessionKind,
    TOKEN_PREFIX, TokenScope,
};

use crate::ApiError;
use crate::client_ip::RateLimiter;
use crate::gateway::GatewayState;
use crate::settings::{PublicUrl, WebClient};

/// How long an answer about a credential is reused within one gateway process.
const CACHE_MS: i64 = 30_000;

/// What the gateway needs to sign people in and to authorize requests.
#[derive(Clone)]
pub struct AccessConfig {
    /// The browser-facing origin (`TARTAN_PUBLIC_URL`).
    pub public_url: PublicUrl,
    /// The web sign-in's OAuth client.
    pub client: WebClient,
    /// The identity resolver, over the fetch-safe client.
    pub resolver: VerifyingResolver,
    /// The deployment configuration's owners.
    pub owners: Vec<String>,
    /// The deployment configuration's readers.
    pub readers: Readers,
    /// Reverse proxies whose `X-Forwarded-For` gives the client address.
    pub trusted_proxies: Vec<IpNet>,
    /// The forge's clock.
    pub clock: Arc<dyn Clock>,
}

/// The gateway's sign-in and access state.
pub struct Access {
    pub(crate) config: AccessConfig,
    cache: Mutex<HashMap<String, (Option<Authenticated>, i64)>>,
    /// Starting sign-ins, per client address.
    pub(crate) sign_ins: RateLimiter,
    /// `GET /-/api/people` lookups of DIDs not cached yet, per client address.
    pub(crate) people_lookups: RateLimiter,
    /// `POST /-/api/people/resolve`, per client address.
    pub(crate) resolves: RateLimiter,
    /// `POST /-/auth/cli/token`, per client address.
    pub(crate) cli_tokens: RateLimiter,
    /// `self` calls that insert a row or make the forge fetch, per principal DID (design D4,
    /// *Route classes*): 600 an hour, bursts of 100.
    pub(crate) self_calls: RateLimiter,
    /// DIDs whose background lookup is running, so each has at most one in flight.
    pub(crate) lookups: Mutex<std::collections::HashSet<String>>,
}

impl Access {
    /// Builds the access state from `config`.
    pub fn new(config: AccessConfig) -> Self {
        let clock = config.clock.clone();
        Self {
            config,
            cache: Mutex::new(HashMap::new()),
            sign_ins: RateLimiter::new(30, 6_000, clock.clone()),
            people_lookups: RateLimiter::new(200, 500, clock.clone()),
            resolves: RateLimiter::new(30, 2_000, clock.clone()),
            cli_tokens: RateLimiter::new(10, 6_000, clock.clone()),
            self_calls: RateLimiter::new(100, 6_000, clock),
            lookups: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// The session cookie's name: `__Host-` prefixed except over plain HTTP in development.
    pub(crate) fn session_cookie(&self) -> &'static str {
        if self.config.public_url.is_http() {
            "tartan_session"
        } else {
            "__Host-tartan_session"
        }
    }

    /// The pre-auth cookie's name (design D1, *Browser binding*).
    pub(crate) fn oauth_cookie(&self) -> &'static str {
        if self.config.public_url.is_http() {
            "tartan_oauth"
        } else {
            "__Host-tartan_oauth"
        }
    }

    /// The outcome cookie's name (design D1, *Outcome details*).
    pub(crate) fn outcome_cookie(&self) -> &'static str {
        if self.config.public_url.is_http() {
            "tartan_outcome"
        } else {
            "__Host-tartan_outcome"
        }
    }

    /// A `Set-Cookie` value for `name`, HttpOnly, SameSite=Lax, Path=/, Secure over HTTPS.
    pub(crate) fn cookie(&self, name: &str, value: &str, max_age: Option<u64>) -> HeaderValue {
        let mut cookie = format!("{name}={value}; HttpOnly; SameSite=Lax; Path=/");
        if !self.config.public_url.is_http() {
            cookie.push_str("; Secure");
        }
        if let Some(age) = max_age {
            cookie.push_str(&format!("; Max-Age={age}"));
        }
        HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    /// A `Set-Cookie` value that clears `name`.
    pub(crate) fn clear_cookie(&self, name: &str) -> HeaderValue {
        self.cookie(name, "", Some(0))
    }

    /// The rights `did` has now (design D4, *Rights*).
    pub fn rights_of(&self, did: &str) -> Rights {
        if self.is_owner(did) {
            Rights::ALL
        } else if self.config.readers.admits(did) {
            Rights {
                read: true,
                write: false,
                admin: false,
            }
        } else {
            Rights::NONE
        }
    }

    /// Whether `did` is one of the owners.
    pub fn is_owner(&self, did: &str) -> bool {
        self.config.owners.iter().any(|owner| owner == did)
    }

    /// Looks up a presented secret, through the per-process cache.
    pub(crate) async fn lookup(
        &self,
        client: &Client,
        secret: &str,
        kind: CredentialKind,
    ) -> Result<Option<Authenticated>, ApiError> {
        let digest = hex::encode(Sha256::digest(secret.as_bytes()));
        let key = format!("{kind:?}:{digest}");
        let now = self.config.clock.now_ms();
        if let Some((answer, at)) = self.lock_cache().get(&key)
            && now - at < CACHE_MS
        {
            return Ok(answer.clone());
        }
        let answer = identity_handle(client)?
            .send(Authenticate {
                secret_sha256: digest,
                kind,
            })
            .await?;
        let mut cache = self.lock_cache();
        if cache.len() > 50_000 {
            cache.retain(|_, (_, at)| now - *at < CACHE_MS);
        }
        cache.insert(key, (answer.clone(), now));
        Ok(answer)
    }

    /// Forgets cached answers for the session or token `id`, after it ended or was revoked.
    pub(crate) fn forget(&self, id: &str) {
        self.lock_cache().retain(|_, (answer, _)| {
            !answer
                .as_ref()
                .is_some_and(|answer| match &answer.credential {
                    Credential::Session { id: cached, .. }
                    | Credential::Token { id: cached, .. } => cached == id,
                })
        });
    }

    /// Forgets a cached answer for a presented secret.
    pub(crate) fn forget_secret(&self, secret: &str, kind: CredentialKind) {
        let digest = hex::encode(Sha256::digest(secret.as_bytes()));
        self.lock_cache().remove(&format!("{kind:?}:{digest}"));
    }

    fn lock_cache(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (Option<Authenticated>, i64)>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Checks a browser request's `Origin` against `TARTAN_PUBLIC_URL` (design D4, *CSRF*).
    pub(crate) fn check_origin(&self, headers: &HeaderMap) -> Option<Response> {
        let expected = self.config.public_url.origin();
        let received = headers
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        if received == Some(expected) {
            return None;
        }
        tracing::warn!(
            expected,
            received = received.unwrap_or("(none)"),
            "refused a browser write whose Origin is not TARTAN_PUBLIC_URL"
        );
        Some(
            (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!(
                    "Origin must be {expected} (TARTAN_PUBLIC_URL); got {}\n",
                    received.unwrap_or("none")
                ),
            )
                .into_response(),
        )
    }

    /// The session cookie's value, if the request carries one.
    pub(crate) fn session_secret<'h>(&self, headers: &'h HeaderMap) -> Option<&'h str> {
        cookie_value(headers, self.session_cookie())
    }
}

/// The value of cookie `name` in the request's `Cookie` headers.
pub(crate) fn cookie_value<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// What a principal may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rights {
    /// `GET` routes and extension `render` calls.
    pub read: bool,
    /// Extension `tool` and `act` calls.
    pub write: bool,
    /// Creating orgs and repositories, placements and subscribers.
    pub admin: bool,
}

impl Rights {
    /// Every right: an owner.
    pub const ALL: Rights = Rights {
        read: true,
        write: true,
        admin: true,
    };
    /// No right: a signed-in person the forge does not let read.
    pub const NONE: Rights = Rights {
        read: false,
        write: false,
        admin: false,
    };

    /// The rights as names, for `GET /-/api/me`.
    pub fn names(self) -> Vec<&'static str> {
        [
            (self.read, "read"),
            (self.write, "write"),
            (self.admin, "admin"),
        ]
        .into_iter()
        .filter_map(|(has, name)| has.then_some(name))
        .collect()
    }

    fn within(self, scopes: &[TokenScope]) -> Rights {
        Rights {
            read: self.read && scopes.contains(&TokenScope::Read),
            write: self.write && scopes.contains(&TokenScope::Write),
            admin: self.admin && scopes.contains(&TokenScope::Admin),
        }
    }
}

/// How a principal authenticated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A browser or CLI session: the person.
    Session {
        /// The session's id.
        id: String,
        /// Browser (cookie) or CLI (`tartan_cli_` bearer).
        kind: SessionKind,
    },
    /// An access token: an agent acting for the person.
    Token {
        /// The token's id.
        id: String,
        /// The repositories it is limited to, if any.
        repos: Option<Vec<String>>,
    },
}

/// The caller of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    /// The person's DID, or the DID a token acts for.
    pub did: String,
    /// How the principal authenticated.
    pub kind: PrincipalKind,
    /// What it may do now.
    pub rights: Rights,
    /// Whether the DID is an owner.
    pub owner: bool,
}

impl Principal {
    /// The caller extensions see.
    pub fn caller(&self) -> Caller {
        match &self.kind {
            PrincipalKind::Session { .. } => Caller::user(&self.did),
            PrincipalKind::Token { id, .. } => Caller::agent(id, &self.did),
        }
    }

    /// The session id, for a session principal.
    pub fn session_id(&self) -> Option<&str> {
        match &self.kind {
            PrincipalKind::Session { id, .. } => Some(id),
            PrincipalKind::Token { .. } => None,
        }
    }

    /// `browser`, `cli` or `token`.
    pub fn kind_name(&self) -> &'static str {
        match &self.kind {
            PrincipalKind::Session { kind, .. } => kind.as_str(),
            PrincipalKind::Token { .. } => "token",
        }
    }
}

/// The request's principal, or anonymous; put into every `/-/api` request's extensions.
#[derive(Clone, Debug)]
pub struct RequestPrincipal(pub Option<Principal>);

/// The class of a route (design D4, *Route classes*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    /// `GET /-/api/me`: anyone, anonymous included.
    Me,
    /// The caller's own sessions: any signed-in session principal, whatever its rights.
    Sessions,
    /// Access tokens: a session principal with some right.
    Tokens,
    /// A signer's own signature records: the principal whose DID is the route's `{did}`,
    /// whatever its rights, or an owner.
    OwnDid,
    /// Re-checking an `unverifiable` result: an owner, or the principal whose DID is stored on
    /// the result, whatever its rights; the handler decides which, since only the repository
    /// knows the stored DID.
    Recheck,
    /// Reading.
    Read,
    /// Extension `tool` and `act` calls.
    Write,
    /// Forge administration.
    Admin,
}

/// The class of `method` on `path` (relative to `/-/api`, as the router matched it).
pub fn classify(method: &Method, path: &str) -> RouteClass {
    let is_get = method == Method::GET || method == Method::HEAD;
    match path {
        "/me" => RouteClass::Me,
        "/sessions" | "/sessions/current" | "/sessions/{id}" => RouteClass::Sessions,
        "/tokens" | "/tokens/{id}" => RouteClass::Tokens,
        "/people/{did}/signatures/{rkey}/refresh" => RouteClass::OwnDid,
        "/orgs/{org}/repos/{repo}/commits/{sha}/signature/recheck" => RouteClass::Recheck,
        "/people" | "/people/resolve" => RouteClass::Read,
        "/orgs" | "/orgs/{org}/repos" if !is_get => RouteClass::Admin,
        "/placements" | "/subscribers/{name}" | "/subscribers/{name}/events" => RouteClass::Admin,
        "/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}" => RouteClass::Write,
        _ if is_get => RouteClass::Read,
        // A new non-GET route is forge administration until it is classified.
        _ => RouteClass::Admin,
    }
}

/// Whether a token limited to `repos` may use a route with these `org`/`repo` parameters.
fn repos_allow(repos: &[String], org: Option<&str>, repo: Option<&str>, reading: bool) -> bool {
    let Some(org) = org else {
        return false;
    };
    repos.iter().any(|entry| {
        let Some((entry_org, entry_repo)) = entry.split_once('/') else {
            return false;
        };
        if entry_org != org {
            return false;
        }
        match repo {
            Some(repo) => entry_repo == "*" || entry_repo == repo,
            // The org's own routes, for reading only.
            None => reading,
        }
    })
}

fn forbidden(message: &str) -> Response {
    ApiError::Forbidden(message.to_owned()).into_response()
}

/// The `authenticate` middleware of the `/-/api` router.
pub async fn authenticate(
    State(state): State<GatewayState>,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    mut request: Request,
    next: Next,
) -> Response {
    let params = params.ok();
    let access = state.access.clone();
    let headers = request.headers().clone();
    let bearer = headers
        .get(header::AUTHORIZATION)
        .map(|value| value.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")));
    let (found, by_cookie) = match bearer {
        Some(Some(secret)) => {
            let kind = if secret.starts_with(TOKEN_PREFIX) {
                CredentialKind::Token
            } else if secret.starts_with(CLI_SESSION_PREFIX) {
                CredentialKind::CliSession
            } else {
                return ApiError::Unauthorized.into_response();
            };
            match access.lookup(&state.client, secret, kind).await {
                // A bad bearer credential is refused even when a good cookie comes with it.
                Ok(None) => return ApiError::Unauthorized.into_response(),
                Ok(found) => (found, false),
                Err(error) => return error.into_response(),
            }
        }
        Some(None) => return ApiError::Unauthorized.into_response(),
        None => match access.session_secret(&headers) {
            Some(secret) => match access
                .lookup(&state.client, secret, CredentialKind::BrowserSession)
                .await
            {
                Ok(found) => (found, true),
                Err(error) => return error.into_response(),
            },
            None => (None, false),
        },
    };
    let principal = found.map(|authenticated| {
        let did_rights = access.rights_of(&authenticated.did);
        let owner = access.is_owner(&authenticated.did);
        match authenticated.credential {
            Credential::Session { id, kind } => Principal {
                did: authenticated.did,
                kind: PrincipalKind::Session { id, kind },
                rights: did_rights,
                owner,
            },
            Credential::Token { id, scopes, repos } => Principal {
                did: authenticated.did,
                kind: PrincipalKind::Token { id, repos },
                rights: did_rights.within(&scopes),
                owner,
            },
        }
    });

    let path = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_default();
    let path = path.strip_prefix("/-/api").unwrap_or(&path).to_owned();
    let method = request.method().clone();
    let mut class = classify(&method, &path);
    let param = |name: &str| {
        params.as_ref().and_then(|params| {
            params
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_owned())
        })
    };
    if class == RouteClass::Write && param("call").as_deref() == Some("render") {
        class = RouteClass::Read;
    }

    let Some(principal) = principal else {
        if class == RouteClass::Me {
            request.extensions_mut().insert(RequestPrincipal(None));
            return next.run(request).await;
        }
        return ApiError::Unauthorized.into_response();
    };

    let is_get = method == Method::GET || method == Method::HEAD;
    if by_cookie
        && !is_get
        && let Some(response) = access.check_origin(&headers)
    {
        return response;
    }

    if let PrincipalKind::Token {
        repos: Some(repos), ..
    } = &principal.kind
    {
        let exempt = class == RouteClass::Me || (path == "/people" && is_get);
        if !exempt
            && !repos_allow(
                repos,
                param("org").as_deref(),
                param("repo").as_deref(),
                class == RouteClass::Read,
            )
        {
            return forbidden("this token is limited to other repositories");
        }
    }

    let rights = principal.rights;
    let allowed = match class {
        RouteClass::Me => true,
        RouteClass::Sessions => principal.session_id().is_some(),
        RouteClass::Tokens => {
            principal.session_id().is_some() && (rights.read || rights.write || rights.admin)
        }
        RouteClass::OwnDid => {
            param("did").as_deref() == Some(principal.did.as_str()) || principal.owner
        }
        RouteClass::Recheck => true,
        RouteClass::Read => rights.read,
        RouteClass::Write => rights.write,
        RouteClass::Admin => rights.admin,
    };
    if !allowed {
        let message = match class {
            RouteClass::Sessions | RouteClass::Tokens if principal.session_id().is_none() => {
                "only a browser or CLI session may do this, not an access token"
            }
            RouteClass::Write | RouteClass::Admin if rights.read => {
                "read-only: only forge owners can make changes until membership exists"
            }
            _ => "this forge is private; ask an owner for access",
        };
        return forbidden(message);
    }
    request
        .extensions_mut()
        .insert(RequestPrincipal(Some(principal)));
    next.run(request).await
}

/// Adds `X-Content-Type-Options: nosniff` and a sandboxing `Content-Security-Policy` to a
/// response (design D4, *Registry responses on the forge origin*).
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_classes() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(classify(&get, "/me"), RouteClass::Me);
        assert_eq!(classify(&get, "/orgs"), RouteClass::Read);
        assert_eq!(classify(&post, "/orgs"), RouteClass::Admin);
        assert_eq!(classify(&post, "/orgs/{org}/repos"), RouteClass::Admin);
        assert_eq!(classify(&Method::PUT, "/placements"), RouteClass::Admin);
        assert_eq!(
            classify(&get, "/subscribers/{name}/events"),
            RouteClass::Admin
        );
        assert_eq!(
            classify(
                &post,
                "/orgs/{org}/repos/{repo}/extensions/{extension}/{call}/{name}"
            ),
            RouteClass::Write
        );
        assert_eq!(classify(&post, "/people/resolve"), RouteClass::Read);
        assert_eq!(
            classify(&post, "/people/{did}/signatures/{rkey}/refresh"),
            RouteClass::OwnDid
        );
        assert_eq!(
            classify(&Method::DELETE, "/sessions/current"),
            RouteClass::Sessions
        );
        assert_eq!(classify(&post, "/tokens"), RouteClass::Tokens);
        assert_eq!(classify(&post, "/something/new"), RouteClass::Admin);
    }

    #[test]
    fn repository_limits() {
        let repos = vec!["acme/api".to_owned(), "beta/*".to_owned()];
        assert!(repos_allow(&repos, Some("acme"), Some("api"), false));
        assert!(!repos_allow(&repos, Some("acme"), Some("web"), true));
        assert!(repos_allow(&repos, Some("beta"), Some("anything"), false));
        assert!(
            repos_allow(&repos, Some("acme"), None, true),
            "its org, for reading"
        );
        assert!(!repos_allow(&repos, Some("acme"), None, false));
        assert!(!repos_allow(&repos, None, None, true), "forge-wide routes");
    }

    #[test]
    fn token_rights_are_scopes_within_the_did_rights() {
        let reader = Rights {
            read: true,
            ..Rights::NONE
        };
        assert_eq!(
            reader.within(&[TokenScope::Read, TokenScope::Write, TokenScope::Admin]),
            reader
        );
        assert_eq!(
            Rights::ALL.within(&[TokenScope::Write]).names(),
            vec!["write"]
        );
    }

    #[test]
    fn cookies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; tartan_session=abc; b=2"),
        );
        assert_eq!(cookie_value(&headers, "tartan_session"), Some("abc"));
        assert_eq!(cookie_value(&headers, "c"), None);
    }
}
