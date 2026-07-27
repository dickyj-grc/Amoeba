//! In-memory token revocation store.
//!
//! Revocation state is deliberately not persisted: a process restart clears the
//! set and forces clients to re-authenticate. This keeps the standalone gateway
//! free of file-I/O or external dependencies for token lifecycle management.

use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared, in-memory set of revoked JWT IDs (`jti`).
#[derive(Clone, Default)]
pub struct InMemoryRevocationStore {
    revoked: Arc<RwLock<HashSet<String>>>,
}

impl InMemoryRevocationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks a `jti` as revoked.
    pub async fn revoke(&self, jti: String) {
        let mut store = self.revoked.write().await;
        store.insert(jti);
    }

    /// Returns true if the given `jti` has been revoked.
    pub async fn is_revoked(&self, jti: &str) -> bool {
        let store = self.revoked.read().await;
        store.contains(jti)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revokes_and_checks_a_jti() {
        let store = InMemoryRevocationStore::new();
        assert!(!store.is_revoked("abc123").await);
        store.revoke("abc123".to_string()).await;
        assert!(store.is_revoked("abc123").await);
    }

    #[tokio::test]
    async fn unknown_jti_is_not_revoked() {
        let store = InMemoryRevocationStore::new();
        store.revoke("abc123".to_string()).await;
        assert!(!store.is_revoked("other").await);
    }
}
