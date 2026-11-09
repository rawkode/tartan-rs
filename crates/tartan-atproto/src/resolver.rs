//! Resolving DIDs and handles with evidence (design D9).
//!
//! [`VerifyingResolver`] is the identity resolver the forge and `tt` use. A `did:plc` DID is
//! resolved by fetching and replaying its audit log ([`crate::plc`]); a `did:web` DID by
//! fetching `/.well-known/did.json` from its host over TLS ([`crate::document`]). Either way the
//! bytes fetched are kept as evidence for the proof bundle.
//!
//! It also implements jacquard's [`IdentityResolver`], [`HttpClient`], [`OAuthResolver`] and
//! [`DpopExt`] traits, so jacquard-oauth's PAR, token, refresh and revocation calls run over the
//! fetch-safe client and see only verified identities: `resolve_did_doc` answers with a
//! document built from the verified facts, and `resolve_handle` answers only for a handle
//! verified in both directions.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jacquard_common::BosStr;
use jacquard_common::deps::bytes::Bytes;
use jacquard_common::deps::smol_str::SmolStr;
use jacquard_common::http_client::HttpClient;
use jacquard_common::types::did::Did as JDid;
use jacquard_common::types::string::Handle as JHandle;
use jacquard_identity::resolver::{
    DidDocResponse, IdentityError, IdentityErrorKind, IdentityResolver, ResolverOptions,
};

use crate::Mode;
use crate::did::{Did, DidMethod};
use crate::document::{AtprotoIdentity, DocumentError, MAX_DOCUMENT_BYTES, read_document};
use crate::fetch::{FetchClient, FetchError};
use crate::handle::{
    Handle, HandleResolution, HandleSource, HandleVerdict, resolve_handle, verify_handle,
};
use crate::plc::{MAX_LOG_BYTES, PlcError, PlcState, verify_audit_log};

/// The production PLC directory.
pub const PLC_DIRECTORY: &str = "https://plc.directory";

/// What a DID resolution rests on, kept for the proof bundle (design D8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// The PLC audit log as fetched and the CID of the operation the document came from.
    Plc {
        /// The bytes of `GET {plc}/{did}/log/audit`.
        audit_log: Vec<u8>,
        /// The CID of the last accepted operation.
        operation_cid: String,
    },
    /// The did:web document as fetched: a record of what TLS served, not an offline proof.
    Web {
        /// The URL fetched.
        url: String,
        /// The bytes served.
        document: Vec<u8>,
    },
}

/// A resolved DID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The verified facts, or `None` when the DID is tombstoned (account status `deleted`).
    pub identity: Option<AtprotoIdentity>,
    /// What the facts rest on.
    pub evidence: Evidence,
    /// When the evidence was fetched (Unix milliseconds).
    pub fetched_at_ms: u64,
}

/// Why a DID could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// The directory or host could not be reached, timed out or answered 5xx or 429.
    #[error("{0}")]
    Unavailable(String),
    /// The DID is unknown to its directory or host (404).
    #[error("{0} was not found")]
    NotFound(String),
    /// The answer is not acceptable evidence (a forged log, a mismatched document).
    #[error("{0}")]
    Invalid(String),
}

impl ResolveError {
    /// True for failures worth retrying later.
    pub fn is_transient(&self) -> bool {
        matches!(self, ResolveError::Unavailable(_))
    }
}

/// How a [`VerifyingResolver`] reaches the network.
#[derive(Clone)]
pub struct ResolverConfig {
    /// The PLC directory's base URL.
    pub plc_url: String,
    /// How handles are resolved.
    pub handles: HandleSource,
}

impl ResolverConfig {
    /// The production configuration: `plc.directory` and the system DNS resolver.
    pub fn production() -> Result<Self, crate::handle::TxtError> {
        Ok(ResolverConfig {
            plc_url: PLC_DIRECTORY.to_owned(),
            handles: HandleSource::Public {
                txt: Arc::new(crate::handle::SystemTxtResolver::new()?),
            },
        })
    }

    /// The development configuration (`TARTAN_PLC_URL` and `TARTAN_DEV_HANDLE_RESOLVER`).
    #[cfg(feature = "dev-identity")]
    pub fn development(plc_url: &str, handle_resolver: &str) -> Self {
        ResolverConfig {
            plc_url: plc_url.trim_end_matches('/').to_owned(),
            handles: HandleSource::Development {
                base: handle_resolver.trim_end_matches('/').to_owned(),
            },
        }
    }
}

/// The verifying identity resolver.
#[derive(Clone)]
pub struct VerifyingResolver {
    fetch: FetchClient,
    config: Arc<ResolverConfig>,
    options: ResolverOptions,
}

impl VerifyingResolver {
    /// Builds a resolver over `fetch`.
    pub fn new(fetch: FetchClient, config: ResolverConfig) -> Self {
        let options = ResolverOptions::default();
        VerifyingResolver {
            fetch,
            config: Arc::new(config),
            options,
        }
    }

    /// The fetch-safe client.
    pub fn fetch(&self) -> &FetchClient {
        &self.fetch
    }

    /// The mode.
    pub fn mode(&self) -> Mode {
        self.fetch.mode()
    }

    /// Resolves `did`, verifying the evidence.
    pub async fn resolve_did(&self, did: &Did) -> Result<Resolved, ResolveError> {
        match did.method() {
            DidMethod::Plc => self.resolve_plc(did).await,
            DidMethod::Web => self.resolve_web(did).await,
        }
    }

    async fn resolve_plc(&self, did: &Did) -> Result<Resolved, ResolveError> {
        let url = format!("{}/{did}/log/audit", self.config.plc_url);
        let fetched = self
            .fetch
            .get(&url, MAX_LOG_BYTES)
            .await
            .map_err(|error| fetch_failure(&error))?;
        check_status(fetched.status, did)?;
        let evidence = verify_audit_log(did, &fetched.body, self.mode())
            .map_err(|error: PlcError| ResolveError::Invalid(format!("{did}: {error}")))?;
        let identity = match evidence.state {
            PlcState::Active(identity) => Some(identity),
            PlcState::Tombstoned => None,
        };
        Ok(Resolved {
            identity,
            evidence: Evidence::Plc {
                audit_log: fetched.body,
                operation_cid: evidence.operation_cid,
            },
            fetched_at_ms: now_ms(),
        })
    }

    async fn resolve_web(&self, did: &Did) -> Result<Resolved, ResolveError> {
        let url = did_web_url(did, self.mode())
            .ok_or_else(|| ResolveError::Invalid(format!("{did} is not a resolvable did:web")))?;
        let fetched = self
            .fetch
            .get_following(&url, MAX_DOCUMENT_BYTES)
            .await
            .map_err(|error| fetch_failure(&error))?;
        check_status(fetched.status, did)?;
        let identity = read_document(did, &fetched.body, self.mode())
            .map_err(|error: DocumentError| ResolveError::Invalid(format!("{did}: {error}")))?;
        Ok(Resolved {
            identity: Some(identity),
            evidence: Evidence::Web {
                url,
                document: fetched.body,
            },
            fetched_at_ms: now_ms(),
        })
    }

    /// Resolves `handle` to a DID (one direction).
    pub async fn resolve_handle(&self, handle: &Handle) -> HandleResolution {
        resolve_handle(&self.fetch, &self.config.handles, handle).await
    }

    /// Checks the handle a resolved identity claims, in both directions.
    pub async fn verify_handle(&self, identity: &AtprotoIdentity) -> HandleVerdict {
        verify_handle(
            &self.fetch,
            &self.config.handles,
            &identity.did,
            identity.handle.as_deref(),
        )
        .await
    }

    /// Resolves a handle to a DID and requires the DID's document to claim the handle back.
    pub async fn resolve_verified_handle(&self, handle: &Handle) -> Result<Did, ResolveError> {
        let did = match self.resolve_handle(handle).await {
            HandleResolution::Resolved(did) => did,
            HandleResolution::NotFound(reason) => return Err(ResolveError::NotFound(reason)),
            HandleResolution::Unavailable(reason) => return Err(ResolveError::Unavailable(reason)),
        };
        let resolved = self.resolve_did(&did).await?;
        let claims = resolved
            .identity
            .as_ref()
            .and_then(|identity| identity.handle.as_deref())
            == Some(handle.as_str());
        if claims {
            Ok(did)
        } else {
            Err(ResolveError::Invalid(format!(
                "{did} does not claim {handle}"
            )))
        }
    }
}

/// The URL a did:web document is fetched from: `https://<host>/.well-known/did.json`, or
/// `http://<host>:<port>/…` for a `localhost` host in development mode.
pub fn did_web_url(did: &Did, mode: Mode) -> Option<String> {
    let (host, port) = did.web_host()?;
    let local = host == "localhost" || host.ends_with(".localhost");
    match port {
        Some(port) if local && mode.is_development() => {
            Some(format!("http://{host}:{port}/.well-known/did.json"))
        }
        Some(_) => None,
        None if local && mode.is_development() => {
            Some(format!("http://{host}/.well-known/did.json"))
        }
        None => Some(format!("https://{host}/.well-known/did.json")),
    }
}

fn check_status(status: reqwest::StatusCode, did: &Did) -> Result<(), ResolveError> {
    if status.is_success() {
        Ok(())
    } else if status.as_u16() == 404 || status.as_u16() == 410 {
        Err(ResolveError::NotFound(did.to_string()))
    } else if status.is_server_error() || status.as_u16() == 429 {
        Err(ResolveError::Unavailable(format!(
            "resolving {did}: HTTP {status}"
        )))
    } else {
        Err(ResolveError::Invalid(format!(
            "resolving {did}: HTTP {status}"
        )))
    }
}

fn fetch_failure(error: &FetchError) -> ResolveError {
    if error.is_transient() {
        ResolveError::Unavailable(error.to_string())
    } else {
        ResolveError::Invalid(error.to_string())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// A W3C DID document holding only the verified facts, for jacquard.
fn document_json(identity: &AtprotoIdentity) -> serde_json::Value {
    let did = identity.did.as_str();
    let mut document = serde_json::json!({
        "@context": ["https://www.w3.org/ns/did/v1", "https://w3id.org/security/multikey/v1"],
        "id": did,
    });
    if let Some(handle) = &identity.handle {
        document["alsoKnownAs"] = serde_json::json!([format!("at://{handle}")]);
    }
    if let Some(key) = &identity.signing_key {
        document["verificationMethod"] = serde_json::json!([{
            "id": format!("{did}#atproto"),
            "type": "Multikey",
            "controller": did,
            "publicKeyMultibase": key.multikey(),
        }]);
    }
    if let Some(pds) = &identity.pds {
        document["service"] = serde_json::json!([{
            "id": "#atproto_pds",
            "type": "AtprotoPersonalDataServer",
            "serviceEndpoint": pds,
        }]);
    }
    document
}

fn identity_error(error: ResolveError) -> IdentityError {
    match error {
        ResolveError::Unavailable(reason) => {
            IdentityError::new(IdentityErrorKind::Transport(SmolStr::from(reason)), None)
        }
        ResolveError::NotFound(reason) | ResolveError::Invalid(reason) => {
            IdentityError::new(IdentityErrorKind::InvalidDoc(SmolStr::from(reason)), None)
        }
    }
}

impl HttpClient for VerifyingResolver {
    type Error = FetchError;

    async fn send_http(
        &self,
        request: http::Request<Vec<u8>>,
    ) -> Result<http::Response<Vec<u8>>, FetchError> {
        self.fetch.send_http(request).await
    }
}

impl IdentityResolver for VerifyingResolver {
    fn options(&self) -> &ResolverOptions {
        &self.options
    }

    async fn resolve_handle<S: BosStr + Sync>(
        &self,
        handle: &JHandle<S>,
    ) -> Result<JDid, IdentityError> {
        let handle = Handle::parse(handle.as_str(), self.mode())
            .map_err(|_| IdentityError::new(IdentityErrorKind::HandleResolutionExhausted, None))?;
        let did = self
            .resolve_verified_handle(&handle)
            .await
            .map_err(identity_error)?;
        JDid::new_owned(did.as_str())
            .map_err(|_| IdentityError::new(IdentityErrorKind::HandleResolutionExhausted, None))
    }

    async fn resolve_did_doc<S: BosStr + Sync>(
        &self,
        did: &JDid<S>,
    ) -> Result<DidDocResponse, IdentityError> {
        let parsed = Did::parse(did.as_str(), self.mode())
            .map_err(|_| IdentityError::unsupported_did_method(did.as_str()))?;
        let resolved = self.resolve_did(&parsed).await.map_err(identity_error)?;
        let identity = resolved.identity.ok_or_else(|| {
            IdentityError::new(
                IdentityErrorKind::InvalidDoc(SmolStr::from(format!("{parsed} is tombstoned"))),
                None,
            )
        })?;
        let body = serde_json::to_vec(&document_json(&identity)).unwrap_or_default();
        Ok(DidDocResponse {
            buffer: Bytes::from(body),
            status: reqwest::StatusCode::OK,
            requested: JDid::new_owned(parsed.as_str()).ok(),
        })
    }
}

impl jacquard_oauth::resolver::OAuthResolver for VerifyingResolver {}
impl jacquard_oauth::dpop::DpopExt for VerifyingResolver {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FetchConfig;
    use crate::handle::tests::FakeTxt;
    use crate::testutil::{Reply, TestServer};

    fn fixture(path: &str) -> Vec<u8> {
        let path = format!("{}/testdata/{path}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).expect(&path)
    }

    const PAUL: &str = "did:plc:ragtjsm2j2vknwkz3zp4oxrd";

    async fn plc_resolver(reply: Reply) -> (VerifyingResolver, TestServer) {
        let server = TestServer::with_handler(move |_| reply.clone()).await;
        let fetch = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
            .expect("client");
        let config = ResolverConfig {
            plc_url: server.url(""),
            handles: HandleSource::PublicAt {
                txt: FakeTxt::new(&[]),
                base: server.url(""),
            },
        };
        (VerifyingResolver::new(fetch, config), server)
    }

    #[tokio::test]
    async fn resolves_plc_with_evidence() {
        let log = fixture("plc/legacy-create-and-rotation.json");
        let (resolver, server) = plc_resolver(Reply::ok(log.clone())).await;
        let did = Did::parse(PAUL, Mode::Production).expect("did");
        let resolved = resolver.resolve_did(&did).await.expect("resolved");
        assert!(resolved.identity.is_some());
        match resolved.evidence {
            Evidence::Plc { audit_log, .. } => assert_eq!(audit_log, log),
            other => panic!("{other:?}"),
        }
        assert_eq!(server.requests()[0].path, format!("/{PAUL}/log/audit"));
    }

    #[tokio::test]
    async fn plc_failures_are_classified() {
        let did = Did::parse(PAUL, Mode::Production).expect("did");
        let (resolver, _server) = plc_resolver(Reply::ok(Vec::new()).status(404)).await;
        assert!(matches!(
            resolver.resolve_did(&did).await,
            Err(ResolveError::NotFound(_))
        ));
        let (resolver, _server) = plc_resolver(Reply::ok(Vec::new()).status(503)).await;
        let error = resolver.resolve_did(&did).await.expect_err("unavailable");
        assert!(error.is_transient());
        let forged = fixture("plc/atproto-com.json");
        let (resolver, _server) = plc_resolver(Reply::ok(forged)).await;
        assert!(matches!(
            resolver.resolve_did(&did).await,
            Err(ResolveError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn jacquard_sees_only_verified_documents() {
        let log = fixture("plc/legacy-create-and-rotation.json");
        let (resolver, _server) = plc_resolver(Reply::ok(log)).await;
        let did: JDid = JDid::new_owned(PAUL).expect("did");
        let response = IdentityResolver::resolve_did_doc(&resolver, &did)
            .await
            .expect("document");
        let document = response.into_owned().expect("parsed");
        assert_eq!(document.id.as_str(), PAUL);
        assert!(document.pds_endpoint().is_some());

        let unsupported: JDid =
            JDid::new_owned("did:key:zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ")
                .expect("did");
        assert!(
            IdentityResolver::resolve_did_doc(&resolver, &unsupported)
                .await
                .is_err()
        );
    }

    /// jacquard-oauth's `par`, `exchange_code`, `refresh` and `revoke` take any resolver with
    /// these bounds, so they run over the fetch-safe client and the verified identities.
    #[test]
    fn usable_by_jacquard_oauth() {
        fn bounds<
            T: jacquard_oauth::resolver::OAuthResolver
                + jacquard_oauth::dpop::DpopExt
                + Send
                + Sync
                + 'static,
        >() {
        }
        bounds::<VerifyingResolver>();
    }

    #[test]
    fn did_web_urls() {
        let did = Did::parse("did:web:rawkode.dev", Mode::Production).expect("did");
        assert_eq!(
            did_web_url(&did, Mode::Production).as_deref(),
            Some("https://rawkode.dev/.well-known/did.json")
        );
    }

    #[cfg(feature = "dev-identity")]
    #[tokio::test]
    async fn resolves_localhost_did_web_in_development() {
        let document = serde_json::json!({
            "id": "PLACEHOLDER",
            "alsoKnownAs": ["at://alice.test"],
            "verificationMethod": [{
                "id": "#atproto",
                "type": "Multikey",
                "publicKeyMultibase": "zQ3shpG8vFZ6MmTHNVv7PHmRrw97oArf4QnYJh88zFnNpuYzJ"
            }],
            "service": [{
                "id": "#atproto_pds",
                "type": "AtprotoPersonalDataServer",
                "serviceEndpoint": "http://127.0.0.1:1"
            }]
        });
        let holder = std::sync::Arc::new(std::sync::Mutex::new(document));
        let served = holder.clone();
        let server = TestServer::with_handler(move |_| {
            Reply::json(served.lock().expect("document").clone())
        })
        .await;
        let did_text = format!("did:web:t1.localhost%3A{}", server.addr.port());
        holder.lock().expect("document")["id"] = serde_json::json!(did_text);
        let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("client");
        let resolver = VerifyingResolver::new(
            fetch,
            ResolverConfig::development("http://127.0.0.1:1", "http://127.0.0.1:1"),
        );
        let did = Did::parse(&did_text, Mode::Development).expect("did");
        let resolved = resolver.resolve_did(&did).await.expect("resolved");
        let identity = resolved.identity.expect("identity");
        assert_eq!(identity.pds.as_deref(), Some("http://127.0.0.1:1"));
        assert_eq!(identity.handle.as_deref(), Some("alice.test"));
        assert_eq!(server.requests()[0].path, "/.well-known/did.json");
    }
}
