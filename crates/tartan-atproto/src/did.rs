//! DID syntax.
//!
//! Tartan identifies people only by DID, and accepts two methods (design D9, spec
//! atproto-identity): `did:plc` and hostname-only `did:web`. A `did:web` with a path is refused,
//! and a port (`%3A<port>`) is accepted only for `localhost` and names under it in
//! [`Mode::Development`], as the ATProto DID specification allows for local testing.

use std::fmt;

use crate::Mode;

/// The longest DID ATProto accepts.
pub const MAX_DID_LEN: usize = 2048;

/// The longest hostname (RFC 1035).
pub const MAX_HOST_LEN: usize = 253;

/// A DID method Tartan accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DidMethod {
    /// `did:plc:<24 base32 characters>`.
    Plc,
    /// `did:web:<hostname>`.
    Web,
}

/// A syntactically valid `did:plc` or hostname-only `did:web` DID.
///
/// The string is kept exactly as given: DIDs are compared byte for byte, and only the canonical
/// spelling (lowercase) is accepted, so two accepted spellings never name the same DID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Did {
    value: String,
    method: DidMethod,
}

/// Why a string is not an acceptable DID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DidError {
    /// The string does not start with `did:`.
    #[error("not a DID")]
    NotADid,
    /// A method other than `plc` or `web`.
    #[error("unsupported DID method `{0}`")]
    UnsupportedMethod(String),
    /// The `did:plc` identifier is not 24 lowercase base32 characters.
    #[error("a did:plc identifier is 24 characters of a-z and 2-7")]
    BadPlcIdentifier,
    /// The `did:web` DID names a path (`did:web:host:path`).
    #[error("a did:web DID may name only a hostname, not a path")]
    WebPath,
    /// The `did:web` host is not a valid lowercase hostname.
    #[error("the did:web host is not a valid lowercase hostname")]
    BadHost,
    /// A port on a host other than `localhost`, or outside development mode.
    #[error("a did:web port is allowed only for localhost in development mode")]
    PortNotAllowed,
    /// Longer than [`MAX_DID_LEN`].
    #[error("the DID is longer than {MAX_DID_LEN} characters")]
    TooLong,
}

impl Did {
    /// Parses `value` under the rules of `mode`.
    pub fn parse(value: &str, mode: Mode) -> Result<Self, DidError> {
        if value.len() > MAX_DID_LEN {
            return Err(DidError::TooLong);
        }
        let rest = value.strip_prefix("did:").ok_or(DidError::NotADid)?;
        let (method, ident) = rest.split_once(':').ok_or(DidError::NotADid)?;
        match method {
            "plc" => {
                if ident.len() == 24 && ident.bytes().all(is_base32_lower) {
                    Ok(Did {
                        value: value.to_owned(),
                        method: DidMethod::Plc,
                    })
                } else {
                    Err(DidError::BadPlcIdentifier)
                }
            }
            "web" => {
                check_web_identifier(ident, mode)?;
                Ok(Did {
                    value: value.to_owned(),
                    method: DidMethod::Web,
                })
            }
            other => Err(DidError::UnsupportedMethod(other.to_owned())),
        }
    }

    /// The DID as a string.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// The method.
    pub fn method(&self) -> DidMethod {
        self.method
    }

    /// For `did:web`, the host and the port, if any (the port only in development mode).
    pub fn web_host(&self) -> Option<(&str, Option<u16>)> {
        if self.method != DidMethod::Web {
            return None;
        }
        let ident = &self.value["did:web:".len()..];
        match ident.split_once("%3A") {
            Some((host, port)) => Some((host, port.parse().ok())),
            None => Some((ident, None)),
        }
    }
}

impl fmt::Display for Did {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

impl AsRef<str> for Did {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

fn is_base32_lower(b: u8) -> bool {
    b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)
}

fn check_web_identifier(ident: &str, mode: Mode) -> Result<(), DidError> {
    if ident.contains(':') {
        return Err(DidError::WebPath);
    }
    let (host, port) = match ident.split_once("%3A") {
        Some((host, port)) => (host, Some(port)),
        None => (ident, None),
    };
    if let Some(port) = port {
        let local = host == "localhost" || host.ends_with(".localhost");
        let digits = !port.is_empty()
            && port.len() <= 5
            && port.bytes().all(|b| b.is_ascii_digit())
            && !port.starts_with('0')
            && port.parse::<u16>().is_ok();
        if !digits {
            return Err(DidError::BadHost);
        }
        if !(local && mode.is_development()) {
            return Err(DidError::PortNotAllowed);
        }
    }
    if !is_hostname(host, mode) {
        return Err(DidError::BadHost);
    }
    Ok(())
}

/// True when `host` is a lowercase DNS hostname: at most 253 characters, labels of 1 to 63
/// characters from `a-z`, `0-9` and `-` that neither start nor end with `-`, at least two labels
/// and a top-level label that does not start with a digit. In development mode the single label
/// `localhost` is also a hostname.
pub fn is_hostname(host: &str, mode: Mode) -> bool {
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return false;
    }
    if host == "localhost" {
        return mode.is_development();
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let labels_ok = labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    let tld_ok = labels
        .last()
        .and_then(|tld| tld.bytes().next())
        .is_some_and(|b| !b.is_ascii_digit());
    labels_ok && tld_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_dids() {
        for (value, method) in [
            ("did:plc:ewvi7nxzyoun6zhxrhs64oiz", DidMethod::Plc),
            ("did:plc:abcdefghijklmnopqrstuvwx", DidMethod::Plc),
            ("did:web:rawkode.dev", DidMethod::Web),
            ("did:web:a.example.com", DidMethod::Web),
            ("did:web:xn--bcher-kva.example", DidMethod::Web),
        ] {
            let did = Did::parse(value, Mode::Production).expect(value);
            assert_eq!(did.method(), method, "{value}");
            assert_eq!(did.as_str(), value);
        }
    }

    #[test]
    fn invalid_dids() {
        for (value, error) in [
            ("", DidError::NotADid),
            ("did", DidError::NotADid),
            ("did:plc", DidError::NotADid),
            ("alice.example.com", DidError::NotADid),
            ("DID:plc:ewvi7nxzyoun6zhxrhs64oiz", DidError::NotADid),
            (
                "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
                DidError::UnsupportedMethod("key".into()),
            ),
            (
                "did:PLC:ewvi7nxzyoun6zhxrhs64oiz",
                DidError::UnsupportedMethod("PLC".into()),
            ),
            (
                "did:plc:ewvi7nxzyoun6zhxrhs64oi",
                DidError::BadPlcIdentifier,
            ),
            (
                "did:plc:ewvi7nxzyoun6zhxrhs64oizz",
                DidError::BadPlcIdentifier,
            ),
            (
                "did:plc:EWVI7NXZYOUN6ZHXRHS64OIZ",
                DidError::BadPlcIdentifier,
            ),
            (
                "did:plc:ewvi7nxzyoun6zhxrhs64oi1",
                DidError::BadPlcIdentifier,
            ),
            (
                "did:plc:ewvi7nxzyoun6zhxrhs64oi8",
                DidError::BadPlcIdentifier,
            ),
            ("did:web:example.com:alice", DidError::WebPath),
            ("did:web:example.com:user:alice", DidError::WebPath),
            ("did:web:Example.com", DidError::BadHost),
            ("did:web:example", DidError::BadHost),
            ("did:web:-a.example", DidError::BadHost),
            ("did:web:a-.example", DidError::BadHost),
            ("did:web:a..example", DidError::BadHost),
            ("did:web:example.123", DidError::BadHost),
            ("did:web:exa_mple.com", DidError::BadHost),
            ("did:web:127.0.0.1", DidError::BadHost),
            ("did:web:", DidError::BadHost),
            ("did:web:example.com%3A8080", DidError::PortNotAllowed),
            ("did:web:localhost%3A9400", DidError::PortNotAllowed),
            ("did:web:localhost", DidError::BadHost),
            ("did:web:localhost%3Aabc", DidError::BadHost),
            ("did:web:localhost%3A99999", DidError::BadHost),
        ] {
            assert_eq!(Did::parse(value, Mode::Production), Err(error), "{value}");
        }
    }

    #[test]
    fn longest_host() {
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{}.com", "b".repeat(57));
        assert_eq!(host.len(), 253);
        let did = format!("did:web:{host}");
        assert!(Did::parse(&did, Mode::Production).is_ok());
        let too_long = format!("did:web:{label}.{label}.{label}.{}.com", "b".repeat(58));
        assert_eq!(
            Did::parse(&too_long, Mode::Production),
            Err(DidError::BadHost)
        );
        let long_label = format!("did:web:{}.com", "a".repeat(64));
        assert_eq!(
            Did::parse(&long_label, Mode::Production),
            Err(DidError::BadHost)
        );
    }

    #[cfg(feature = "dev-identity")]
    #[test]
    fn development_ports() {
        let dev = Mode::Development;
        for value in [
            "did:web:localhost%3A9400",
            "did:web:t7.localhost%3A9401",
            "did:web:localhost",
        ] {
            assert!(Did::parse(value, dev).is_ok(), "{value}");
        }
        let did = Did::parse("did:web:t7.localhost%3A9401", dev).expect("valid");
        assert_eq!(did.web_host(), Some(("t7.localhost", Some(9401))));
        for (value, error) in [
            ("did:web:example.com%3A8080", DidError::PortNotAllowed),
            ("did:web:localhost%3A", DidError::BadHost),
            ("did:web:localhost%3A09400", DidError::BadHost),
            ("did:web:localhost:9400", DidError::WebPath),
        ] {
            assert_eq!(Did::parse(value, dev), Err(error), "{value}");
        }
    }
}
