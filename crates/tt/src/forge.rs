//! Talking to a forge's HTTP API with the stored forge credential (`tartan_cli_` from
//! `tt login`, or `tartan_pat_` from `tt login --with-token`).

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::ctx::Ctx;
use crate::state;

/// A forge credential as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeCredential {
    /// `cli` or `token`.
    pub kind: String,
    /// The secret.
    pub secret: String,
}

impl ForgeCredential {
    /// Whether this is an access token, which cannot manage tokens or withdraw signatures.
    pub fn is_token(&self) -> bool {
        self.kind == "token"
    }
}

/// Reads the stored credential for `forge`.
pub fn credential(ctx: &Ctx, forge: &str) -> Result<Option<ForgeCredential>> {
    let Some(text) = ctx.store.get(forge)? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .with_context(|| format!("the stored credential for {forge} is unreadable"))
}

/// The forge a command talks to: `--forge`, else `TT_FORGE`, else the default forge.
pub fn choose(ctx: &Ctx, flag: Option<&str>, config: &Config) -> Result<Option<String>> {
    let chosen = flag
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("TT_FORGE")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| config.file.default_forge.clone());
    chosen.map(|forge| ctx.forge_origin(&forge)).transpose()
}

/// A forge client.
pub struct Forge {
    /// The forge's origin.
    pub origin: String,
    http: reqwest::Client,
    credential: Option<ForgeCredential>,
}

/// A forge answer that was not a success.
#[derive(Debug)]
pub struct ForgeError {
    /// The status, or `None` when the forge could not be reached.
    pub status: Option<StatusCode>,
    /// What went wrong.
    pub message: String,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ForgeError {}

impl Forge {
    /// A client for `origin` with its stored credential, if any.
    pub fn new(ctx: &Ctx, origin: &str) -> Result<Self> {
        let credential = credential(ctx, origin)?;
        Self::with_credential(origin, credential)
    }

    /// A client for `origin` with `credential`.
    pub fn with_credential(origin: &str, credential: Option<ForgeCredential>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("tt/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Forge {
            origin: origin.to_owned(),
            http,
            credential,
        })
    }

    /// The credential.
    pub fn credential(&self) -> Option<&ForgeCredential> {
        self.credential.as_ref()
    }

    /// Fails unless a credential is stored, naming `tt login`.
    pub fn require_credential(&self) -> Result<&ForgeCredential> {
        self.credential.as_ref().ok_or_else(|| {
            anyhow!(
                "not signed in to {}; run `tt login --forge {}`",
                self.origin,
                self.origin
            )
        })
    }

    /// Sends a request to `path` (absolute, starting with `/-/`), with the credential.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ForgeError> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.origin))
            .header("Accept", "application/json");
        if let Some(credential) = &self.credential {
            request = request.bearer_auth(&credential.secret);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|error| ForgeError {
            status: None,
            message: format!(
                "the forge {} could not be reached: {}",
                self.origin,
                without_url(&error)
            ),
            retry_after: None,
        })?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok());
        let bytes = response.bytes().await.unwrap_or_default();
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        let detail = value
            .get("message")
            .or_else(|| value.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).trim().to_owned());
        let message = if status == StatusCode::UNAUTHORIZED {
            format!(
                "the forge {} refused the stored credential (it ended, expired or was revoked); \
                 run `tt login --forge {}`",
                self.origin, self.origin
            )
        } else if detail.is_empty() {
            format!("the forge answered {status}")
        } else {
            format!("the forge answered {status}: {detail}")
        };
        Err(ForgeError {
            status: Some(status),
            message,
            retry_after,
        })
    }

    /// `GET /-/api/me`.
    pub async fn me(&self) -> Result<Me, ForgeError> {
        let value = self.send(Method::GET, "/-/api/me", None).await?;
        serde_json::from_value(value).map_err(|error| ForgeError {
            status: None,
            message: format!("the forge's /-/api/me answer is not understood: {error}"),
            retry_after: None,
        })
    }
}

fn without_url(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(url.as_str(), "");
    }
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        text = format!("{text}: {inner}");
        source = inner.source();
    }
    text
}

/// `GET /-/api/me`'s answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Me {
    /// The DID, unless anonymous.
    #[serde(default)]
    pub did: Option<String>,
    /// The handle.
    #[serde(default)]
    pub handle: Option<String>,
    /// Whether the DID is an owner.
    #[serde(default)]
    pub owner: bool,
    /// The rights.
    #[serde(default)]
    pub rights: Vec<String>,
}

/// Stores `credential` for `forge` and indexes it.
pub fn store(
    ctx: &Ctx,
    forge: &str,
    credential: &ForgeCredential,
    me: &Me,
    expires_at: Option<i64>,
) -> Result<crate::credentials::Written> {
    let text = serde_json::to_string(credential).context("encoding the forge credential")?;
    let written = ctx.store.set(forge, &text)?;
    let entry = state::ForgeEntry {
        kind: credential.kind.clone(),
        did: me.did.clone().unwrap_or_default(),
        handle: me.handle.clone(),
        expires_at,
    };
    state::update_index(&ctx.dirs, |index| {
        index.forges.insert(forge.to_owned(), entry);
    })?;
    Ok(written)
}

/// Deletes the stored credential for `forge`.
pub fn forget(ctx: &Ctx, forge: &str) -> Result<()> {
    ctx.store.delete(forge)?;
    state::update_index(&ctx.dirs, |index| {
        index.forges.remove(forge);
    })
}

/// Fails with a message when `credential` is an access token, which cannot manage tokens.
pub fn require_session(credential: &ForgeCredential) -> Result<()> {
    if credential.is_token() {
        bail!(
            "signed in with an access token (`tt login --with-token`), which cannot manage access \
             tokens; a session is needed: run `tt login`"
        );
    }
    Ok(())
}
