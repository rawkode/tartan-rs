//! The outbound HTTP client for identity work (design D9, *Fetch safety*).
//!
//! Every identity fetch, in the forge and in `tt`, goes through one [`FetchClient`]: DID
//! documents, PLC logs, handle resolution, `getRecord`, `getRepoStatus`, and every request
//! jacquard-oauth makes (PAR, token, revocation, metadata). DID documents and records name hosts
//! that whoever wrote them chooses, so the client refuses to reach anything that is not on the
//! public internet:
//!
//! - it is built with `.no_proxy()`, so `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` never move a
//!   request elsewhere;
//! - before every request and every redirect hop the URL is checked: `https` only, and a host
//!   that is an IP literal must pass the public-address predicate ([`is_public`]), because
//!   hyper-util's connector dials IP literals without asking the resolver;
//! - its DNS resolver drops every resolved address that fails the same predicate;
//! - requests time out after 10 seconds, and bodies are capped while they stream;
//! - redirects are followed only by [`FetchClient::get_following`] (handle resolution and
//!   did:web documents). [`FetchClient::send`], and so every jacquard request, never follows
//!   one, because a 307 would resend a client assertion or a DPoP-bound body to another host.
//!
//! In [`Mode::Development`] the client also allows `http` and private addresses (loopback,
//! RFC 1918, link-local, ULA, CGNAT) and resolves every name under `localhost.` to the loopback
//! address itself (RFC 6761 §6.3), so the development network can serve many did:web hosts from
//! one listener. Nothing else changes.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ipnet::{Ipv4Net, Ipv6Net};
use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_LENGTH, LOCATION};

use crate::Mode;

/// The default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The default body cap for requests made through jacquard's [`HttpClient`] trait.
///
/// [`HttpClient`]: jacquard_common::http_client::HttpClient
pub const DEFAULT_BODY_CAP: usize = 1024 * 1024;

/// The most redirects [`FetchClient::get_following`] follows.
pub const MAX_REDIRECTS: usize = 5;

/// How a [`FetchClient`] is built.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Production or development rules.
    pub mode: Mode,
    /// The per-request timeout.
    pub timeout: Duration,
    /// A separate connect timeout (`tt`'s verification uses 3 seconds, design D7).
    pub connect_timeout: Option<Duration>,
    /// The `User-Agent` header.
    pub user_agent: String,
}

impl FetchConfig {
    /// The production defaults.
    pub fn new(mode: Mode) -> Self {
        FetchConfig {
            mode,
            timeout: DEFAULT_TIMEOUT,
            connect_timeout: None,
            user_agent: format!("tartan/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Why a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The URL is not absolute or cannot be parsed.
    #[error("invalid URL")]
    InvalidUrl,
    /// The scheme is not `https` (or `http` in development mode).
    #[error("scheme `{0}` is not allowed")]
    SchemeNotAllowed(String),
    /// The host is an address, or resolves only to addresses, that are not public.
    #[error("host {0} is not a public address")]
    NotPublic(String),
    /// More than [`MAX_REDIRECTS`] redirects, or a redirect without a usable `Location`.
    #[error("too many or malformed redirects")]
    Redirect,
    /// The body is larger than the cap.
    #[error("the response is larger than {0} bytes")]
    TooLarge(usize),
    /// The request timed out.
    #[error("the request to {0} timed out")]
    Timeout(String),
    /// The connection failed.
    #[error("could not connect to {0}")]
    Connect(String),
    /// Any other transport failure.
    #[error("request to {host} failed: {detail}")]
    Transport {
        /// The host of the request.
        host: String,
        /// What failed.
        detail: String,
    },
}

impl FetchError {
    /// True for failures worth retrying later: timeouts, connection and transport failures.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            FetchError::Timeout(_) | FetchError::Connect(_) | FetchError::Transport { .. }
        )
    }
}

/// A response read in full.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The URL that answered (after redirects).
    pub url: Url,
    /// The status.
    pub status: reqwest::StatusCode,
    /// Every response header, `DPoP-Nonce`, `WWW-Authenticate` and `ratelimit-*` included.
    pub headers: reqwest::header::HeaderMap,
    /// The body.
    pub body: Vec<u8>,
}

/// The fetch-safe HTTP client.
#[derive(Clone)]
pub struct FetchClient {
    inner: reqwest::Client,
    policy: Arc<Policy>,
}

impl std::fmt::Debug for FetchClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchClient")
            .field("mode", &self.policy.mode)
            .finish()
    }
}

#[derive(Debug)]
struct Policy {
    mode: Mode,
    /// A test server exempt from the scheme and address checks, so unit tests can exercise
    /// production rules (redirects to private targets) against a local listener.
    #[cfg(test)]
    exempt: Option<SocketAddr>,
}

impl FetchClient {
    /// Builds a client.
    pub fn new(config: FetchConfig) -> Result<Self, FetchError> {
        let mode = config.mode;
        Self::build(
            config,
            Policy {
                mode,
                #[cfg(test)]
                exempt: None,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn with_exempt(config: FetchConfig, exempt: SocketAddr) -> Result<Self, FetchError> {
        Self::build(
            config.clone(),
            Policy {
                mode: config.mode,
                exempt: Some(exempt),
            },
        )
    }

    fn build(config: FetchConfig, policy: Policy) -> Result<Self, FetchError> {
        let policy = Arc::new(policy);
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .user_agent(config.user_agent)
            .dns_resolver(Arc::new(PublicResolver {
                policy: policy.clone(),
            }));
        if let Some(connect) = config.connect_timeout {
            builder = builder.connect_timeout(connect);
        }
        let inner = builder.build().map_err(|error| FetchError::Transport {
            host: String::new(),
            detail: error.to_string(),
        })?;
        Ok(FetchClient { inner, policy })
    }

    /// The mode the client was built for.
    pub fn mode(&self) -> Mode {
        self.policy.mode
    }

    /// Checks that `url` may be fetched: its scheme, and its host when it is an IP literal.
    pub fn check_url(&self, url: &Url) -> Result<(), FetchError> {
        self.policy.check_url(url)
    }

    /// Sends `request` without following redirects and reads at most `body_cap` bytes.
    pub async fn send(
        &self,
        request: http::Request<Vec<u8>>,
        body_cap: usize,
    ) -> Result<Fetched, FetchError> {
        let (parts, body) = request.into_parts();
        let url = Url::parse(&parts.uri.to_string()).map_err(|_| FetchError::InvalidUrl)?;
        self.policy.check_url(&url)?;
        let mut builder = self.inner.request(parts.method, url.clone()).body(body);
        for (name, value) in &parts.headers {
            builder = builder.header(name, value);
        }
        self.execute(builder, url, body_cap).await
    }

    /// `GET`s `url`, following up to [`MAX_REDIRECTS`] redirects (each hop checked like the
    /// first), and reads at most `body_cap` bytes. Only handle resolution and did:web documents
    /// use this.
    pub async fn get_following(&self, url: &str, body_cap: usize) -> Result<Fetched, FetchError> {
        let mut url = Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
        for _ in 0..=MAX_REDIRECTS {
            self.policy.check_url(&url)?;
            let fetched = self
                .execute(self.inner.get(url.clone()), url.clone(), body_cap)
                .await?;
            if !fetched.status.is_redirection() {
                return Ok(fetched);
            }
            let location = fetched
                .headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::Redirect)?;
            url = url.join(location).map_err(|_| FetchError::Redirect)?;
        }
        Err(FetchError::Redirect)
    }

    /// `GET`s `url` without following redirects.
    pub async fn get(&self, url: &str, body_cap: usize) -> Result<Fetched, FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
        self.policy.check_url(&url)?;
        self.execute(self.inner.get(url.clone()), url, body_cap)
            .await
    }

    async fn execute(
        &self,
        builder: reqwest::RequestBuilder,
        url: Url,
        body_cap: usize,
    ) -> Result<Fetched, FetchError> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let mut response = builder
            .send()
            .await
            .map_err(|error| transport_error(&host, &error))?;
        let status = response.status();
        let headers = response.headers().clone();
        let declared = headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok());
        if declared.is_some_and(|length| length > body_cap) {
            return Err(FetchError::TooLarge(body_cap));
        }
        let mut body = Vec::with_capacity(declared.unwrap_or(0).min(body_cap));
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| transport_error(&host, &error))?
        {
            if body.len() + chunk.len() > body_cap {
                return Err(FetchError::TooLarge(body_cap));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Fetched {
            url,
            status,
            headers,
            body,
        })
    }
}

fn transport_error(host: &str, error: &reqwest::Error) -> FetchError {
    if error.is_timeout() {
        return FetchError::Timeout(host.to_owned());
    }
    // A refusal by the resolver surfaces as a connect error whose source chain names it.
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(current) = source {
        if let Some(refused) = current.downcast_ref::<NotPublicError>() {
            return FetchError::NotPublic(refused.0.clone());
        }
        source = current.source();
    }
    if error.is_connect() {
        return FetchError::Connect(host.to_owned());
    }
    FetchError::Transport {
        host: host.to_owned(),
        detail: error.to_string(),
    }
}

impl Policy {
    fn exempt(&self, url: &Url) -> bool {
        #[cfg(test)]
        if let Some(exempt) = self.exempt {
            let host_matches = ip_literal(url) == Some(exempt.ip());
            return host_matches && url.port_or_known_default() == Some(exempt.port());
        }
        let _ = url;
        false
    }

    fn check_url(&self, url: &Url) -> Result<(), FetchError> {
        if self.exempt(url) {
            return Ok(());
        }
        let scheme = url.scheme();
        let scheme_ok = scheme == "https" || (self.mode.is_development() && scheme == "http");
        if !scheme_ok {
            return Err(FetchError::SchemeNotAllowed(scheme.to_owned()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(FetchError::InvalidUrl);
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(FetchError::InvalidUrl);
        }
        match ip_literal(url) {
            Some(ip) => self.check_ip(ip),
            None => Ok(()),
        }
    }

    fn check_ip(&self, ip: IpAddr) -> Result<(), FetchError> {
        if self.allows(ip) {
            Ok(())
        } else {
            Err(FetchError::NotPublic(ip.to_string()))
        }
    }

    fn allows(&self, ip: IpAddr) -> bool {
        is_public(ip) || (self.mode.is_development() && is_private(ip))
    }
}

/// The host of `url` as an IP address, when it is an IP literal. The URL parser has already
/// normalised every IPv4 spelling (`0x7f.1`, `2130706433`) to dotted decimal.
fn ip_literal(url: &Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6.parse::<Ipv6Addr>().ok().map(IpAddr::V6),
        None => host.parse::<Ipv4Addr>().ok().map(IpAddr::V4),
    }
}

/// True when `ip` is a public unicast address. Refused: loopback, RFC 1918, link-local
/// (169.254.0.0/16 and fe80::/10), CGNAT (100.64.0.0/10), ULA (fc00::/7), site-local
/// (fec0::/10), multicast, unspecified, broadcast and the rest of 240.0.0.0/4, the IETF
/// protocol, documentation and benchmarking ranges, discard (100::/64), Teredo, and the NAT64
/// prefixes. IPv4-mapped, IPv4-compatible and 6to4 IPv6 addresses are judged by the IPv4
/// address they embed.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

const V4_REFUSED: &[(&str, &str)] = &[
    ("0.0.0.0/8", "this network, unspecified"),
    ("10.0.0.0/8", "RFC 1918"),
    ("100.64.0.0/10", "CGNAT"),
    ("127.0.0.0/8", "loopback"),
    ("169.254.0.0/16", "link-local"),
    ("172.16.0.0/12", "RFC 1918"),
    ("192.0.0.0/24", "IETF protocol assignments"),
    ("192.0.2.0/24", "documentation"),
    ("192.88.99.0/24", "6to4 relay anycast"),
    ("192.168.0.0/16", "RFC 1918"),
    ("198.18.0.0/15", "benchmarking"),
    ("198.51.100.0/24", "documentation"),
    ("203.0.113.0/24", "documentation"),
    ("224.0.0.0/4", "multicast"),
    ("240.0.0.0/4", "reserved and broadcast"),
];

const V6_REFUSED: &[(&str, &str)] = &[
    ("::/128", "unspecified"),
    ("::1/128", "loopback"),
    ("64:ff9b::/96", "NAT64"),
    ("64:ff9b:1::/48", "local-use NAT64"),
    ("100::/64", "discard"),
    ("2001::/32", "Teredo"),
    ("2001:2::/48", "benchmarking"),
    ("2001:db8::/32", "documentation"),
    ("3fff::/20", "documentation"),
    ("fc00::/7", "unique local"),
    ("fe80::/10", "link-local"),
    ("fec0::/10", "site-local"),
    ("ff00::/8", "multicast"),
];

fn in_any_v4(ip: Ipv4Addr, table: &[(&str, &str)]) -> bool {
    table
        .iter()
        .filter_map(|(net, _)| net.parse::<Ipv4Net>().ok())
        .any(|net| net.contains(&ip))
}

fn in_any_v6(ip: Ipv6Addr, table: &[(&str, &str)]) -> bool {
    table
        .iter()
        .filter_map(|(net, _)| net.parse::<Ipv6Net>().ok())
        .any(|net| net.contains(&ip))
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !in_any_v4(ip, V4_REFUSED)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    !in_any_v6(ip, V6_REFUSED)
}

/// The IPv4 address an IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`) or 6to4
/// (`2002:aabb:ccdd::/48`) address embeds.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let from =
        |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if s[..5] == [0; 5] && s[5] == 0xffff {
        return Some(from(s[6], s[7]));
    }
    if s[..6] == [0; 6] && !(s[6] == 0 && s[7] <= 1) {
        return Some(from(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return Some(from(s[1], s[2]));
    }
    None
}

/// The private ranges development mode also allows: loopback, RFC 1918, CGNAT, link-local and
/// ULA, directly or embedded in an IPv6 address.
fn is_private(ip: IpAddr) -> bool {
    const V4: &[(&str, &str)] = &[
        ("10.0.0.0/8", ""),
        ("100.64.0.0/10", ""),
        ("127.0.0.0/8", ""),
        ("169.254.0.0/16", ""),
        ("172.16.0.0/12", ""),
        ("192.168.0.0/16", ""),
    ];
    const V6: &[(&str, &str)] = &[("::1/128", ""), ("fc00::/7", ""), ("fe80::/10", "")];
    match ip {
        IpAddr::V4(v4) => in_any_v4(v4, V4),
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => in_any_v4(v4, V4),
            None => in_any_v6(v6, V6),
        },
    }
}

#[derive(Debug)]
struct NotPublicError(String);

impl std::fmt::Display for NotPublicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} resolves to no public address", self.0)
    }
}

impl std::error::Error for NotPublicError {}

/// The DNS resolver: the system resolver, keeping only addresses the policy allows. In
/// development mode every name under `localhost.` is the loopback address.
struct PublicResolver {
    policy: Arc<Policy>,
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.policy.clone();
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        Box::pin(async move {
            let local = host == "localhost" || host.ends_with(".localhost");
            let addresses: Vec<SocketAddr> = if local && policy.mode.is_development() {
                vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]
            } else {
                tokio::net::lookup_host((host.as_str(), 0))
                    .await?
                    .filter(|address| policy.allows(address.ip()))
                    .collect()
            };
            if addresses.is_empty() {
                return Err(
                    Box::new(NotPublicError(host)) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

impl jacquard_common::http_client::HttpClient for FetchClient {
    type Error = FetchError;

    async fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        let fetched = self.send(request, DEFAULT_BODY_CAP).await?;
        let mut builder = http::Response::builder().status(fetched.status.as_u16());
        for (name, value) in &fetched.headers {
            builder = builder.header(name, value);
        }
        builder
            .body(fetched.body)
            .map_err(|error| FetchError::Transport {
                host: fetched.url.host_str().unwrap_or_default().to_owned(),
                detail: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Reply, TestServer};

    fn production() -> FetchClient {
        FetchClient::new(FetchConfig::new(Mode::Production)).expect("client")
    }

    #[test]
    fn predicate() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "::ffff:10.0.0.5",
            "::ffff:127.0.0.1",
            "::10.0.0.5",
            "64:ff9b::a00:5",
            "2001:db8::1",
            "2002:a00:5::1",
        ] {
            let ip: IpAddr = ip.parse().expect(ip);
            assert!(!is_public(ip), "{ip} must not be public");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "2606:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            let ip: IpAddr = ip.parse().expect(ip);
            assert!(is_public(ip), "{ip} must be public");
        }
    }

    #[tokio::test]
    async fn refuses_ip_literals_and_plain_http() {
        let client = production();
        for url in [
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://[::ffff:10.0.0.5]/",
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.5/",
            "https://192.168.0.1/",
            "https://[fd00::1]/",
            "https://[fe80::1]/",
        ] {
            assert!(
                matches!(client.get(url, 1024).await, Err(FetchError::NotPublic(_))),
                "{url}"
            );
        }
        assert_eq!(
            client.get("http://127.0.0.1/", 1024).await.err(),
            Some(FetchError::SchemeNotAllowed("http".into()))
        );
        assert_eq!(
            client.get("http://example.com/", 1024).await.err(),
            Some(FetchError::SchemeNotAllowed("http".into()))
        );
    }

    #[tokio::test]
    async fn refuses_names_that_resolve_to_private_addresses() {
        let client = production();
        let error = client.get("https://localhost/", 1024).await.err();
        assert!(matches!(error, Some(FetchError::NotPublic(_))), "{error:?}");
    }

    #[tokio::test]
    async fn refuses_redirects_to_private_targets() {
        for target in [
            "https://10.0.0.5/x",
            "https://[::ffff:10.0.0.5]/x",
            "https://169.254.169.254/latest",
            "https://127.0.0.1/x",
            "https://localhost/x",
            "http://example.com/x",
        ] {
            let server = TestServer::start(vec![Reply::redirect(target)]).await;
            let client = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
                .expect("client");
            let error = client
                .get_following(&server.url("/start"), 1024)
                .await
                .err();
            assert!(
                matches!(
                    error,
                    Some(FetchError::NotPublic(_)) | Some(FetchError::SchemeNotAllowed(_))
                ),
                "{target}: {error:?}"
            );
            assert_eq!(server.requests().len(), 1, "{target}");
        }
    }

    #[tokio::test]
    async fn send_never_follows_redirects() {
        let server = TestServer::start(vec![Reply::redirect("/elsewhere")]).await;
        let client = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
            .expect("client");
        let request = http::Request::post(server.url("/token"))
            .body(b"assertion".to_vec())
            .expect("request");
        let fetched = client.send(request, 1024).await.expect("answer");
        assert_eq!(fetched.status.as_u16(), 307);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].body, b"assertion");
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "user-agent" && value.starts_with("tartan/"))
        );
    }

    #[tokio::test]
    async fn body_cap_while_streaming() {
        let server = TestServer::start(vec![
            Reply::ok(vec![b'a'; 2048]).chunked(),
            Reply::ok(vec![b'a'; 2048]),
        ])
        .await;
        let client = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
            .expect("client");
        for _ in 0..2 {
            assert_eq!(
                client.get(&server.url("/big"), 1024).await.err(),
                Some(FetchError::TooLarge(1024))
            );
        }
    }

    #[tokio::test]
    async fn passes_headers_through_jacquard_trait() {
        use jacquard_common::http_client::HttpClient as _;
        let server = TestServer::start(vec![
            Reply::ok(b"{}".to_vec())
                .status(400)
                .header("DPoP-Nonce", "n-1")
                .header("WWW-Authenticate", "DPoP error=\"use_dpop_nonce\"")
                .header("ratelimit-reset", "1700000000"),
        ])
        .await;
        let client = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
            .expect("client");
        let request = http::Request::post(server.url("/par"))
            .body(Vec::new())
            .expect("request");
        let response = client.send_http(request).await.expect("answer");
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(response.headers()["dpop-nonce"], "n-1");
        assert!(response.headers().contains_key("www-authenticate"));
        assert_eq!(response.headers()["ratelimit-reset"], "1700000000");
    }

    #[tokio::test]
    async fn times_out() {
        let server =
            TestServer::start(vec![Reply::ok(Vec::new()).delay(Duration::from_secs(5))]).await;
        let mut config = FetchConfig::new(Mode::Production);
        config.timeout = Duration::from_millis(300);
        let client = FetchClient::with_exempt(config, server.addr).expect("client");
        assert!(matches!(
            client.get(&server.url("/slow"), 1024).await,
            Err(FetchError::Timeout(_))
        ));
    }

    #[cfg(feature = "dev-identity")]
    #[tokio::test]
    async fn development_allows_http_and_private_addresses() {
        let server = TestServer::start(vec![
            Reply::ok(b"one".to_vec()),
            Reply::ok(b"two".to_vec()),
            Reply::redirect("/three"),
            Reply::ok(b"three".to_vec()),
        ])
        .await;
        let client = FetchClient::new(FetchConfig::new(Mode::Development)).expect("client");
        let first = client
            .get(&server.url("/one"), 1024)
            .await
            .expect("loopback over http");
        assert_eq!(first.body, b"one");
        let named = format!("http://t3.localhost:{}/two", server.addr.port());
        let second = client.get(&named, 1024).await.expect("*.localhost");
        assert_eq!(second.body, b"two");
        let third = client
            .get_following(&server.url("/redirect"), 1024)
            .await
            .expect("redirect within loopback");
        assert_eq!(third.body, b"three");
        // Development mode allows private addresses, nothing else.
        for url in ["ftp://127.0.0.1/", "http://224.0.0.1/", "http://0.0.0.0/"] {
            assert!(client.get(url, 1024).await.is_err(), "{url}");
        }
    }
}
