//! Container start/stop, active connection counting, cooldown timers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Runtime tracker for active connections and activity timestamps.
pub struct ServiceRuntimeState {
    pub last_accessed_unix: AtomicU64,
    pub active_connections: AtomicU64,
    /// Whether this service has ever received a real request. `new()` sets
    /// `last_accessed_unix` to the construction time, so without this flag a
    /// never-touched service would look "recently active" to
    /// `should_scale_to_zero`/`is_occupying_capacity` for a full cooldown
    /// window after startup or hot-reload.
    pub has_activated: AtomicBool,
    /// The upstream host a driver dynamically resolved at last cold-boot
    /// (e.g. `AppleContainerDriver`, whose containers get a fresh IP on every
    /// start with no stable DNS name). `None` for drivers that never call
    /// `set_resolved_host` (Docker/Compose), in which case the proxy falls
    /// back to the static `placement.ip`/`primary_service` config value.
    resolved_host: RwLock<Option<String>>,
}

impl ServiceRuntimeState {
    pub fn new() -> Self {
        Self {
            last_accessed_unix: AtomicU64::new(now_unix()),
            active_connections: AtomicU64::new(0),
            has_activated: AtomicBool::new(false),
            resolved_host: RwLock::new(None),
        }
    }

    pub fn touch(&self) {
        self.last_accessed_unix.store(now_unix(), Ordering::Relaxed);
        self.has_activated.store(true, Ordering::Relaxed);
    }

    pub fn resolved_host(&self) -> Option<String> {
        self.resolved_host.read().unwrap().clone()
    }

    pub fn set_resolved_host(&self, host: String) {
        *self.resolved_host.write().unwrap() = Some(host);
    }
}

impl Default for ServiceRuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Decides whether an idle service should be scaled down to zero.
pub fn should_scale_to_zero(
    active_connections: u64,
    last_accessed_unix: u64,
    now_unix: u64,
    cooldown_seconds: u64,
) -> bool {
    active_connections == 0 && now_unix.saturating_sub(last_accessed_unix) >= cooldown_seconds
}

/// Whether a service is currently occupying its machine's capacity budget:
/// mid-request, recently accessed within its cooldown window, or always-on
/// (no cooldown configured, i.e. a warm/stateful service the reaper never
/// manages). A service that has never activated never counts, regardless of
/// how recent its constructor-time `last_accessed_unix` looks.
pub fn is_occupying_capacity(
    cooldown_seconds: Option<u64>,
    has_activated: bool,
    active_connections: u64,
    last_accessed_unix: u64,
    now_unix: u64,
) -> bool {
    match cooldown_seconds {
        None => true,
        Some(cooldown) => {
            has_activated
                && !should_scale_to_zero(active_connections, last_accessed_unix, now_unix, cooldown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_starts_with_no_active_connections() {
        let state = ServiceRuntimeState::new();
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn resolved_host_starts_none() {
        let state = ServiceRuntimeState::new();
        assert_eq!(state.resolved_host(), None);
    }

    #[test]
    fn set_resolved_host_is_visible_via_resolved_host() {
        let state = ServiceRuntimeState::new();
        state.set_resolved_host("192.168.64.3".to_string());
        assert_eq!(state.resolved_host(), Some("192.168.64.3".to_string()));
    }

    #[test]
    fn touch_updates_last_accessed_forward_in_time() {
        let state = ServiceRuntimeState::new();
        state.last_accessed_unix.store(0, Ordering::Relaxed);
        state.touch();
        assert!(state.last_accessed_unix.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn scales_to_zero_when_idle_past_cooldown() {
        assert!(should_scale_to_zero(0, 100, 200, 60));
    }

    #[test]
    fn does_not_scale_to_zero_with_active_connections() {
        assert!(!should_scale_to_zero(1, 100, 200, 60));
    }

    #[test]
    fn does_not_scale_to_zero_before_cooldown_elapsed() {
        assert!(!should_scale_to_zero(0, 190, 200, 60));
    }

    #[test]
    fn scale_check_does_not_underflow_when_clock_looks_stale() {
        // last_accessed_unix newer than now_unix should not panic/underflow.
        assert!(!should_scale_to_zero(0, 500, 200, 60));
    }

    #[test]
    fn new_state_starts_not_activated() {
        let state = ServiceRuntimeState::new();
        assert!(!state.has_activated.load(Ordering::Relaxed));
    }

    #[test]
    fn touch_marks_runtime_state_as_activated() {
        let state = ServiceRuntimeState::new();
        state.touch();
        assert!(state.has_activated.load(Ordering::Relaxed));
    }

    #[test]
    fn is_occupying_capacity_always_true_for_warm_stateful_service() {
        assert!(is_occupying_capacity(None, false, 0, 0, 1_000_000));
    }

    #[test]
    fn is_occupying_capacity_false_when_never_activated() {
        // Even with a recent last_accessed_unix (as set by ServiceRuntimeState::new()),
        // a service that has never received a real request should not count.
        assert!(!is_occupying_capacity(Some(60), false, 0, 200, 200));
    }

    #[test]
    fn is_occupying_capacity_true_when_active_connections_present() {
        assert!(is_occupying_capacity(Some(60), true, 1, 100, 200));
    }

    #[test]
    fn is_occupying_capacity_true_within_cooldown_window() {
        assert!(is_occupying_capacity(Some(60), true, 0, 190, 200));
    }

    #[test]
    fn is_occupying_capacity_false_once_cooldown_elapsed() {
        assert!(!is_occupying_capacity(Some(60), true, 0, 100, 200));
    }
}
