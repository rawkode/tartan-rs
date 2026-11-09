//! What every command shares: the identity mode, the resolver, `tt`'s directories and its
//! credential store.
//!
//! The development switches (design D12) compile only with the `dev-identity` feature and act
//! only with `TARTAN_DEV_IDENTITY=1`: then `TARTAN_PLC_URL` and `TARTAN_DEV_HANDLE_RESOLVER`
//! are required, plain HTTP and private addresses are allowed, and a forge on plain HTTP may
//! serve the signing client's metadata. A release build refuses to run with the variable set.

use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::Url;
use tartan_atproto::Mode;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tartan_atproto::resolver::{ResolverConfig, VerifyingResolver};

use crate::credentials::Store;
use crate::paths::Dirs;

/// The development network, under `TARTAN_DEV_IDENTITY=1`.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "dev-identity"), allow(dead_code))]
pub struct DevNetwork {
    /// `TARTAN_PLC_URL`.
    pub plc_url: String,
    /// `TARTAN_DEV_HANDLE_RESOLVER`.
    pub handle_resolver: String,
}

/// The shared context.
#[derive(Clone)]
pub struct Ctx {
    /// Production or development rules.
    pub mode: Mode,
    /// The development network, when the switches act.
    pub dev: Option<DevNetwork>,
    /// `tt`'s directories.
    pub dirs: Dirs,
    /// Where secrets go.
    pub store: Store,
}

impl Ctx {
    /// The context from the environment.
    pub fn from_env() -> Result<Self> {
        let (mode, dev) = mode_from_env()?;
        let dirs = Dirs::from_env()?;
        let store = Store::from_env(&dirs);
        Ok(Ctx {
            mode,
            dev,
            dirs,
            store,
        })
    }

    /// A fetch-safe client with `timeout` and an optional connect timeout.
    pub fn fetch(&self, timeout: Duration, connect: Option<Duration>) -> Result<FetchClient> {
        let mut config = FetchConfig::new(self.mode);
        config.timeout = timeout;
        config.connect_timeout = connect;
        config.user_agent = format!("tt/{}", env!("CARGO_PKG_VERSION"));
        Ok(FetchClient::new(config)?)
    }

    /// The verifying resolver over `fetch`.
    pub fn resolver_with(&self, fetch: FetchClient) -> Result<VerifyingResolver> {
        let config = match &self.dev {
            #[cfg(feature = "dev-identity")]
            Some(dev) => ResolverConfig::development(&dev.plc_url, &dev.handle_resolver),
            _ => ResolverConfig::production()
                .map_err(|error| anyhow::anyhow!("the system DNS configuration: {error}"))?,
        };
        Ok(VerifyingResolver::new(fetch, config))
    }

    /// The verifying resolver with the default timeouts.
    pub fn resolver(&self) -> Result<VerifyingResolver> {
        self.resolver_with(self.fetch(Duration::from_secs(15), None)?)
    }

    /// Normalises a forge URL to its origin (`scheme://host[:port]`).
    ///
    /// `https` is always accepted. Plain `http` is accepted for a loopback forge (a forge on
    /// the developer's own machine; nothing leaves it), and for any host under the development
    /// switches.
    pub fn forge_origin(&self, text: &str) -> Result<String> {
        let trimmed = text.trim().trim_end_matches('/');
        let url = Url::parse(trimmed).map_err(|_| {
            anyhow::anyhow!("{trimmed:?} is not a forge URL (such as https://forge.example)")
        })?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("{trimmed:?} has no host"))?;
        match url.scheme() {
            "https" => {}
            "http" if self.mode.is_development() || is_loopback_host(host) => {}
            scheme => bail!("a forge URL must be https, not {scheme} ({trimmed})"),
        }
        if url.path() != "/" && !url.path().is_empty() {
            bail!(
                "a forge URL has no path ({trimmed}); give its origin, such as https://forge.example"
            );
        }
        Ok(url.origin().ascii_serialization())
    }

    /// Whether a forge at `origin` can serve `tt`'s signing client metadata to authorization
    /// servers: `https`, or plain HTTP under the development switches.
    pub fn forge_serves_client_metadata(&self, origin: &str) -> bool {
        origin.starts_with("https://") || self.mode.is_development()
    }
}

/// Whether `host` names the loopback interface.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn dev_identity_requested() -> bool {
    std::env::var("TARTAN_DEV_IDENTITY").is_ok_and(|value| !value.is_empty() && value != "0")
}

#[cfg(feature = "dev-identity")]
fn mode_from_env() -> Result<(Mode, Option<DevNetwork>)> {
    if !dev_identity_requested() {
        return Ok((Mode::Production, None));
    }
    let required = |name: &str| -> Result<String> {
        match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(value.trim_end_matches('/').to_owned()),
            _ => bail!(
                "{name} must be set with TARTAN_DEV_IDENTITY=1; tt never falls back to the public \
                 network in development mode"
            ),
        }
    };
    let dev = DevNetwork {
        plc_url: required("TARTAN_PLC_URL")?,
        handle_resolver: required("TARTAN_DEV_HANDLE_RESOLVER")?,
    };
    Ok((Mode::Development, Some(dev)))
}

#[cfg(not(feature = "dev-identity"))]
fn mode_from_env() -> Result<(Mode, Option<DevNetwork>)> {
    if dev_identity_requested() {
        bail!(
            "TARTAN_DEV_IDENTITY is set, but this tt is a release build without the development \
             identity switches; unset TARTAN_DEV_IDENTITY (and do not export it from a shell \
             profile or .envrc)"
        );
    }
    Ok((Mode::Production, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        let dirs = Dirs {
            config: "/nonexistent/config".into(),
            state: "/nonexistent/state".into(),
            cache: "/nonexistent/cache".into(),
        };
        Ctx {
            mode: Mode::Production,
            dev: None,
            store: Store::File(dirs.credentials()),
            dirs,
        }
    }

    #[test]
    fn forge_urls_are_origins() {
        let ctx = ctx();
        assert_eq!(
            ctx.forge_origin("https://forge.example/").expect("origin"),
            "https://forge.example"
        );
        assert_eq!(
            ctx.forge_origin("http://127.0.0.1:8080").expect("origin"),
            "http://127.0.0.1:8080"
        );
        assert!(ctx.forge_origin("http://forge.example").is_err());
        assert!(ctx.forge_origin("https://forge.example/path").is_err());
        assert!(ctx.forge_origin("forge.example").is_err());
        assert!(!ctx.forge_serves_client_metadata("http://127.0.0.1:8080"));
        assert!(ctx.forge_serves_client_metadata("https://forge.example"));
    }
}
