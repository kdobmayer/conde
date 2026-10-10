//! In-process token buckets for the remote MCP surface (docs/REMOTE-MCP.md §3): `tools/call`
//! per grant, `send` and `react` per user, the OAuth endpoints per client IP. Each replica keeps
//! its own buckets, so with N replicas a client gets at most N times the rate.

use std::hash::Hash;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota};

/// The buckets' sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpLimits {
    /// `tools/call` per grant per minute, and the burst it may spend at once.
    pub tool_calls_per_min: u32,
    pub tool_calls_burst: u32,
    /// `send` and `react` per user per minute (burst: the same number).
    pub writes_per_min: u32,
    /// Requests to the OAuth endpoints per client IP per minute (burst: the same number).
    pub oauth_per_min: u32,
}

impl Default for McpLimits {
    fn default() -> McpLimits {
        McpLimits { tool_calls_per_min: 120, tool_calls_burst: 30, writes_per_min: 30, oauth_per_min: 60 }
    }
}

fn quota(per_min: u32, burst: u32) -> Quota {
    let n = |v: u32| NonZeroU32::new(v.max(1)).expect("at least one");
    Quota::per_minute(n(per_min)).allow_burst(n(burst))
}

/// A keyed bucket set that forgets idle keys now and then.
struct Bucket<K: Hash + Eq + Clone> {
    limiter: DefaultKeyedRateLimiter<K>,
    checks: AtomicU64,
}

impl<K: Hash + Eq + Clone> Bucket<K> {
    fn new(quota: Quota) -> Bucket<K> {
        Bucket { limiter: DefaultKeyedRateLimiter::dashmap(quota), checks: AtomicU64::new(0) }
    }

    /// Ok, or the whole seconds until the next request would pass (at least 1).
    fn check(&self, key: &K) -> Result<(), u64> {
        if self.checks.fetch_add(1, Ordering::Relaxed) % 4096 == 4095 {
            self.limiter.retain_recent();
        }
        self.limiter.check_key(key).map_err(|not_until| {
            let wait = not_until.wait_time_from(DefaultClock::default().now());
            (wait.as_secs() + u64::from(wait.subsec_nanos() > 0)).max(1)
        })
    }
}

pub(crate) struct Limiters {
    tools: Bucket<String>,
    writes: Bucket<i64>,
    oauth: Bucket<IpAddr>,
}

impl Limiters {
    pub(crate) fn new(l: McpLimits) -> Limiters {
        Limiters {
            tools: Bucket::new(quota(l.tool_calls_per_min, l.tool_calls_burst)),
            writes: Bucket::new(quota(l.writes_per_min, l.writes_per_min)),
            oauth: Bucket::new(quota(l.oauth_per_min, l.oauth_per_min)),
        }
    }

    pub(crate) fn tool_call(&self, grant_id: &str) -> Result<(), u64> {
        self.tools.check(&grant_id.to_string())
    }

    pub(crate) fn write(&self, user_id: i64) -> Result<(), u64> {
        self.writes.check(&user_id)
    }

    pub(crate) fn oauth(&self, ip: IpAddr) -> Result<(), u64> {
        self.oauth.check(&ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_runs_out_and_names_the_wait() {
        let l = Limiters::new(McpLimits { tool_calls_per_min: 60, tool_calls_burst: 2, writes_per_min: 1, oauth_per_min: 1 });
        assert!(l.tool_call("g1").is_ok());
        assert!(l.tool_call("g1").is_ok());
        let wait = l.tool_call("g1").unwrap_err();
        assert!((1..=1).contains(&wait), "one call per second refills: {wait}");
        assert!(l.tool_call("g2").is_ok(), "per grant");
        assert!(l.write(7).is_ok());
        assert!((1..=60).contains(&l.write(7).unwrap_err()));
        assert!(l.write(8).is_ok(), "per user");
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(l.oauth(ip).is_ok());
        assert!(l.oauth(ip).is_err());
    }
}

/// The client's address for the per-IP bucket: the peer, or, when the peer is a private,
/// carrier-grade NAT or loopback address (the ingress in front), the last `X-Forwarded-For`
/// entry, which that proxy appended (earlier entries are the client's to forge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientIp(pub IpAddr);

fn internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            // 100.64.0.0/10 (RFC 6598): Container Apps' ingress may connect from it.
            let shared = v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64;
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || shared
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

pub(crate) fn client_ip(parts: &axum::http::request::Parts) -> IpAddr {
    let peer = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    if !internal(peer) {
        return peer;
    }
    parts
        .headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|e| e.trim().parse::<IpAddr>().ok())
        .next_back()
        .unwrap_or(peer)
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<ClientIp, Self::Rejection> {
        Ok(ClientIp(client_ip(parts)))
    }
}

#[cfg(test)]
mod ip_tests {
    use super::*;

    fn parts(peer: &str, xff: &[&str]) -> axum::http::request::Parts {
        let mut b = axum::http::Request::builder();
        for v in xff {
            b = b.header("x-forwarded-for", *v);
        }
        let (mut parts, _) = b.body(()).unwrap().into_parts();
        parts.extensions.insert(axum::extract::ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
        parts
    }

    #[test]
    fn the_proxy_appended_address_counts_only_behind_a_private_peer() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(client_ip(&parts("10.0.0.5:1", &["6.6.6.6, 203.0.113.9"])), ip("203.0.113.9"));
        assert_eq!(client_ip(&parts("10.0.0.5:1", &["6.6.6.6", "203.0.113.9"])), ip("203.0.113.9"));
        assert_eq!(client_ip(&parts("10.0.0.5:1", &[])), ip("10.0.0.5"));
        for ingress in ["100.64.0.1:1", "100.100.12.7:1", "100.127.255.254:1"] {
            assert_eq!(client_ip(&parts(ingress, &["203.0.113.9"])), ip("203.0.113.9"), "{ingress}: shared address space");
        }
        for public in ["100.63.255.255:1", "100.128.0.1:1"] {
            assert_eq!(client_ip(&parts(public, &["203.0.113.9"])), ip(&public[..public.len() - 2]), "{public}");
        }
        assert_eq!(client_ip(&parts("198.51.100.1:1", &["203.0.113.9"])), ip("198.51.100.1"), "a public peer is the client");
    }
}
