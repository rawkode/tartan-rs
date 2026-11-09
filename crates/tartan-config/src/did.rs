//! The DIDs a configuration may name: the forge's `owners` and `readers`, and the signers of a
//! repository's signing rule.
//!
//! The syntax is that of `#DID` in `tartan.rawkode.academy/types@v0.2.0`, so a value the CUE
//! type accepts is accepted here and the other way round:
//!
//! - `did:plc:` followed by 24 base32 characters (`a-z`, `2-7`);
//! - `did:web:` followed by a lowercase hostname, with no path. ATProto accepts only
//!   hostname-level did:web, so `did:web:example.com:user` is refused. Only `localhost` may carry
//!   a port, written `%3A` as did:web encodes it (`did:web:localhost%3A9400`), because ATProto
//!   allows a port only for local testing.
//!
//! A syntactically valid DID says nothing about whether it resolves: resolution and verification
//! happen in the forge. Whether a `localhost` DID is acceptable is the caller's decision too
//! ([`Did::is_localhost`]): the forge refuses one unless it runs with its development identity
//! network.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// The longest hostname a did:web DID may name, in characters.
const MAX_HOSTNAME: usize = 253;

static DID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^did:(plc:[a-z2-7]{24}",
        r"|web:(localhost(%3A[1-9][0-9]{0,4})?",
        r"|([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]([a-z0-9-]{0,61}[a-z0-9])?))$",
    ))
    .expect("valid regex")
});

/// A `did:plc` or hostname-only `did:web` DID, checked for syntax only.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Did(String);

impl Did {
    /// The DID as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is a `did:web` DID of `localhost`, with or without a port: usable only with
    /// the development identity network.
    pub fn is_localhost(&self) -> bool {
        self.0
            .strip_prefix("did:web:localhost")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("%3A"))
    }
}

impl std::str::FromStr for Did {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hostname_too_long = text
            .strip_prefix("did:web:")
            .is_some_and(|host| host.len() > MAX_HOSTNAME);
        if DID.is_match(text) && !hostname_too_long {
            return Ok(Self(text.to_owned()));
        }
        Err(match text.strip_prefix("did:") {
            None => format!(
                "{text:?} is not a DID (a handle is not one: find the DID with `tt identity resolve <handle>`)"
            ),
            Some(rest) => match rest.split_once(':') {
                Some(("plc", _)) => {
                    format!(
                        "{text:?} is not a did:plc DID: it has 24 characters from a-z and 2-7 after did:plc:"
                    )
                }
                Some(("web", host)) if host.contains(':') => {
                    format!("{text:?} names a path: only hostname-level did:web DIDs are accepted")
                }
                Some(("web", host)) if host.contains("%3A") => {
                    format!("{text:?} has a port: only did:web:localhost may have one")
                }
                Some(("web", _)) => format!("{text:?} does not name a lowercase hostname"),
                _ => format!("{text:?} is not a did:plc or did:web DID"),
            },
        })
    }
}

impl TryFrom<String> for Did {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Did> for String {
    fn from(did: Did) -> Self {
        did.0
    }
}

impl AsRef<str> for Did {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Did {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plc_and_hostname_web_dids() {
        for valid in [
            "did:plc:abcdefghijklmnopqrstuvwx",
            "did:plc:ewvi7nxzyoun6zhxrhs64oiz",
            "did:web:rawkode.dev",
            "did:web:pds.example.co.uk",
            "did:web:xn--bcher-kva.example",
            "did:web:localhost",
            "did:web:localhost%3A9400",
        ] {
            let did: Did = valid.parse().unwrap_or_else(|e| panic!("{valid}: {e}"));
            assert_eq!(did.as_str(), valid);
        }
    }

    #[test]
    fn refuses_anything_else() {
        let refused = |text: &str| text.parse::<Did>().expect_err(text);
        assert!(refused("alice.bsky.social").contains("not a DID"));
        assert!(refused("did:web:example.com:alice").contains("path"));
        assert!(refused("did:web:example.com%3A8443").contains("port"));
        assert!(refused("did:plc:abc").contains("24 characters"));
        for invalid in [
            "",
            "did:",
            "did:plc:",
            "did:plc:abcdefghijklmnopqrstuvw",
            "did:plc:abcdefghijklmnopqrstuvwxy",
            "did:plc:ABCDEFGHIJKLMNOPQRSTUVWX",
            "did:plc:abcdefghijklmnopqrstuvw1",
            "did:web:",
            "did:web:Rawkode.dev",
            "did:web:example",
            "did:web:-example.com",
            "did:web:example-.com",
            "did:web:example.com/",
            "did:web:example.com.",
            "did:web:localhost%3A",
            "did:web:localhost%3A0",
            "did:web:localhost%3A123456",
            "did:web:localhost%3a9400",
            "did:web:localhost:9400",
            "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme",
            " did:plc:abcdefghijklmnopqrstuvwx",
            "did:plc:abcdefghijklmnopqrstuvwx\n",
        ] {
            assert!(invalid.parse::<Did>().is_err(), "{invalid:?}");
        }
        let long = format!("did:web:{}.com", vec!["a".repeat(60); 5].join("."));
        assert!(
            long.parse::<Did>().is_err(),
            "a hostname over 253 characters"
        );
    }

    #[test]
    fn localhost_is_development_only() {
        let did = |text: &str| text.parse::<Did>().expect("valid");
        assert!(did("did:web:localhost").is_localhost());
        assert!(did("did:web:localhost%3A9400").is_localhost());
        assert!(!did("did:web:localhost.example.com").is_localhost());
        assert!(!did("did:plc:abcdefghijklmnopqrstuvwx").is_localhost());
    }

    #[test]
    fn round_trips_through_serde() {
        let did: Did = serde_json::from_str("\"did:web:rawkode.dev\"").expect("valid");
        assert_eq!(
            serde_json::to_string(&did).expect("json"),
            "\"did:web:rawkode.dev\""
        );
        assert!(serde_json::from_str::<Did>("\"rawkode.dev\"").is_err());
    }
}
