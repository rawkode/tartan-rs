//! The PDS calls signing makes: `com.atproto.repo.applyWrites` and `deleteRecord` with the
//! DPoP-bound access token, and the public `com.atproto.repo.getRecord` a drain reconciles with.

use std::time::Duration;

use anyhow::Result;
use jacquard_oauth::dpop::wrap_request_with_dpop;
use serde_json::{Value, json};
use tartan_atproto::resolver::VerifyingResolver;
use tartan_atproto::signature::COLLECTION;

use crate::oauth::SigningSession;

/// The bound on one PDS write (design D7, *Signing*, step 4).
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// A record to create.
#[derive(Debug, Clone)]
pub struct Create {
    /// The record key.
    pub rkey: String,
    /// The record as JSON.
    pub value: Value,
}

/// How a write ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// Written; the answer's `results` (each `{uri, cid}`) in order.
    Written(Vec<(String, String)>),
    /// 429, with the `ratelimit-reset` time (Unix seconds) when given.
    RateLimited(Option<i64>),
    /// The connection failed or timed out: whether the PDS committed is unknown.
    Unreachable(String),
    /// 5xx: whether the PDS committed is unknown.
    ServerError(String),
    /// 401 or 403: the access token was refused.
    Unauthorized(String),
    /// Any other 4xx.
    Refused(String),
}

/// `applyWrites` holding exactly `creates`, bounded to `timeout`.
pub async fn apply_writes(
    resolver: &VerifyingResolver,
    session: &mut SigningSession,
    creates: &[Create],
    timeout: Duration,
) -> WriteOutcome {
    let writes: Vec<Value> = creates
        .iter()
        .map(|create| {
            json!({
                "$type": "com.atproto.repo.applyWrites#create",
                "collection": COLLECTION,
                "rkey": create.rkey,
                "value": create.value,
            })
        })
        .collect();
    let body = json!({"repo": session.did, "writes": writes});
    let response = match post(
        resolver,
        session,
        "com.atproto.repo.applyWrites",
        &body,
        timeout,
    )
    .await
    {
        Ok(response) => response,
        Err(reason) => return WriteOutcome::Unreachable(reason),
    };
    let status = response.status();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    if status.is_success() {
        let results = body
            .get("results")
            .and_then(Value::as_array)
            .map(|results| {
                results
                    .iter()
                    .map(|result| {
                        (
                            result["uri"].as_str().unwrap_or_default().to_owned(),
                            result["cid"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        return WriteOutcome::Written(results);
    }
    let reason = describe(status.as_u16(), &body);
    match status.as_u16() {
        429 => WriteOutcome::RateLimited(
            response
                .headers()
                .get("ratelimit-reset")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<i64>().ok()),
        ),
        401 | 403 => WriteOutcome::Unauthorized(reason),
        code if code >= 500 => WriteOutcome::ServerError(reason),
        _ => WriteOutcome::Refused(reason),
    }
}

/// `deleteRecord` of `rkey` in the signature collection.
pub async fn delete_record(
    resolver: &VerifyingResolver,
    session: &mut SigningSession,
    rkey: &str,
) -> WriteOutcome {
    let body = json!({"repo": session.did, "collection": COLLECTION, "rkey": rkey});
    let response = match post(
        resolver,
        session,
        "com.atproto.repo.deleteRecord",
        &body,
        WRITE_TIMEOUT,
    )
    .await
    {
        Ok(response) => response,
        Err(reason) => return WriteOutcome::Unreachable(reason),
    };
    let status = response.status();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    if status.is_success() {
        return WriteOutcome::Written(Vec::new());
    }
    let reason = describe(status.as_u16(), &body);
    match status.as_u16() {
        429 => WriteOutcome::RateLimited(None),
        401 | 403 => WriteOutcome::Unauthorized(reason),
        code if code >= 500 => WriteOutcome::ServerError(reason),
        _ => WriteOutcome::Refused(reason),
    }
}

fn describe(status: u16, body: &Value) -> String {
    let error = body["error"].as_str().unwrap_or_default();
    let message = body["message"].as_str().unwrap_or_default();
    match (error.is_empty(), message.is_empty()) {
        (true, _) => format!("the PDS answered {status}"),
        (false, true) => format!("the PDS answered {status} {error}"),
        (false, false) => format!("the PDS answered {status} {error}: {message}"),
    }
}

async fn post(
    resolver: &VerifyingResolver,
    session: &mut SigningSession,
    method: &str,
    body: &Value,
    timeout: Duration,
) -> Result<http::Response<Vec<u8>>, String> {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{}/xrpc/{method}", session.pds))
        .header("Authorization", format!("DPoP {}", session.access_token))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(body).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let host = host_of(&session.pds);
    match tokio::time::timeout(
        timeout,
        wrap_request_with_dpop(resolver, &mut session.dpop, false, request),
    )
    .await
    {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(format!("the PDS {host} could not be reached: {error}")),
        Err(_) => Err(format!("the PDS {host} did not answer within {timeout:?}")),
    }
}

/// The host of a URL, for messages.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            })
        })
        .unwrap_or_else(|| url.to_owned())
}

/// What `com.atproto.repo.getRecord` said about a queued record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Present with this CID.
    Present(String),
    /// `RecordNotFound`.
    Absent,
    /// The read failed.
    Failed(String),
}

/// Reads a signature record (public; no session).
pub async fn get_record(resolver: &VerifyingResolver, pds: &str, did: &str, rkey: &str) -> Lookup {
    let Ok(mut url) = reqwest::Url::parse(&format!("{pds}/xrpc/com.atproto.repo.getRecord")) else {
        return Lookup::Failed(format!("{pds} is not a PDS URL"));
    };
    url.query_pairs_mut()
        .append_pair("repo", did)
        .append_pair("collection", COLLECTION)
        .append_pair("rkey", rkey);
    let fetched = match resolver.fetch().get(url.as_str(), 256 * 1024).await {
        Ok(fetched) => fetched,
        Err(error) => return Lookup::Failed(error.to_string()),
    };
    let body: Value = serde_json::from_slice(&fetched.body).unwrap_or(Value::Null);
    if fetched.status.is_success() {
        return match body["cid"].as_str() {
            Some(cid) => Lookup::Present(cid.to_owned()),
            None => Lookup::Failed("the PDS answered a record without a cid".to_owned()),
        };
    }
    if fetched.status.as_u16() == 400 && body["error"].as_str() == Some("RecordNotFound") {
        return Lookup::Absent;
    }
    Lookup::Failed(describe(fetched.status.as_u16(), &body))
}
