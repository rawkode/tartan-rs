use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// A failed API request.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request is malformed.
    #[error("{0}")]
    BadRequest(String),
    /// The credentials are missing or wrong.
    #[error("unauthorized")]
    Unauthorized,
    /// The caller may not do this.
    #[error("{0}")]
    Forbidden(String),
    /// The named org, repository, package or installation does not exist.
    #[error("{0} not found")]
    NotFound(String),
    /// The request conflicts with what exists.
    #[error("{0}")]
    Conflict(String),
    /// An extension refused or failed the call; `kind` is its error kind.
    #[error("{kind}: {message}")]
    Extension {
        /// `denied`, `not_found`, `invalid`, `conflict`, `unavailable` or `internal`.
        kind: String,
        /// The extension's message.
        message: String,
    },
    /// The caller is over a rate limit or cap; it may try again after `retry_after` seconds.
    #[error("{message}")]
    TooManyRequests {
        /// What was limited.
        message: String,
        /// Seconds until a retry may succeed.
        retry_after: u64,
    },
    /// A server the forge asked on the caller's behalf (a PLC directory, a handle's DNS or
    /// HTTPS host, a PDS) could not be reached or answered badly.
    #[error("{0}")]
    Upstream(String),
    /// An actor or the Git host failed; the request can be retried.
    #[error("unavailable: {0}")]
    Unavailable(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Extension { kind, .. } => match kind.as_str() {
                "denied" => StatusCode::FORBIDDEN,
                "not_found" => StatusCode::NOT_FOUND,
                "invalid" => StatusCode::BAD_REQUEST,
                "conflict" => StatusCode::CONFLICT,
                "unavailable" => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_GATEWAY,
            },
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Unavailable(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                StatusCode::SERVICE_UNAVAILABLE
            }
        };
        let body = match &self {
            // Internal failures are logged, not echoed.
            Self::Unavailable(_) => json!({ "error": "temporarily unavailable" }),
            Self::Extension { kind, message } => {
                json!({ "error": message, "kind": kind })
            }
            other => json!({ "error": other.to_string() }),
        };
        let mut response = (status, Json(body)).into_response();
        if let Self::TooManyRequests { retry_after, .. } = &self
            && let Ok(value) = axum::http::HeaderValue::from_str(&retry_after.to_string())
        {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        response
    }
}
