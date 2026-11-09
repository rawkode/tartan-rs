//! `tt admin` (design D16, *`tt admin` and the identity actor*): operator commands that need no
//! signed-in person.
//!
//! `tt admin token …` reaches the identity actor `identity` keyed `[TARTAN_FORGE_ID]` (default
//! `forge`, read as the `tartan` binary reads it) through Rivet (`RIVET_ENDPOINT`,
//! `RIVET_TOKEN`, `RIVET_NAMESPACE`, `RIVET_POOL`), with `rivetkit-client`'s untyped action
//! call. `tt` reads no deployment configuration: the actor refuses a DID that is not one of the
//! running forge's owners. `tt admin oauth keygen` prints a fresh ES256 private JWKS.

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rivetkit_client::{Client, ClientConfig, GetOrCreateOptions};
use serde_json::{Value, json};
use tartan_identity_api::{
    ACTOR_NAME, AdminCreateToken, AdminListTokens, AdminRevokeToken, IdentityAction, TokenRefusal,
    TokenRequest, TokenScope, identity_key,
};

use crate::account::{print_token, print_tokens};
use crate::cli::{AdminCommand, AdminOauthCommand, AdminTokenCommand, TokenCreateArgs};

/// `tt admin …`.
pub async fn run(command: AdminCommand) -> Result<i32> {
    match command {
        AdminCommand::Oauth(oauth) => match oauth.command {
            AdminOauthCommand::Keygen => {
                println!("{}", keygen()?);
                Ok(0)
            }
        },
        AdminCommand::Token(token) => token_command(token.command).await,
    }
}

/// Wraps a serialised action argument the way rivetkit's typed `call` does: an array stays
/// positional, null is no argument, anything else is one argument.
pub fn positional(value: Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values,
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

fn client() -> Client {
    let endpoint = std::env::var("RIVET_ENDPOINT")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:6420".to_owned());
    let local = endpoint.contains("127.0.0.1") || endpoint.contains("localhost");
    let token = std::env::var("RIVET_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| local.then(|| "default".to_owned()));
    let namespace = std::env::var("RIVET_NAMESPACE").unwrap_or_else(|_| "default".to_owned());
    let pool = std::env::var("RIVET_POOL").unwrap_or_else(|_| "default".to_owned());
    Client::new(
        ClientConfig::new(endpoint)
            .token_opt(token)
            .namespace(namespace)
            .pool_name(pool),
    )
}

/// Sends `message` to the identity actor of `TARTAN_FORGE_ID`.
pub async fn call<M: IdentityAction>(message: &M) -> Result<M::Output> {
    let forge_id = std::env::var("TARTAN_FORGE_ID").unwrap_or_else(|_| "forge".to_owned());
    let client = client();
    let handle = client
        .get_or_create(
            ACTOR_NAME,
            identity_key(&forge_id),
            GetOrCreateOptions::default(),
        )
        .map_err(|error| anyhow!("reaching the identity actor: {error}"))?;
    let args = positional(serde_json::to_value(message).context("encoding the action")?);
    let answer = handle.action(M::NAME, args).await.map_err(|error| {
        anyhow!(
            "the identity actor of forge {forge_id:?} could not be reached through Rivet \
             (RIVET_ENDPOINT, RIVET_TOKEN, RIVET_NAMESPACE): {error}"
        )
    })?;
    serde_json::from_value(answer).context("decoding the identity actor's answer")
}

fn refusal(error: TokenRefusal) -> anyhow::Error {
    match error {
        TokenRefusal::NotOwner { did } => anyhow!(
            "{did} is not one of this forge's owners; the identity actor refused it and created \
             nothing"
        ),
        other => anyhow!("{other}"),
    }
}

fn token_request(create: &TokenCreateArgs) -> Result<TokenRequest> {
    let scopes = create
        .scopes
        .iter()
        .map(|scope| match scope.trim() {
            "read" => Ok(TokenScope::Read),
            "write" => Ok(TokenScope::Write),
            "admin" => Ok(TokenScope::Admin),
            other => bail!("unknown scope {other:?} (read, write or admin)"),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TokenRequest {
        name: create.name.clone(),
        scopes,
        repos: create.repos.clone(),
        expires_in_days: create.expires_in_days,
    })
}

async fn token_command(command: AdminTokenCommand) -> Result<i32> {
    match command {
        AdminTokenCommand::Create { did, token } => {
            let request = token_request(&token)?;
            let created = call(&AdminCreateToken { did, request })
                .await?
                .map_err(refusal)?;
            print_token(&serde_json::to_value(created).context("encoding the token")?);
        }
        AdminTokenCommand::List { did } => {
            let tokens = call(&AdminListTokens { did }).await?.map_err(refusal)?;
            print_tokens(&serde_json::to_value(tokens).context("encoding the tokens")?);
        }
        AdminTokenCommand::Revoke { did, id } => {
            call(&AdminRevokeToken {
                did,
                id: id.clone(),
            })
            .await?
            .map_err(refusal)?;
            println!("Revoked token {id}.");
        }
    }
    Ok(0)
}

/// A fresh ES256 private JWKS with one key.
pub fn keygen() -> Result<String> {
    let secret = loop {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| anyhow!("the operating system's RNG failed"))?;
        if let Ok(secret) = p256::SecretKey::from_slice(&bytes) {
            break secret;
        }
    };
    let point =
        p256::elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&secret.public_key(), false);
    let (Some(x), Some(y)) = (point.x(), point.y()) else {
        bail!("the generated key has no affine coordinates");
    };
    let mut kid = [0u8; 8];
    getrandom::fill(&mut kid).map_err(|_| anyhow!("the operating system's RNG failed"))?;
    let jwks = json!({
        "keys": [{
            "kty": "EC",
            "crv": "P-256",
            "alg": "ES256",
            "use": "sig",
            "kid": hex::encode(kid),
            "x": URL_SAFE_NO_PAD.encode(x),
            "y": URL_SAFE_NO_PAD.encode(y),
            "d": URL_SAFE_NO_PAD.encode(secret.to_bytes()),
        }]
    });
    serde_json::to_string_pretty(&jwks).context("encoding the JWKS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_positional_like_rivetkit() {
        assert_eq!(positional(json!({"did": "x"})), vec![json!({"did": "x"})]);
        assert_eq!(positional(Value::Null), Vec::<Value>::new());
        assert_eq!(positional(json!([1, 2])), vec![json!(1), json!(2)]);
    }

    #[test]
    fn keygen_writes_a_private_p256_jwks() {
        let jwks: Value = serde_json::from_str(&keygen().expect("keygen")).expect("json");
        let key = &jwks["keys"][0];
        assert_eq!(key["kty"], "EC");
        assert_eq!(key["crv"], "P-256");
        assert_eq!(key["d"].as_str().map(str::len), Some(43));
        assert_eq!(key["x"].as_str().map(str::len), Some(43));
        assert!(key["kid"].as_str().is_some_and(|kid| !kid.is_empty()));
    }
}
