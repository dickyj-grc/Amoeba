//! Container start/stop, active connection counting, cooldown timers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Runtime tracker for active connections and activity timestamps.
pub struct ServiceRuntimeState {
    pub last_accessed_unix: AtomicU64,
    pub active_connections: AtomicU64,
}

impl ServiceRuntimeState {
    pub fn new() -> Self {
        Self {
            last_accessed_unix: AtomicU64::new(now_unix()),
            active_connections: AtomicU64::new(0),
        }
    }

    pub fn touch(&self) {
        self.last_accessed_unix.store(now_unix(), Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_starts_with_no_active_connections() {
        let state = ServiceRuntimeState::new();
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 0);
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
}
