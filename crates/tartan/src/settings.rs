//! The forge's settings from the environment, checked before anything starts (design D13, the
//! *Configuration (environment)* table, D1 *Validation* and D12).
//!
//! Every check here runs without the network, so a forge whose PLC directory or owners' PDSes
//! are down still starts. Each refusal names the variable to change.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use ipnet::IpNet;
use jacquard_oauth::keyset::Keyset;
use jose_jwk::{EcCurves, Jwk, Key};
use reqwest::Url;

/// The public Jetstream instance used when `TARTAN_JETSTREAM_URL` is unset outside development.
pub const DEFAULT_JETSTREAM_URL: &str = "wss://jetstream2.us-east.bsky.network/subscribe";

/// Which OAuth client the forge's web sign-in is.
#[derive(Clone)]
pub enum WebClient {
    /// The confidential client of design D1: `<TARTAN_PUBLIC_URL>/-/auth/client-metadata.json`,
    /// authenticated with `private_key_jwt` signed by the first key of `TARTAN_OAUTH_KEYS_FILE`.
    Confidential(Keyset),
    /// ATProto's `http://localhost` development client: a public client whose metadata the
    /// authorization server derives from the client id itself, with a loopback redirect to
    /// `http://127.0.0.1:<port>/-/auth/callback`. Only `dev-identity` builds started with
    /// `TARTAN_DEV_LOCALHOST_CLIENT=1` use it, so a forge on a developer's machine can sign in
    /// against real PDSes, which cannot reach it.
    Localhost,
    /// A public native client whose metadata document the developer hosts at an `https` URL
    /// (the client id), with the loopback redirect `http://127.0.0.1:<port>/-/auth/callback`.
    /// For authorization servers that accept only `https` client ids. Only `dev-identity`
    /// builds started with `TARTAN_DEV_CLIENT_METADATA_URL` use it; the forge serves the
    /// document to copy at `/-/auth/client-metadata.json`.
    Native {
        /// The URL the metadata document is hosted at.
        client_id: String,
    },
}

impl std::fmt::Debug for WebClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Confidential(_) => f.write_str("Confidential(..)"),
            Self::Localhost => f.write_str("Localhost"),
            Self::Native { client_id } => write!(f, "Native({client_id})"),
        }
    }
}

/// What `TARTAN_PUBLIC_URL` may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicUrlRules {
    /// `https://<host>`, without port or path.
    Production,
    /// As production, or `http://127.0.0.1:<port>` (dev-identity).
    Development,
    /// Only `http://127.0.0.1:<port>`, for the `http://localhost` development client.
    Loopback,
}

/// The browser-facing origin of the forge (design D1): it serves the web UI and forwards `/-/`
/// and `/v2/` to the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicUrl {
    origin: String,
}

impl PublicUrl {
    /// Parses and checks `text` under `rules`.
    pub fn parse(text: &str, rules: PublicUrlRules) -> Result<Self, String> {
        let url = Url::parse(text.trim())
            .map_err(|error| format!("TARTAN_PUBLIC_URL {text:?} is not a URL: {error}"))?;
        let problem = |what: &str| format!("TARTAN_PUBLIC_URL {text:?} {what}");
        if !url.username().is_empty() || url.password().is_some() {
            return Err(problem("must not carry a user name or password"));
        }
        if !(url.path().is_empty() || url.path() == "/") {
            return Err(problem(
                "must have no path: it is the origin that serves the web UI",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(problem("must have no query or fragment"));
        }
        let host = url.host_str().unwrap_or_default();
        let loopback = url.scheme() == "http"
            && host.parse::<Ipv4Addr>() == Ok(Ipv4Addr::LOCALHOST)
            && url.port().is_some();
        match (url.scheme(), rules) {
            ("http", PublicUrlRules::Development | PublicUrlRules::Loopback) => {
                if host == "localhost" {
                    return Err(problem(
                        "must use 127.0.0.1, not localhost: an http://localhost client id is \
                         a public client, not this forge's confidential one",
                    ));
                }
                if !loopback {
                    return Err(problem("must be http://127.0.0.1:<port> in development"));
                }
            }
            ("https", PublicUrlRules::Production | PublicUrlRules::Development) => {
                if url.port().is_some() {
                    return Err(problem(
                        "must not name a port: ATProto client ids have none",
                    ));
                }
                if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
                    return Err(problem("must name a host, not an IP address"));
                }
            }
            (_, PublicUrlRules::Loopback) => {
                return Err(problem(
                    "must be http://127.0.0.1:<port> with TARTAN_DEV_LOCALHOST_CLIENT=1",
                ));
            }
            _ => return Err(problem("must be an https URL")),
        }
        let mut origin = format!("{}://{host}", url.scheme());
        if let Some(port) = url.port() {
            origin.push_str(&format!(":{port}"));
        }
        Ok(Self { origin })
    }

    /// `scheme://host[:port]`, without a trailing slash.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// `path` (starting with `/`) on this origin.
    pub fn join(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    /// Whether the origin is plain HTTP, where cookies cannot be `Secure` or `__Host-`.
    pub fn is_http(&self) -> bool {
        self.origin.starts_with("http://")
    }

    /// The host (and port), as a client name shows it.
    pub fn host(&self) -> &str {
        self.origin
            .split_once("://")
            .map_or(self.origin.as_str(), |(_, host)| host)
    }
}

/// Reads the private ES256 JWKS at `path` (design D1, *The client keys are the operator's*):
/// at least one key, every key a P-256 private key with a distinct `kid` whose public half
/// matches it; the file must not be readable by group or others.
pub fn load_keyset(path: &Path) -> Result<Keyset, String> {
    let shown = path.display();
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("TARTAN_OAUTH_KEYS_FILE {shown}: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "TARTAN_OAUTH_KEYS_FILE {shown} is readable by group or others (mode {mode:o}); \
                 run chmod 600 on it"
            ));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("TARTAN_OAUTH_KEYS_FILE {shown}: {error}"))?;
    parse_keyset(&text).map_err(|error| format!("TARTAN_OAUTH_KEYS_FILE {shown}: {error}"))
}

/// Parses a private ES256 JWKS (`{"keys": [...]}`), checking each key.
pub fn parse_keyset(text: &str) -> Result<Keyset, String> {
    #[derive(serde::Deserialize)]
    struct Jwks {
        keys: Vec<serde_json::Value>,
    }
    let jwks: Jwks = serde_json::from_str(text)
        .map_err(|error| format!("not a JWKS ({{\"keys\": [...]}}): {error}"))?;
    if jwks.keys.is_empty() {
        return Err("holds no keys; make one with tt admin oauth keygen".to_owned());
    }
    let mut keys = Vec::with_capacity(jwks.keys.len());
    let mut kids = std::collections::BTreeSet::new();
    for (index, value) in jwks.keys.into_iter().enumerate() {
        let jwk: Jwk = serde_json::from_value(value)
            .map_err(|error| format!("key {index} is not a JWK: {error}"))?;
        let kid = jwk
            .prm
            .kid
            .clone()
            .filter(|kid| !kid.is_empty())
            .ok_or_else(|| format!("key {index} has no kid"))?;
        if !kids.insert(kid.clone()) {
            return Err(format!("key {index}: kid {kid:?} is used twice"));
        }
        let Key::Ec(ec) = &jwk.key else {
            return Err(format!("key {kid:?} is not an EC key; ES256 needs P-256"));
        };
        if ec.crv != EcCurves::P256 {
            return Err(format!("key {kid:?} is not on P-256; ES256 needs P-256"));
        }
        let Some(d) = &ec.d else {
            return Err(format!(
                "key {kid:?} has no private part (d); the file holds private keys"
            ));
        };
        let secret = p256::SecretKey::from_slice(d.as_ref())
            .map_err(|_| format!("key {kid:?} has an invalid private part"))?;
        let point = p256::EncodedPoint::from(secret.public_key());
        let (x, y) = (point.x().map(|x| x.to_vec()), point.y().map(|y| y.to_vec()));
        let (jx, jy): (&[u8], &[u8]) = (ec.x.as_ref(), ec.y.as_ref());
        if x.as_deref() != Some(jx) || y.as_deref() != Some(jy) {
            return Err(format!(
                "key {kid:?}: its public part (x, y) does not match its private part"
            ));
        }
        keys.push(jwk);
    }
    Keyset::try_from(keys).map_err(|error| format!("{error}"))
}

/// Parses `TARTAN_TRUSTED_PROXIES`: comma-separated CIDRs or single addresses.
pub fn parse_trusted_proxies(text: &str) -> Result<Vec<IpNet>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("TARTAN_TRUSTED_PROXIES: {entry:?} is not an address or CIDR"))
        })
        .collect()
}

/// The `tt` command that replaces a `tartan` subcommand (design D16).
pub fn refuse_arguments(args: &[String]) -> Result<(), String> {
    let Some(first) = args.first() else {
        return Ok(());
    };
    let replacement = match first.as_str() {
        "token" | "tokens" => "tt admin token create --did <owner>",
        "oauth" | "keygen" => "tt admin oauth keygen",
        "identity" | "resolve" => "tt identity resolve <handle>",
        "login" => "tt login",
        "admin" => "tt admin",
        _ => "tt (see tt --help)",
    };
    Err(format!(
        "tartan is the server and takes no arguments (got {first:?}); use {replacement}"
    ))
}

/// The startup log line naming who may read (design D13).
pub fn read_mode_line(readers: &tartan_config::deployment::Readers) -> String {
    use tartan_config::deployment::Readers;
    match readers {
        Readers::Owners => "readers: owners (only owners can read; other people who sign in see \
                            a private-forge page)"
            .to_owned(),
        Readers::SignedIn => "readers: signed_in (any ATProto account can sign in and read every \
                              org and repository)"
            .to_owned(),
        Readers::Dids(dids) => format!(
            "readers: {} listed DIDs (owners and these DIDs can read)",
            dids.len()
        ),
    }
}

/// The settings the environment gives.
#[derive(Debug)]
pub struct Settings {
    /// Whether the development identity switches act (`TARTAN_DEV_IDENTITY=1`).
    pub dev_identity: bool,
    /// The development PLC directory and handle resolver, under dev-identity.
    pub dev_resolver: Option<(String, String)>,
    /// The browser-facing origin.
    pub public_url: PublicUrl,
    /// The web sign-in's OAuth client.
    pub client: WebClient,
    /// The Jetstream to subscribe to; `None` when disabled.
    pub jetstream_url: Option<String>,
    /// Reverse proxies whose `X-Forwarded-For` gives the client address.
    pub trusted_proxies: Vec<IpNet>,
    /// Where the forge keeps its disposable local state.
    pub state_dir: PathBuf,
}

/// Reads and checks the environment through `env` (`None` for an unset variable).
/// `dev_compiled` says whether this build has the `dev-identity` feature.
pub fn from_env(
    env: &dyn Fn(&str) -> Option<String>,
    dev_compiled: bool,
) -> Result<Settings, String> {
    if env("TARTAN_ADMIN_TOKEN").is_some() {
        return Err(
            "TARTAN_ADMIN_TOKEN is no longer accepted: list the administrators' DIDs \
                    in owners in the deployment configuration (TARTAN_CONFIG), sign in through \
                    the web UI, and give scripts a token from tt admin token create"
                .to_owned(),
        );
    }
    let flag = |name: &str| -> Result<bool, String> {
        match env(name).as_deref() {
            None => Ok(false),
            Some(_) if !dev_compiled => Err(format!(
                "{name} is set, but this tartan was built without the dev-identity feature; \
                 unset {name}"
            )),
            Some("1") => Ok(true),
            Some(other) => Err(format!("{name} must be 1 or unset, not {other:?}")),
        }
    };
    let dev_identity = flag("TARTAN_DEV_IDENTITY")?;
    let localhost_client = flag("TARTAN_DEV_LOCALHOST_CLIENT")?;
    let native_client = match env("TARTAN_DEV_CLIENT_METADATA_URL") {
        None => None,
        Some(_) if !dev_compiled => {
            return Err(
                "TARTAN_DEV_CLIENT_METADATA_URL is set, but this tartan was built \
                        without the dev-identity feature; unset TARTAN_DEV_CLIENT_METADATA_URL"
                    .to_owned(),
            );
        }
        Some(_) if localhost_client => {
            return Err(
                "set TARTAN_DEV_LOCALHOST_CLIENT or TARTAN_DEV_CLIENT_METADATA_URL, \
                        not both"
                    .to_owned(),
            );
        }
        Some(url) => {
            let parsed = Url::parse(&url).map_err(|error| {
                format!("TARTAN_DEV_CLIENT_METADATA_URL {url:?} is not a URL: {error}")
            })?;
            if parsed.scheme() != "https" && !dev_identity {
                return Err(format!(
                    "TARTAN_DEV_CLIENT_METADATA_URL {url:?} must be https: authorization \
                     servers fetch it"
                ));
            }
            Some(url)
        }
    };
    if env("TARTAN_CONFIG").is_none_or(|value| value.is_empty()) {
        return Err(
            "TARTAN_CONFIG must name the deployment configuration (package forge), which lists \
             the forge's owners"
                .to_owned(),
        );
    }
    let required = |name: &str| -> Result<String, String> {
        env(name)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{name} must be set"))
    };
    let dev_resolver = if dev_identity {
        let plc = required("TARTAN_PLC_URL")
            .map_err(|error| format!("{error} with TARTAN_DEV_IDENTITY=1"))?;
        let handles = required("TARTAN_DEV_HANDLE_RESOLVER")
            .map_err(|error| format!("{error} with TARTAN_DEV_IDENTITY=1"))?;
        Some((plc, handles))
    } else {
        None
    };
    let jetstream_url = match env("TARTAN_JETSTREAM_URL") {
        Some(url) if url.is_empty() => None,
        Some(url) => Some(url),
        None if dev_identity => {
            return Err(
                "TARTAN_JETSTREAM_URL must be set with TARTAN_DEV_IDENTITY=1: the \
                        devnet's /subscribe, or empty to disable Jetstream"
                    .to_owned(),
            );
        }
        None => Some(DEFAULT_JETSTREAM_URL.to_owned()),
    };
    let rules = if localhost_client || native_client.is_some() {
        PublicUrlRules::Loopback
    } else if dev_identity {
        PublicUrlRules::Development
    } else {
        PublicUrlRules::Production
    };
    let public_url = PublicUrl::parse(&required("TARTAN_PUBLIC_URL")?, rules)?;
    let client = if localhost_client {
        WebClient::Localhost
    } else if let Some(client_id) = native_client {
        WebClient::Native { client_id }
    } else {
        WebClient::Confidential(load_keyset(Path::new(&required(
            "TARTAN_OAUTH_KEYS_FILE",
        )?))?)
    };
    let trusted_proxies =
        parse_trusted_proxies(&env("TARTAN_TRUSTED_PROXIES").unwrap_or_default())?;
    let state_dir = state_dir(env)?;
    Ok(Settings {
        dev_identity,
        dev_resolver,
        public_url,
        client,
        jetstream_url,
        trusted_proxies,
        state_dir,
    })
}

/// `TARTAN_STATE_DIR`, else `$XDG_STATE_HOME/tartan`, else `$HOME/.local/state/tartan`, created
/// if missing.
fn state_dir(env: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    let dir = if let Some(dir) = set("TARTAN_STATE_DIR") {
        PathBuf::from(dir)
    } else if let Some(state) = set("XDG_STATE_HOME") {
        Path::new(&state).join("tartan")
    } else if let Some(home) = set("HOME") {
        Path::new(&home).join(".local/state/tartan")
    } else {
        return Err(
            "no state directory: set TARTAN_STATE_DIR (or XDG_STATE_HOME or HOME)".to_owned(),
        );
    };
    std::fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "the state directory {} is not usable: {error}",
            dir.display()
        )
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn private_jwk(seed: u8, kid: &str) -> serde_json::Value {
        let secret = p256::SecretKey::from_slice(&[seed; 32]).expect("a key");
        let mut jwk = serde_json::to_value(Jwk {
            key: Key::from(&jose_jwk::crypto::Key::from(secret)),
            prm: Default::default(),
        })
        .expect("jwk");
        jwk["kid"] = serde_json::json!(kid);
        jwk
    }

    fn keys_file(dir: &Path, keys: serde_json::Value, mode: u32) -> PathBuf {
        let path = dir.join("keys.json");
        std::fs::write(&path, serde_json::json!({ "keys": keys }).to_string()).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        }
        path
    }

    #[test]
    fn public_urls() {
        use PublicUrlRules::*;
        let ok =
            |text: &str, rules| PublicUrl::parse(text, rules).map(|url| url.origin().to_owned());
        assert_eq!(
            ok("https://forge.example", Production).as_deref(),
            Ok("https://forge.example")
        );
        assert_eq!(
            ok("https://forge.example/", Production).as_deref(),
            Ok("https://forge.example")
        );
        for bad in [
            "http://forge.example",
            "https://forge.example:8443",
            "https://forge.example/tartan",
            "https://forge.example/?x=1",
            "https://user@forge.example",
            "https://10.0.0.5",
            "https://[::1]",
            "http://127.0.0.1:4321",
            "forge.example",
        ] {
            assert!(
                ok(bad, Production).is_err(),
                "{bad} is refused in production"
            );
        }
        assert_eq!(
            ok("http://127.0.0.1:4321", Development).as_deref(),
            Ok("http://127.0.0.1:4321")
        );
        assert_eq!(
            ok("https://forge.example", Development).as_deref(),
            Ok("https://forge.example")
        );
        for bad in [
            "http://localhost:4321",
            "http://127.0.0.1",
            "http://10.0.0.5:80",
            "http://forge.example:80",
        ] {
            assert!(
                ok(bad, Development).is_err(),
                "{bad} is refused in development"
            );
        }
        assert_eq!(
            ok("http://127.0.0.1:4321", Loopback).as_deref(),
            Ok("http://127.0.0.1:4321")
        );
        for bad in [
            "https://forge.example",
            "http://localhost:4321",
            "http://127.0.0.1",
        ] {
            assert!(
                ok(bad, Loopback).is_err(),
                "{bad} is refused for the localhost client"
            );
        }
        let url = PublicUrl::parse("http://127.0.0.1:4321", Development).expect("url");
        assert!(url.is_http());
        assert_eq!(
            url.join("/-/auth/callback"),
            "http://127.0.0.1:4321/-/auth/callback"
        );
        assert_eq!(url.host(), "127.0.0.1:4321");
    }

    #[test]
    fn key_files() {
        let dir = tempfile::tempdir().expect("dir");
        let good = keys_file(dir.path(), serde_json::json!([private_jwk(7, "k1")]), 0o600);
        let keyset = load_keyset(&good).expect("a valid key file");
        let public = serde_json::to_value(keyset.public_jwks()).expect("jwks");
        assert!(public["keys"][0].get("d").is_none(), "{public}");

        #[cfg(unix)]
        {
            let open = keys_file(dir.path(), serde_json::json!([private_jwk(7, "k1")]), 0o640);
            let error = load_keyset(&open).expect_err("refused");
            assert!(error.contains("chmod 600"), "{error}");
        }
        let refused = |keys: serde_json::Value| {
            parse_keyset(&serde_json::json!({ "keys": keys }).to_string()).expect_err("refused")
        };
        assert!(refused(serde_json::json!([])).contains("no keys"));
        let mut no_kid = private_jwk(7, "k1");
        no_kid.as_object_mut().expect("object").remove("kid");
        assert!(refused(serde_json::json!([no_kid])).contains("no kid"));
        assert!(
            refused(serde_json::json!([
                private_jwk(7, "k1"),
                private_jwk(8, "k1")
            ]))
            .contains("twice")
        );
        let mut public_only = private_jwk(7, "k1");
        public_only.as_object_mut().expect("object").remove("d");
        assert!(refused(serde_json::json!([public_only])).contains("no private part"));
        let mut mismatched = private_jwk(7, "k1");
        mismatched["x"] = private_jwk(8, "k2")["x"].clone();
        assert!(refused(serde_json::json!([mismatched])).contains("does not match"));
        let okp = serde_json::json!({"kty": "OKP", "crv": "Ed25519", "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo", "kid": "e"});
        assert!(refused(serde_json::json!([okp])).contains("not an EC key"));
        assert!(parse_keyset("not json").is_err());
    }

    #[test]
    fn the_read_mode_is_named() {
        use tartan_config::deployment::Readers;
        assert!(read_mode_line(&Readers::Owners).contains("only owners can read"));
        assert!(read_mode_line(&Readers::SignedIn).contains("any ATProto account"));
        let did = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb".parse().expect("a DID");
        assert!(read_mode_line(&Readers::Dids(vec![did])).starts_with("readers: 1 listed"));
    }

    #[test]
    fn trusted_proxies() {
        let proxies = parse_trusted_proxies("127.0.0.1/32, 10.0.0.0/8,::1").expect("proxies");
        assert_eq!(proxies.len(), 3);
        assert!(parse_trusted_proxies("").expect("empty").is_empty());
        assert!(parse_trusted_proxies("proxy.example").is_err());
    }

    #[test]
    fn subcommands_name_their_tt_replacement() {
        assert!(refuse_arguments(&[]).is_ok());
        let error = refuse_arguments(&["token".into(), "create".into()]).expect_err("refused");
        assert!(error.contains("tt admin token create"), "{error}");
        let error = refuse_arguments(&["oauth".into()]).expect_err("refused");
        assert!(error.contains("tt admin oauth keygen"), "{error}");
    }

    struct Env(HashMap<&'static str, String>);

    impl Env {
        fn production(dir: &Path) -> Self {
            let keys = keys_file(dir, serde_json::json!([private_jwk(7, "k1")]), 0o600);
            Self(HashMap::from([
                ("TARTAN_CONFIG", "/etc/tartan/forge.cue".to_owned()),
                ("TARTAN_PUBLIC_URL", "https://forge.example".to_owned()),
                ("TARTAN_OAUTH_KEYS_FILE", keys.display().to_string()),
                ("TARTAN_STATE_DIR", dir.join("state").display().to_string()),
            ]))
        }

        fn with(mut self, name: &'static str, value: &str) -> Self {
            self.0.insert(name, value.to_owned());
            self
        }

        fn without(mut self, name: &'static str) -> Self {
            self.0.remove(name);
            self
        }

        fn check(&self, dev_compiled: bool) -> Result<Settings, String> {
            from_env(&|name| self.0.get(name).cloned(), dev_compiled)
        }
    }

    #[test]
    fn startup_checks() {
        let dir = tempfile::tempdir().expect("dir");
        let settings = Env::production(dir.path())
            .check(false)
            .expect("a production forge starts");
        assert!(!settings.dev_identity);
        assert_eq!(
            settings.jetstream_url.as_deref(),
            Some(DEFAULT_JETSTREAM_URL)
        );
        assert!(matches!(settings.client, WebClient::Confidential(_)));
        assert!(dir.path().join("state").is_dir());

        let refused = |env: Env, dev: bool, needle: &str| {
            let error = env.check(dev).expect_err("refused");
            assert!(error.contains(needle), "{error} names {needle}");
        };
        let base = || Env::production(dir.path());
        refused(base().without("TARTAN_CONFIG"), false, "TARTAN_CONFIG");
        refused(
            base().with("TARTAN_ADMIN_TOKEN", "an-old-admin-token"),
            false,
            "owners",
        );
        refused(
            base().with("TARTAN_DEV_IDENTITY", "1"),
            false,
            "without the dev-identity feature",
        );
        refused(
            base().with("TARTAN_DEV_LOCALHOST_CLIENT", "1"),
            false,
            "without the dev-identity feature",
        );
        refused(
            base().without("TARTAN_PUBLIC_URL"),
            false,
            "TARTAN_PUBLIC_URL",
        );
        refused(
            base().with("TARTAN_PUBLIC_URL", "http://127.0.0.1:8080"),
            false,
            "https",
        );
        refused(
            base().without("TARTAN_OAUTH_KEYS_FILE"),
            false,
            "TARTAN_OAUTH_KEYS_FILE",
        );
        refused(
            base().with("TARTAN_TRUSTED_PROXIES", "nope"),
            false,
            "TARTAN_TRUSTED_PROXIES",
        );
        refused(
            base()
                .without("TARTAN_STATE_DIR")
                .without("XDG_STATE_HOME")
                .without("HOME"),
            false,
            "TARTAN_STATE_DIR",
        );
        let empty_jetstream = base()
            .with("TARTAN_JETSTREAM_URL", "")
            .check(false)
            .expect("starts");
        assert_eq!(empty_jetstream.jetstream_url, None);

        // Development: the devnet's three URLs are each required, by name.
        let dev = || {
            base()
                .with("TARTAN_DEV_IDENTITY", "1")
                .with("TARTAN_PLC_URL", "http://127.0.0.1:9400")
                .with("TARTAN_DEV_HANDLE_RESOLVER", "http://127.0.0.1:9400")
                .with("TARTAN_JETSTREAM_URL", "ws://127.0.0.1:9400/subscribe")
                .with("TARTAN_PUBLIC_URL", "http://127.0.0.1:4321")
        };
        let settings = dev().check(true).expect("a development forge starts");
        assert!(settings.dev_identity);
        refused(dev().without("TARTAN_PLC_URL"), true, "TARTAN_PLC_URL");
        refused(
            dev().without("TARTAN_DEV_HANDLE_RESOLVER"),
            true,
            "TARTAN_DEV_HANDLE_RESOLVER",
        );
        refused(
            dev().without("TARTAN_JETSTREAM_URL"),
            true,
            "TARTAN_JETSTREAM_URL",
        );
        refused(
            dev().with("TARTAN_PUBLIC_URL", "http://localhost:4321"),
            true,
            "127.0.0.1",
        );
        refused(dev().with("TARTAN_DEV_IDENTITY", "yes"), true, "must be 1");

        // The localhost client needs no key file and only a loopback origin.
        let localhost = base()
            .with("TARTAN_DEV_LOCALHOST_CLIENT", "1")
            .with("TARTAN_PUBLIC_URL", "http://127.0.0.1:4321")
            .without("TARTAN_OAUTH_KEYS_FILE");
        let settings = localhost.check(true).expect("the localhost client starts");
        assert!(matches!(settings.client, WebClient::Localhost));
        assert!(!settings.dev_identity, "it works against the real network");
        refused(
            base().with("TARTAN_DEV_LOCALHOST_CLIENT", "1"),
            true,
            "http://127.0.0.1:<port>",
        );

        // A hosted native client: https metadata, a loopback origin, no key file.
        let native = || {
            base()
                .with(
                    "TARTAN_DEV_CLIENT_METADATA_URL",
                    "https://dev.example/tartan.json",
                )
                .with("TARTAN_PUBLIC_URL", "http://127.0.0.1:4321")
                .without("TARTAN_OAUTH_KEYS_FILE")
        };
        let settings = native().check(true).expect("the native client starts");
        assert!(matches!(settings.client, WebClient::Native { .. }));
        refused(native(), false, "without the dev-identity feature");
        refused(
            native().with(
                "TARTAN_DEV_CLIENT_METADATA_URL",
                "http://dev.example/t.json",
            ),
            true,
            "must be https",
        );
        refused(
            native().with("TARTAN_DEV_LOCALHOST_CLIENT", "1"),
            true,
            "not both",
        );
    }
}
