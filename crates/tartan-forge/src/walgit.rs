//! [walgit](https://github.com/tobi/walgit) as the [`GitHost`]: it stores repositories in the
//! object store, serves Git over smart HTTP and posts ref events to Tartan's webhook.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::GitHost;

/// A walgit server reached over HTTP.
#[derive(Clone, Debug)]
pub struct WalgitClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl WalgitClient {
    /// The server at `base_url`, called with the bearer `token` when there is one.
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl GitHost for WalgitClient {
    async fn create_repo(&self, owner: &str, name: &str) -> Result<()> {
        // walgit's `PUT /{owner}/{repo}/api` creates the repository (201) or finds it (200).
        let url = format!("{}/{owner}/{name}/api", self.base_url);
        let mut request = self.http.put(&url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.with_context(|| format!("PUT {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!(
                "PUT {url}: {status}: {}",
                body.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }

    fn clone_url(&self, owner: &str, name: &str) -> String {
        format!("{}/{owner}/{name}.git", self.base_url)
    }
}

/// One ref event as walgit's webhook sends it (walgit `docs/EVENTS.md`). Its dedup key is
/// `(repo, walgit.seq, ref_name)` and events of one repository arrive in `seq` order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefEvent {
    /// `create`, `update` or `delete`.
    pub action: String,
    /// `branch`, `tag` or empty.
    #[serde(default)]
    pub ref_type: String,
    /// The full ref name.
    pub ref_name: String,
    /// The old object id; all zeros on create.
    pub old: String,
    /// The new object id; all zeros on delete.
    pub new: String,
    /// The authenticated principal that pushed.
    #[serde(default)]
    pub pusher: String,
    /// `owner/name`.
    pub repo: String,
    /// walgit's log position of the change.
    #[serde(rename = "_walgit")]
    pub walgit: WalgitMeta,
}

/// walgit's bookkeeping on a [`RefEvent`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalgitMeta {
    /// The repository's log sequence number, as a decimal string.
    pub seq: String,
    /// `push` or `ref_update`.
    #[serde(default)]
    pub entry_kind: String,
}

/// Whether `signature`, an `X-Walgit-Signature` header of the form `sha256=<hex>`, is the
/// HMAC-SHA256 of `body` under `secret`. The comparison takes constant time.
pub fn verify_signature(secret: &[u8], body: &[u8], signature: &str) -> bool {
    let Some(hex_digest) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_digest) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("any key length");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verifies_walgit_signatures() {
        let body = br#"[{"action":"create"}]"#;
        let signature = sign(b"secret", body);
        assert!(verify_signature(b"secret", body, &signature));
        assert!(!verify_signature(b"other", body, &signature));
        assert!(!verify_signature(b"secret", b"tampered", &signature));
        assert!(!verify_signature(b"secret", body, "md5=00"));
    }

    #[test]
    fn decodes_the_documented_event_shape() {
        let json = r#"{
            "action": "update", "ref_type": "branch", "ref_name": "refs/heads/main",
            "old": "48a0637", "new": "cb38da1", "pusher": "alice@example.com",
            "correlation_id": "d1f9", "repo": "acme/monorepo",
            "_walgit": {"schema_version": 1, "seq": "42", "entry_kind": "push", "request_id": "d1"}
        }"#;
        let event: RefEvent = serde_json::from_str(json).expect("decode");
        assert_eq!(event.walgit.seq, "42");
        assert_eq!(event.repo, "acme/monorepo");
    }
}
