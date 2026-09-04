//! Token revocation store: a shared set of revoked JWT IDs (`jti`) with
//! optional write-through persistence to a JSON file.
//!
//! Persistence matters because the signing secret usually survives a process
//! restart (it comes from config/env). Without a persisted revocation list, a
//! restart would silently clear revocations and re-accept every revoked-but-
//! unexpired token. `RevocationStore::persistent` reloads the file at startup
//! so revoked tokens stay revoked for the rest of their lifetime.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

/// Shared set of revoked JWT IDs (`jti`), optionally persisted to disk.
#[derive(Clone, Default)]
pub struct RevocationStore {
    revoked: Arc<RwLock<HashSet<String>>>,
    persist_path: Option<PathBuf>,
}

impl RevocationStore {
    /// Pure in-memory store: revocations are lost on restart. Intended for
    /// tests; production startup uses `persistent`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads previously persisted revocations from `path` (treated as empty
    /// when the file is missing or corrupt) and persists every future revoke
    /// back to it, so revocations survive process restarts.
    pub fn persistent(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let revoked = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<HashSet<String>>(&content).ok())
            .unwrap_or_default();
        Self {
            revoked: Arc::new(RwLock::new(revoked)),
            persist_path: Some(path),
        }
    }

    /// Marks a `jti` as revoked.
    pub async fn revoke(&self, jti: String) {
        let inserted = {
            let mut store = self.revoked.write().await;
            store.insert(jti)
        };
        // Only rewrite the file when the set actually changed.
        if inserted {
            self.persist().await;
        }
    }

    /// Returns true if the given `jti` has been revoked.
    pub async fn is_revoked(&self, jti: &str) -> bool {
        let store = self.revoked.read().await;
        store.contains(jti)
    }

    /// Writes the full revoked set to `persist_path` (temp file + rename, so a
    /// crash mid-write can't leave a truncated file), owner-only `0600` since
    /// the set reveals which tokens were force-expired. Best-effort: a failed
    /// write is logged but does not fail the in-memory revoke.
    async fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };

        let content = {
            let store = self.revoked.read().await;
            match serde_json::to_string_pretty(&*store) {
                Ok(content) => content,
                Err(e) => {
                    warn!("failed to serialize revocation set for {path:?}: {e}");
                    return;
                }
            }
        };

        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, &content) {
            warn!("failed to write revocation file {tmp:?}: {e}");
            return;
        }
        restrict_to_owner(&tmp);
        if let Err(e) = std::fs::rename(&tmp, path) {
            warn!("failed to replace revocation file {path:?}: {e}");
        }
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        warn!("failed to set permissions on {path:?}: {e}");
    }
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_revocation_file(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "amoeba-revocation-test-{}-{label}.json",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn revokes_and_checks_a_jti() {
        let store = RevocationStore::new();
        assert!(!store.is_revoked("abc123").await);
        store.revoke("abc123".to_string()).await;
        assert!(store.is_revoked("abc123").await);
    }

    #[tokio::test]
    async fn unknown_jti_is_not_revoked() {
        let store = RevocationStore::new();
        store.revoke("abc123".to_string()).await;
        assert!(!store.is_revoked("other").await);
    }

    #[tokio::test]
    async fn persistent_store_survives_restart() {
        let path = temp_revocation_file("restart");
        let store = RevocationStore::persistent(&path);
        store.revoke("token-1".to_string()).await;

        // A "restart": brand-new store instance over the same file.
        let restarted = RevocationStore::persistent(&path);
        assert!(restarted.is_revoked("token-1").await);
        assert!(!restarted.is_revoked("token-2").await);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn missing_persist_file_starts_empty() {
        let path = temp_revocation_file("missing");
        let store = RevocationStore::persistent(&path);
        assert!(!store.is_revoked("anything").await);
        // Revoking creates the file.
        store.revoke("fresh".to_string()).await;
        assert!(path.exists());
        std::fs::remove_file(&path).ok();
    }
}
