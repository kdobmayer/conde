//! In-process token buckets for the `conde login` redeem: per client IP, and pasted-code attempts
//! per account. Each replica keeps its own buckets, so with N replicas a client gets at most N
//! times the rate.

use std::hash::Hash;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota};

/// The buckets' sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimits {
    /// `POST /auth/cli/redeem` per client IP per minute (burst: the same number).
    pub cli_redeem_per_min: u32,
    /// Pasted-code attempts per account per hour (burst: the same number). The per-code budget
    /// (5 tries) restarts with every newly issued code; this one does not.
    pub cli_guesses_per_hour: u32,
}

impl Default for RateLimits {
    fn default() -> RateLimits {
        RateLimits {
            cli_redeem_per_min: 30,
            cli_guesses_per_hour: 15,
        }
    }
}

fn n(v: u32) -> NonZeroU32 {
    NonZeroU32::new(v.max(1)).expect("at least one")
}

fn per_minute(n_per_min: u32) -> Quota {
    Quota::per_minute(n(n_per_min)).allow_burst(n(n_per_min))
}

/// A keyed bucket set that forgets idle keys now and then.
struct Bucket<K: Hash + Eq + Clone> {
    limiter: DefaultKeyedRateLimiter<K>,
    checks: AtomicU64,
}

impl<K: Hash + Eq + Clone> Bucket<K> {
    fn new(quota: Quota) -> Bucket<K> {
        Bucket {
            limiter: DefaultKeyedRateLimiter::dashmap(quota),
            checks: AtomicU64::new(0),
        }
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
    cli_redeem: Bucket<IpAddr>,
    cli_guesses: Bucket<i64>,
}

impl Limiters {
    pub(crate) fn new(l: RateLimits) -> Limiters {
        Limiters {
            cli_redeem: Bucket::new(per_minute(l.cli_redeem_per_min)),
            cli_guesses: Bucket::new(
                Quota::per_hour(n(l.cli_guesses_per_hour)).allow_burst(n(l.cli_guesses_per_hour)),
            ),
        }
    }

    pub(crate) fn cli_redeem(&self, ip: IpAddr) -> Result<(), u64> {
        self.cli_redeem.check(&ip)
    }

    pub(crate) fn cli_guess(&self, user_id: i64) -> Result<(), u64> {
        self.cli_guesses.check(&user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_runs_out_and_names_the_wait() {
        let l = Limiters::new(RateLimits {
            cli_redeem_per_min: 1,
            cli_guesses_per_hour: 2,
        });
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(l.cli_redeem(ip).is_ok());
        assert!(
            (1..=60).contains(&l.cli_redeem(ip).unwrap_err()),
            "per minute"
        );
        assert!(
            l.cli_redeem("10.0.0.2".parse().unwrap()).is_ok(),
            "per address"
        );
        assert!(l.cli_guess(7).is_ok() && l.cli_guess(7).is_ok());
        assert!(
            (1..=3600).contains(&l.cli_guess(7).unwrap_err()),
            "per hour"
        );
        assert!(l.cli_guess(8).is_ok(), "per account");
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
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || shared
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

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<ClientIp, Self::Rejection> {
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
        parts.extensions.insert(axum::extract::ConnectInfo(
            peer.parse::<std::net::SocketAddr>().unwrap(),
        ));
        parts
    }

    #[test]
    fn the_proxy_appended_address_counts_only_behind_a_private_peer() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(
            client_ip(&parts("10.0.0.5:1", &["6.6.6.6, 203.0.113.9"])),
            ip("203.0.113.9")
        );
        assert_eq!(
            client_ip(&parts("10.0.0.5:1", &["6.6.6.6", "203.0.113.9"])),
            ip("203.0.113.9")
        );
        assert_eq!(client_ip(&parts("10.0.0.5:1", &[])), ip("10.0.0.5"));
        for ingress in ["100.64.0.1:1", "100.100.12.7:1", "100.127.255.254:1"] {
            assert_eq!(
                client_ip(&parts(ingress, &["203.0.113.9"])),
                ip("203.0.113.9"),
                "{ingress}: shared address space"
            );
        }
        for public in ["100.63.255.255:1", "100.128.0.1:1"] {
            assert_eq!(
                client_ip(&parts(public, &["203.0.113.9"])),
                ip(&public[..public.len() - 2]),
                "{public}"
            );
        }
        assert_eq!(
            client_ip(&parts("198.51.100.1:1", &["203.0.113.9"])),
            ip("198.51.100.1"),
            "a public peer is the client"
        );
    }
}
