//! Small helpers shared by the devnet's services: time, random tokens and XRPC errors.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;

/// Unix time in milliseconds.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Unix time in microseconds.
pub(crate) fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_default()
}

/// `count` bytes from the operating system's random number generator, in unpadded base64url.
pub(crate) fn random_token(count: usize) -> String {
    let mut bytes = vec![0u8; count];
    // The OS RNG failing is unrecoverable for a server that hands out secrets.
    getrandom::fill(&mut bytes).expect("the operating system's random number generator failed");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Formats Unix milliseconds as RFC 3339 in UTC with millisecond precision, e.g.
/// `2026-11-09T12:00:00.000Z`.
pub(crate) fn rfc3339(ms: u64) -> String {
    let seconds = ms / 1000;
    let millis = ms % 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    )
}

/// Converts days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// An XRPC error answer: `{"error": …, "message": …}`.
pub(crate) fn xrpc_error(status: StatusCode, error: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error": error, "message": message.into()})),
    )
        .into_response()
}

/// Parses an `application/x-www-form-urlencoded` body or query string into ordered pairs.
pub(crate) fn form_pairs(input: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(input)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// The first value of `name` in `pairs`.
pub(crate) fn first<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Every value of `name` in `pairs`.
pub(crate) fn all<'a>(pairs: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    pairs
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_762_689_600_123), "2025-11-09T12:00:00.123Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn tokens_are_random() {
        assert_ne!(random_token(16), random_token(16));
        assert_eq!(random_token(32).len(), 43);
    }
}
