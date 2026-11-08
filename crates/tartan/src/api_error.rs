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
    /// The named org or repository does not exist.
    #[error("{0} not found")]
    NotFound(String),
    /// An actor or the Git host failed; the request can be retried.
    #[error("unavailable: {0}")]
    Unavailable(#[from] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Unavailable(error) => {
                tracing::error!(error = format!("{error:#}"), "request failed");
                StatusCode::SERVICE_UNAVAILABLE
            }
        };
        let message = match &self {
            // Internal failures are logged, not echoed.
            Self::Unavailable(_) => "temporarily unavailable".to_owned(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
