//! App lifecycle state tracked separately from the service catalog.
//!
//! When an app is installed Amoeba pre-pulls/validates it in the background.
//! The state machine here exposes that progress so callers can poll for
//! readiness and the proxy can fail fast while the app is still being prepared.

use serde::Serialize;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    /// Catalog entry exists but background preparation has not started yet.
    Installed,
    /// Images are being pulled and/or the stack is being brought up.
    Pulling,
    /// Image(s) are present and the app's port responds to a TCP probe.
    Ready,
    /// Preparation failed; `message` holds the details.
    Error,
}

/// A point-in-time snapshot of an app's lifecycle state.
#[derive(Debug, Clone, Serialize)]
pub struct AppLifecycleSnapshot {
    pub state: LifecycleStatus,
    pub message: Option<String>,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
struct Inner {
    state: LifecycleStatus,
    message: Option<String>,
    updated_at: u64,
}

/// Thread-safe, mutable lifecycle state for a single app.
#[derive(Debug)]
pub struct AppLifecycleState {
    inner: RwLock<Inner>,
}

impl AppLifecycleState {
    pub fn installed() -> Self {
        Self::new(LifecycleStatus::Installed, None)
    }

    pub fn ready() -> Self {
        Self::new(LifecycleStatus::Ready, None)
    }

    fn new(state: LifecycleStatus, message: Option<String>) -> Self {
        Self {
            inner: RwLock::new(Inner {
                state,
                message,
                updated_at: now_unix(),
            }),
        }
    }

    /// Atomically transition to a new state with an optional human-readable message.
    pub fn transition(&self, state: LifecycleStatus, message: Option<String>) {
        let mut inner = self.inner.write().unwrap();
        inner.state = state;
        inner.message = message;
        inner.updated_at = now_unix();
    }

    /// Read the current state without blocking other readers.
    pub fn snapshot(&self) -> AppLifecycleSnapshot {
        let inner = self.inner.read().unwrap();
        AppLifecycleSnapshot {
            state: inner.state,
            message: inner.message.clone(),
            updated_at: inner.updated_at,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.inner.read().unwrap().state == LifecycleStatus::Ready
    }

    pub fn is_error(&self) -> bool {
        self.inner.read().unwrap().state == LifecycleStatus::Error
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_state_is_installed() {
        let state = AppLifecycleState::installed();
        assert_eq!(state.snapshot().state, LifecycleStatus::Installed);
    }

    #[test]
    fn transitions_update_state_and_time() {
        let state = AppLifecycleState::installed();
        let before = state.snapshot().updated_at;
        state.transition(LifecycleStatus::Pulling, Some("pulling image".to_string()));

        let snap = state.snapshot();
        assert_eq!(snap.state, LifecycleStatus::Pulling);
        assert_eq!(snap.message, Some("pulling image".to_string()));
        assert!(snap.updated_at >= before);
    }

    #[test]
    fn ready_state_is_ready() {
        let state = AppLifecycleState::ready();
        assert!(state.is_ready());
        assert!(!state.is_error());
    }

    #[test]
    fn error_state_is_not_ready() {
        let state = AppLifecycleState::installed();
        state.transition(LifecycleStatus::Error, Some("pull failed".to_string()));
        assert!(!state.is_ready());
        assert!(state.is_error());
    }
}
