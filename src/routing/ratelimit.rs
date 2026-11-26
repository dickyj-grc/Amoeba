//! In-memory rate limits: failed sign-ins per account and per client address,
//! and requests per caller per service. Counts live in this gateway process
//! only, which matches one gateway per machine; they reset on restart.

use axum::http::HeaderMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failed sign-ins allowed per account within [`LOGIN_WINDOW`].
pub const LOGIN_FAILURES_PER_ACCOUNT: usize = 5;
/// Failed sign-ins allowed per client address within [`LOGIN_WINDOW`].
pub const LOGIN_FAILURES_PER_ADDRESS: usize = 20;
pub const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Requests per caller per service per minute when neither the service nor
/// `AMOEBA_RATE_LIMIT_PER_MINUTE` says otherwise.
pub const DEFAULT_PER_MINUTE: u32 = 600;

/// Requests per caller per service per minute: `AMOEBA_RATE_LIMIT_PER_MINUTE`
/// or [`DEFAULT_PER_MINUTE`]; `0` turns the limit off.
pub fn default_per_minute() -> u32 {
    std::env::var("AMOEBA_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_PER_MINUTE)
}

/// The client's address for limits: the peer, or, when the peer is a proxy
/// on this machine or a private network (Caddy, a load balancer), the first
/// `X-Forwarded-For` entry it reports. A public peer's own claim is ignored,
/// so a caller cannot dodge the limit by inventing addresses.
pub fn client_address(peer: Option<IpAddr>, headers: &HeaderMap) -> Option<String> {
    let peer = peer?;
    let trusted_proxy = match peer {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
    };
    if trusted_proxy
        && let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .and_then(|v| v.parse::<IpAddr>().ok())
    {
        return Some(forwarded.to_string());
    }
    Some(peer.to_string())
}

#[derive(Default)]
pub struct Limits {
    /// key -> start of the current one-minute window and requests in it.
    windows: Mutex<HashMap<String, (Instant, u32)>>,
    /// key -> times of recent failed sign-ins.
    login_failures: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Limits {
    /// Counts one request for `key`. `Err(seconds)` when it is over
    /// `per_minute`, with how long until the window resets.
    pub fn allow_request(&self, key: &str, per_minute: u32) -> Result<(), u64> {
        self.allow_request_at(key, per_minute, Instant::now())
    }

    fn allow_request_at(&self, key: &str, per_minute: u32, now: Instant) -> Result<(), u64> {
        if per_minute == 0 {
            return Ok(());
        }
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if windows.len() > 100_000 {
            windows.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        }
        let entry = windows.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        if entry.1 >= per_minute {
            let reset = Duration::from_secs(60).saturating_sub(now.duration_since(entry.0));
            return Err(reset.as_secs().max(1));
        }
        entry.1 += 1;
        Ok(())
    }

    /// `Err(seconds)` while `keys` (an account, an address) have too many
    /// recent failed sign-ins, with how long until the oldest expires.
    pub fn login_blocked(&self, keys: &[(&str, usize)]) -> Result<(), u64> {
        self.login_blocked_at(keys, Instant::now())
    }

    fn login_blocked_at(&self, keys: &[(&str, usize)], now: Instant) -> Result<(), u64> {
        let mut failures = self
            .login_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (key, limit) in keys {
            let Some(times) = failures.get_mut(*key) else {
                continue;
            };
            times.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
            if times.len() >= *limit {
                let oldest = times[0];
                return Err(LOGIN_WINDOW
                    .saturating_sub(now.duration_since(oldest))
                    .as_secs()
                    .max(1));
            }
        }
        Ok(())
    }

    pub fn record_login_failure(&self, keys: &[&str]) {
        self.record_login_failure_at(keys, Instant::now());
    }

    fn record_login_failure_at(&self, keys: &[&str], now: Instant) {
        let mut failures = self
            .login_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if failures.len() > 100_000 {
            failures.retain(|_, times| times.iter().any(|t| now.duration_since(*t) < LOGIN_WINDOW));
        }
        for key in keys {
            failures.entry(key.to_string()).or_default().push(now);
        }
    }

    /// A successful sign-in clears the account's failures (not the address's).
    pub fn clear_login_failures(&self, key: &str) {
        self.login_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_limited_per_key_per_minute() {
        let limits = Limits::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limits.allow_request_at("svc:alice", 3, t0).is_ok());
        }
        let wait = limits.allow_request_at("svc:alice", 3, t0).unwrap_err();
        assert!((1..=60).contains(&wait));
        // Another caller, or the same one a minute later, is not affected.
        assert!(limits.allow_request_at("svc:bob", 3, t0).is_ok());
        assert!(
            limits
                .allow_request_at("svc:alice", 3, t0 + Duration::from_secs(61))
                .is_ok()
        );
        // 0 means no limit.
        for _ in 0..1000 {
            assert!(limits.allow_request_at("svc:carol", 0, t0).is_ok());
        }
    }

    #[test]
    fn only_a_local_proxy_may_name_the_client() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.9, 10.0.0.2".parse().unwrap());
        let proxy: IpAddr = "172.18.0.3".parse().unwrap();
        let public: IpAddr = "198.51.100.4".parse().unwrap();
        assert_eq!(
            client_address(Some(proxy), &headers).unwrap(),
            "203.0.113.9"
        );
        assert_eq!(
            client_address(Some(public), &headers).unwrap(),
            "198.51.100.4"
        );
        assert_eq!(client_address(None, &headers), None);
    }

    #[test]
    fn failed_sign_ins_block_the_account_for_a_while() {
        let limits = Limits::default();
        let t0 = Instant::now();
        let keys = [("user:alice", 2), ("ip:1.2.3.4", 10)];
        assert!(limits.login_blocked_at(&keys, t0).is_ok());
        limits.record_login_failure_at(&["user:alice", "ip:1.2.3.4"], t0);
        limits.record_login_failure_at(&["user:alice", "ip:1.2.3.4"], t0);
        assert!(limits.login_blocked_at(&keys, t0).is_err());
        // The window passes.
        assert!(
            limits
                .login_blocked_at(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(1))
                .is_ok()
        );
        // Success clears the account.
        limits.record_login_failure_at(&["user:alice"], t0);
        limits.record_login_failure_at(&["user:alice"], t0);
        limits.clear_login_failures("user:alice");
        assert!(limits.login_blocked_at(&keys, t0).is_ok());
    }
}
