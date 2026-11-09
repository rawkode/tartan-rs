//! The client address rate limits and caps key on (design D4, *Client address*), and a small
//! token-bucket rate limiter.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use ipnet::IpNet;
use tartan_forge::Clock;

/// The client address of a request: the peer, or, when the peer is a trusted proxy, the
/// right-most `X-Forwarded-For` address that is not one. IPv6 addresses are bucketed by /64.
pub fn client_address<B>(request: &Request<B>, trusted: &[IpNet]) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    bucket(client_ip(peer, request.headers(), trusted))
}

/// The client IP address (design D4): `X-Forwarded-For` counts only when `peer` is trusted.
pub fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpNet]) -> Option<IpAddr> {
    let peer = peer?;
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|net| net.contains(ip));
    if !is_trusted(&peer) {
        return Some(peer);
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|entry| entry.trim().parse::<IpAddr>().ok())
        .collect();
    forwarded
        .iter()
        .rev()
        .find(|ip| !is_trusted(ip))
        .or_else(|| forwarded.first())
        .copied()
        .or(Some(peer))
}

/// The bucket an address counts in: the address, or its /64 for IPv6.
pub fn bucket(ip: Option<IpAddr>) -> String {
    match ip {
        Some(IpAddr::V4(ip)) => ip.to_string(),
        Some(IpAddr::V6(ip)) => match ip.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let segments = ip.segments();
                format!(
                    "{:x}:{:x}:{:x}:{:x}::/64",
                    segments[0], segments[1], segments[2], segments[3]
                )
            }
        },
        None => "unknown".to_owned(),
    }
}

/// A token bucket per key: `burst` requests at once, refilled at one per `every_ms`.
pub struct RateLimiter {
    burst: f64,
    every_ms: f64,
    clock: Arc<dyn Clock>,
    buckets: Mutex<HashMap<String, (f64, i64)>>,
}

impl RateLimiter {
    /// A limiter allowing `burst` at once and one more every `every_ms` milliseconds.
    pub fn new(burst: u32, every_ms: u64, clock: Arc<dyn Clock>) -> Self {
        Self {
            burst: f64::from(burst),
            every_ms: every_ms as f64,
            clock,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one request for `key`: `Ok` when allowed, else the seconds until the next one.
    pub fn take(&self, key: &str) -> Result<(), u64> {
        let now = self.clock.now_ms();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() > 100_000 {
            // Full buckets carry no information; drop them so memory stays bounded.
            let (burst, every) = (self.burst, self.every_ms);
            buckets.retain(|_, (tokens, at)| *tokens + (now - *at) as f64 / every < burst);
        }
        let (tokens, at) = buckets.entry(key.to_owned()).or_insert((self.burst, now));
        let refilled = (*tokens + (now - *at).max(0) as f64 / self.every_ms).min(self.burst);
        *at = now;
        if refilled >= 1.0 {
            *tokens = refilled - 1.0;
            Ok(())
        } else {
            *tokens = refilled;
            Err(((1.0 - refilled) * self.every_ms / 1000.0).ceil().max(1.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct Fixed(AtomicI64);

    impl Clock for Fixed {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }

        fn offset_ms(&self) -> i64 {
            0
        }
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-for", value.parse().expect("header"));
        }
        headers
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("ip")
    }

    #[test]
    fn forwarded_addresses_count_only_from_trusted_proxies() {
        let trusted: Vec<IpNet> = vec!["127.0.0.1/32".parse().expect("net")];
        // An untrusted peer's header is ignored.
        assert_eq!(
            client_ip(
                Some(ip("203.0.113.9")),
                &headers(&["198.51.100.1"]),
                &trusted
            ),
            Some(ip("203.0.113.9"))
        );
        // Behind a trusted proxy, the right-most untrusted address.
        assert_eq!(
            client_ip(
                Some(ip("127.0.0.1")),
                &headers(&["198.51.100.1, 203.0.113.7", "127.0.0.1"]),
                &trusted
            ),
            Some(ip("203.0.113.7"))
        );
        // Only trusted addresses: the left-most valid one.
        assert_eq!(
            client_ip(Some(ip("127.0.0.1")), &headers(&["127.0.0.1"]), &trusted),
            Some(ip("127.0.0.1"))
        );
        // No header, or a malformed one: the peer.
        assert_eq!(
            client_ip(Some(ip("127.0.0.1")), &headers(&["nonsense"]), &trusted),
            Some(ip("127.0.0.1"))
        );
        assert_eq!(
            bucket(Some(ip("2001:db8:1:2:3:4:5:6"))),
            "2001:db8:1:2::/64"
        );
        assert_eq!(bucket(Some(ip("::ffff:10.0.0.5"))), "10.0.0.5");
    }

    #[test]
    fn buckets_refill() {
        let clock = Arc::new(Fixed::default());
        let limiter = RateLimiter::new(2, 1000, clock.clone());
        assert!(limiter.take("a").is_ok());
        assert!(limiter.take("a").is_ok());
        assert_eq!(limiter.take("a"), Err(1));
        assert!(limiter.take("b").is_ok(), "buckets are per key");
        clock.0.store(1000, Ordering::SeqCst);
        assert!(limiter.take("a").is_ok());
        assert!(limiter.take("a").is_err());
    }
}
