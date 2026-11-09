//! Handles (design D9, *Handles*).
//!
//! A handle is shown only once it is verified in both directions: the handle resolves to the
//! DID, and the DID's document claims the handle as its first `at://` `alsoKnownAs` entry
//! (compared lowercased).
//!
//! Resolution runs the DNS TXT lookup of `_atproto.<handle>` and the HTTPS lookup of
//! `https://<handle>/.well-known/atproto-did` at the same time and prefers DNS. TXT records that
//! name more than one DID are a conflict and resolve nothing. TXT lookups use the system
//! nameservers through a [`TxtResolver`], never the HTTP client; in development mode handles
//! resolve only through the development network's `com.atproto.identity.resolveHandle`, with no
//! DNS query at all.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::Mode;
use crate::did::{Did, is_hostname};
use crate::fetch::{FetchClient, FetchError};

/// The largest `/.well-known/atproto-did` or `resolveHandle` body read.
const MAX_ANSWER_BYTES: usize = 2048;

/// Top-level labels that never name a resolvable handle. `.test` is allowed in development
/// mode, where the development network's people live.
const RESERVED_TLDS: &[&str] = &[
    "alt",
    "arpa",
    "example",
    "internal",
    "invalid",
    "local",
    "localhost",
    "onion",
    "test",
];

/// A syntactically valid handle, lowercased.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(String);

/// Why a string is not a handle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid handle")]
pub struct HandleSyntaxError;

impl Handle {
    /// Parses and lowercases `value`.
    pub fn parse(value: &str, mode: Mode) -> Result<Self, HandleSyntaxError> {
        let lower = value.to_ascii_lowercase();
        let tld = lower.rsplit('.').next().unwrap_or_default();
        let reserved = RESERVED_TLDS.contains(&tld) && !(mode.is_development() && tld == "test");
        if reserved || !is_hostname(&lower, Mode::Production) {
            return Err(HandleSyntaxError);
        }
        Ok(Handle(lower))
    }

    /// The handle.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A TXT lookup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TxtError {
    /// The name has no TXT records (or does not exist).
    #[error("no TXT records")]
    NoRecords,
    /// The lookup failed (timeout, unreachable nameserver, SERVFAIL).
    #[error("DNS lookup failed: {0}")]
    Failed(String),
}

/// A boxed future, so resolvers can be swapped in tests.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Looks up TXT records.
pub trait TxtResolver: Send + Sync {
    /// The TXT strings at `name` (each record's character strings concatenated).
    fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, TxtError>>;
}

/// The system resolver (hickory, reading `/etc/resolv.conf` or the platform equivalent).
pub struct SystemTxtResolver {
    resolver: hickory_resolver::TokioResolver,
}

impl SystemTxtResolver {
    /// Builds a resolver from the system configuration.
    pub fn new() -> Result<Self, TxtError> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .and_then(|builder| builder.build())
            .map_err(|error| TxtError::Failed(error.to_string()))?;
        Ok(SystemTxtResolver { resolver })
    }
}

impl TxtResolver for SystemTxtResolver {
    fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, TxtError>> {
        Box::pin(async move {
            let lookup = self.resolver.txt_lookup(name).await.map_err(|error| {
                if error.is_no_records_found() {
                    TxtError::NoRecords
                } else {
                    TxtError::Failed(error.to_string())
                }
            })?;
            let mut values = Vec::new();
            for record in lookup.answers() {
                if let hickory_resolver::proto::rr::RData::TXT(txt) = &record.data {
                    let joined: Vec<u8> = txt
                        .txt_data
                        .iter()
                        .flat_map(|part| part.iter().copied())
                        .collect();
                    values.push(String::from_utf8_lossy(&joined).into_owned());
                }
            }
            Ok(values)
        })
    }
}

/// What resolving a handle found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleResolution {
    /// The handle names this DID.
    Resolved(Did),
    /// The handle affirmatively names no DID: no TXT record and no well-known answer, or
    /// conflicting TXT records.
    NotFound(String),
    /// Resolution could not be completed now (DNS or network failure); try again later.
    Unavailable(String),
}

/// How handles are resolved.
#[derive(Clone)]
pub enum HandleSource {
    /// DNS TXT and HTTPS well-known.
    Public {
        /// The TXT resolver.
        txt: Arc<dyn TxtResolver>,
    },
    /// The development network's `com.atproto.identity.resolveHandle` at this base URL
    /// (`TARTAN_DEV_HANDLE_RESOLVER`), and nothing else.
    #[cfg(feature = "dev-identity")]
    Development {
        /// The resolver's base URL.
        base: String,
    },
    /// DNS TXT from `txt`, and the well-known document from `base` instead of the handle's
    /// host, so tests need no network.
    #[cfg(test)]
    PublicAt {
        /// The TXT resolver.
        txt: Arc<dyn TxtResolver>,
        /// The base URL standing in for `https://<handle>`.
        base: String,
    },
}

/// Resolves `handle` to a DID (one direction only; see [`verify_handle`]).
pub async fn resolve_handle(
    fetch: &FetchClient,
    source: &HandleSource,
    handle: &Handle,
) -> HandleResolution {
    match source {
        HandleSource::Public { txt } => {
            let well_known = format!("https://{handle}/.well-known/atproto-did");
            resolve_public(fetch, txt.as_ref(), handle, &well_known).await
        }
        #[cfg(feature = "dev-identity")]
        HandleSource::Development { base } => resolve_development(fetch, base, handle).await,
        #[cfg(test)]
        HandleSource::PublicAt { txt, base } => {
            let well_known = format!("{base}/.well-known/atproto-did");
            resolve_public(fetch, txt.as_ref(), handle, &well_known).await
        }
    }
}

async fn resolve_public(
    fetch: &FetchClient,
    txt: &dyn TxtResolver,
    handle: &Handle,
    well_known: &str,
) -> HandleResolution {
    let mode = fetch.mode();
    let dns_name = format!("_atproto.{handle}.");
    let (dns, https) = tokio::join!(
        txt.txt(&dns_name),
        fetch.get_following(well_known, MAX_ANSWER_BYTES)
    );

    let dns_failure = match dns {
        Ok(values) => {
            let mut dids: Vec<Did> = values
                .iter()
                .filter_map(|value| value.strip_prefix("did="))
                .filter_map(|value| Did::parse(value.trim(), mode).ok())
                .collect();
            dids.sort();
            dids.dedup();
            match dids.len() {
                0 => None,
                1 => return HandleResolution::Resolved(dids.remove(0)),
                _ => {
                    return HandleResolution::NotFound(format!(
                        "conflicting TXT records for _atproto.{handle}"
                    ));
                }
            }
        }
        Err(TxtError::NoRecords) => None,
        Err(TxtError::Failed(reason)) => Some(reason),
    };

    // `Err(true)`: the host could not be reached; `Err(false)`: it answered without a DID.
    let https_outcome: Result<Did, bool> = match https {
        Ok(fetched) if fetched.status.is_success() => {
            let text = String::from_utf8_lossy(&fetched.body);
            Did::parse(text.trim(), mode).map_err(|_| false)
        }
        Ok(fetched) => Err(fetched.status.is_server_error()),
        Err(error) => Err(error.is_transient() || matches!(error, FetchError::NotPublic(_))),
    };
    match (dns_failure, https_outcome) {
        (_, Ok(did)) => HandleResolution::Resolved(did),
        (Some(reason), Err(_)) => HandleResolution::Unavailable(reason),
        (None, Err(true)) => {
            HandleResolution::Unavailable(format!("https://{handle} could not be reached"))
        }
        (None, Err(false)) => HandleResolution::NotFound(format!("{handle} names no DID")),
    }
}

#[cfg(feature = "dev-identity")]
async fn resolve_development(fetch: &FetchClient, base: &str, handle: &Handle) -> HandleResolution {
    let url = format!(
        "{}/xrpc/com.atproto.identity.resolveHandle?handle={handle}",
        base.trim_end_matches('/')
    );
    match fetch.get(&url, MAX_ANSWER_BYTES).await {
        Ok(fetched) if fetched.status.is_success() => {
            let did = serde_json::from_slice::<serde_json::Value>(&fetched.body)
                .ok()
                .and_then(|value| value.get("did")?.as_str().map(str::to_owned))
                .and_then(|did| Did::parse(&did, fetch.mode()).ok());
            match did {
                Some(did) => HandleResolution::Resolved(did),
                None => HandleResolution::NotFound(format!("{handle}: malformed answer")),
            }
        }
        Ok(fetched) if fetched.status.as_u16() == 400 => {
            HandleResolution::NotFound(format!("{handle} names no DID"))
        }
        Ok(fetched) => {
            HandleResolution::Unavailable(format!("resolveHandle answered {}", fetched.status))
        }
        Err(error) => HandleResolution::Unavailable(error.to_string()),
    }
}

/// The result of checking a DID's claimed handle in both directions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleVerdict {
    /// The handle resolves to the DID and the DID claims it.
    Verified(Handle),
    /// An affirmative mismatch: the DID claims no valid handle, or the handle names another DID
    /// or none. Shown as `handle.invalid`.
    Invalid(String),
    /// The check could not be completed now; keep the last verified handle.
    Unavailable(String),
}

/// Checks the handle `claimed` (the DID document's first `at://` `alsoKnownAs` entry) against
/// `did`.
pub async fn verify_handle(
    fetch: &FetchClient,
    source: &HandleSource,
    did: &Did,
    claimed: Option<&str>,
) -> HandleVerdict {
    let Some(claimed) = claimed else {
        return HandleVerdict::Invalid(format!("{did} claims no handle"));
    };
    let Ok(handle) = Handle::parse(claimed, fetch.mode()) else {
        return HandleVerdict::Invalid(format!("{did} claims an invalid handle"));
    };
    match resolve_handle(fetch, source, &handle).await {
        HandleResolution::Resolved(resolved) if &resolved == did => HandleVerdict::Verified(handle),
        HandleResolution::Resolved(other) => {
            HandleVerdict::Invalid(format!("{handle} resolves to {other}, not {did}"))
        }
        HandleResolution::NotFound(reason) => HandleVerdict::Invalid(reason),
        HandleResolution::Unavailable(reason) => HandleVerdict::Unavailable(reason),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fetch::FetchConfig;
    use crate::testutil::{Reply, TestServer};
    use std::collections::HashMap;

    /// A TXT resolver answering from a table.
    pub(crate) struct FakeTxt(pub HashMap<String, Result<Vec<String>, TxtError>>);

    impl FakeTxt {
        pub(crate) fn new(entries: &[(&str, Result<Vec<String>, TxtError>)]) -> Arc<Self> {
            Arc::new(FakeTxt(
                entries
                    .iter()
                    .map(|(name, answer)| ((*name).to_owned(), answer.clone()))
                    .collect(),
            ))
        }
    }

    impl TxtResolver for FakeTxt {
        fn txt<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Vec<String>, TxtError>> {
            let answer = self
                .0
                .get(name)
                .cloned()
                .unwrap_or(Err(TxtError::NoRecords));
            Box::pin(async move { answer })
        }
    }

    const ALICE: &str = "did:plc:ewvi7nxzyoun6zhxrhs64oiz";
    const BOB: &str = "did:plc:ragtjsm2j2vknwkz3zp4oxrd";

    fn did(value: &str) -> Did {
        Did::parse(value, Mode::Production).expect("did")
    }

    fn handle(value: &str) -> Handle {
        Handle::parse(value, Mode::Production).expect("handle")
    }

    /// A fake DNS table and a local HTTP server standing in for every handle's host.
    async fn setup(
        dns: &[(&str, Result<Vec<String>, TxtError>)],
        well_known: Reply,
    ) -> (FetchClient, HandleSource, TestServer) {
        let reply = well_known.clone();
        let server = TestServer::with_handler(move |_| reply.clone()).await;
        let fetch = FetchClient::with_exempt(FetchConfig::new(Mode::Production), server.addr)
            .expect("client");
        let source = HandleSource::PublicAt {
            txt: FakeTxt::new(dns),
            base: server.url(""),
        };
        (fetch, source, server)
    }

    fn not_found() -> Reply {
        Reply::ok(Vec::new()).status(404)
    }

    #[test]
    fn syntax() {
        for ok in [
            "alice.example.com",
            "Alice.Example.COM",
            "a.co",
            "xn--ls8h.la",
        ] {
            assert!(Handle::parse(ok, Mode::Production).is_ok(), "{ok}");
        }
        assert_eq!(handle("Alice.Example.COM").as_str(), "alice.example.com");
        for bad in [
            "alice",
            "localhost",
            "alice.local",
            "bob.test",
            "x.onion",
            "a.example",
            "alice.123",
            "-a.example.com",
            "a_b.example.com",
            "did:plc:abc",
            "",
        ] {
            assert!(Handle::parse(bad, Mode::Production).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn dns_is_preferred_over_https() {
        let (fetch, source, _server) = setup(
            &[(
                "_atproto.alice.example.com.",
                Ok(vec![format!("did={ALICE}")]),
            )],
            Reply::ok(BOB.as_bytes().to_vec()),
        )
        .await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert_eq!(resolution, HandleResolution::Resolved(did(ALICE)));
    }

    #[tokio::test]
    async fn https_answers_without_dns() {
        let (fetch, source, server) =
            setup(&[], Reply::ok(format!("{ALICE}\n").into_bytes())).await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert_eq!(resolution, HandleResolution::Resolved(did(ALICE)));
        assert_eq!(server.requests()[0].path, "/.well-known/atproto-did");
    }

    #[tokio::test]
    async fn conflicting_txt_records_fail() {
        let (fetch, source, _server) = setup(
            &[(
                "_atproto.alice.example.com.",
                Ok(vec![format!("did={ALICE}"), format!("did={BOB}")]),
            )],
            not_found(),
        )
        .await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert!(
            matches!(resolution, HandleResolution::NotFound(_)),
            "{resolution:?}"
        );
    }

    #[tokio::test]
    async fn duplicate_txt_records_of_one_did_resolve() {
        let (fetch, source, _server) = setup(
            &[(
                "_atproto.alice.example.com.",
                Ok(vec![
                    format!("did={ALICE}"),
                    format!("did={ALICE}"),
                    "v=spf1".into(),
                ]),
            )],
            not_found(),
        )
        .await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert_eq!(resolution, HandleResolution::Resolved(did(ALICE)));
    }

    #[tokio::test]
    async fn nothing_found() {
        let (fetch, source, _server) = setup(&[], not_found()).await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert!(
            matches!(resolution, HandleResolution::NotFound(_)),
            "{resolution:?}"
        );
    }

    #[tokio::test]
    async fn failures_are_unavailable_not_invalid() {
        let (fetch, source, _server) = setup(
            &[(
                "_atproto.alice.example.com.",
                Err(TxtError::Failed("SERVFAIL".into())),
            )],
            not_found(),
        )
        .await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert!(
            matches!(resolution, HandleResolution::Unavailable(_)),
            "{resolution:?}"
        );

        let (fetch, source, _server) = setup(&[], not_found().status(503)).await;
        let resolution = resolve_handle(&fetch, &source, &handle("alice.example.com")).await;
        assert!(
            matches!(resolution, HandleResolution::Unavailable(_)),
            "{resolution:?}"
        );
        assert!(matches!(
            verify_handle(&fetch, &source, &did(ALICE), Some("alice.example.com")).await,
            HandleVerdict::Unavailable(_)
        ));
    }

    #[tokio::test]
    async fn verification_needs_both_directions() {
        let (fetch, source, _server) = setup(
            &[
                (
                    "_atproto.alice.example.com.",
                    Ok(vec![format!("did={ALICE}")]),
                ),
                (
                    "_atproto.mallory.example.com.",
                    Ok(vec![format!("did={BOB}")]),
                ),
            ],
            not_found(),
        )
        .await;
        let alice = did(ALICE);
        assert_eq!(
            verify_handle(&fetch, &source, &alice, Some("Alice.Example.com")).await,
            HandleVerdict::Verified(handle("alice.example.com"))
        );
        // The document claims a handle that names someone else.
        assert!(matches!(
            verify_handle(&fetch, &source, &alice, Some("mallory.example.com")).await,
            HandleVerdict::Invalid(_)
        ));
        // A claimed handle that resolves to nothing.
        assert!(matches!(
            verify_handle(&fetch, &source, &alice, Some("nobody.example.com")).await,
            HandleVerdict::Invalid(_)
        ));
        assert!(matches!(
            verify_handle(&fetch, &source, &alice, None).await,
            HandleVerdict::Invalid(_)
        ));
        assert!(matches!(
            verify_handle(&fetch, &source, &alice, Some("not a handle")).await,
            HandleVerdict::Invalid(_)
        ));
    }

    #[cfg(feature = "dev-identity")]
    #[tokio::test]
    async fn development_resolver() {
        let server = TestServer::with_handler(|request| {
            if request.path.ends_with("handle=alice.test") {
                Reply::json(serde_json::json!({"did": ALICE}))
            } else {
                Reply::json(serde_json::json!({"error": "InvalidRequest"})).status(400)
            }
        })
        .await;
        let fetch = FetchClient::new(FetchConfig::new(Mode::Development)).expect("client");
        let source = HandleSource::Development {
            base: server.url(""),
        };
        let alice = Handle::parse("alice.test", Mode::Development).expect("dev handle");
        assert_eq!(
            resolve_handle(&fetch, &source, &alice).await,
            HandleResolution::Resolved(did(ALICE))
        );
        let nobody = Handle::parse("nobody.test", Mode::Development).expect("dev handle");
        assert!(matches!(
            resolve_handle(&fetch, &source, &nobody).await,
            HandleResolution::NotFound(_)
        ));
        assert_eq!(
            verify_handle(&fetch, &source, &did(ALICE), Some("alice.test")).await,
            HandleVerdict::Verified(alice)
        );
        assert!(
            server
                .requests()
                .iter()
                .all(|r| r.path.starts_with("/xrpc/"))
        );
    }
}
