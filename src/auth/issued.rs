//! Registry of service tokens issued from `/admin/apps/<name>/token`.
//!
//! Only metadata is kept (label, org, expiry, `jti`), never the token itself,
//! so the file cannot be replayed. It lets an admin see which tokens exist
//! for a service and revoke one by `jti` without holding it. Expired entries
//! are dropped on the next write.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssuedToken {
    pub jti: String,
    pub service: String,
    #[serde(default)]
    pub label: String,
    pub org_id: Option<String>,
    pub role: String,
    /// Unix seconds.
    pub issued_at: u64,
    /// Unix seconds; the token's `exp`.
    pub expires_at: u64,
}

#[derive(Clone, Default)]
pub struct IssuedTokenStore {
    tokens: Arc<RwLock<Vec<IssuedToken>>>,
    persist_path: Option<PathBuf>,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl IssuedTokenStore {
    /// Loads `path` (empty when missing or corrupt) and writes every change
    /// back to it.
    pub fn persistent(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let tokens = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str::<Vec<IssuedToken>>(&content).ok())
            .unwrap_or_default();
        Self {
            tokens: Arc::new(RwLock::new(tokens)),
            persist_path: Some(path),
        }
    }

    pub async fn record(&self, token: IssuedToken) {
        {
            let now = now_secs();
            let mut tokens = self.tokens.write().await;
            tokens.retain(|t| t.expires_at > now);
            tokens.push(token);
        }
        self.persist().await;
    }

    /// Unexpired tokens for `service`, newest first.
    pub async fn list(&self, service: &str) -> Vec<IssuedToken> {
        let now = now_secs();
        let mut out: Vec<IssuedToken> = self
            .tokens
            .read()
            .await
            .iter()
            .filter(|t| t.service == service && t.expires_at > now)
            .cloned()
            .collect();
        out.sort_by_key(|t| std::cmp::Reverse(t.issued_at));
        out
    }

    /// Removes and returns the entry when it belongs to `service`.
    pub async fn remove(&self, service: &str, jti: &str) -> Option<IssuedToken> {
        let removed = {
            let mut tokens = self.tokens.write().await;
            let pos = tokens
                .iter()
                .position(|t| t.service == service && t.jti == jti)?;
            tokens.remove(pos)
        };
        self.persist().await;
        Some(removed)
    }

    /// Temp file + rename, owner-only, like the revocation file. Best-effort:
    /// a failed write is logged and the in-memory state stays authoritative.
    async fn persist(&self) {
        let Some(path) = &self.persist_path else {
            return;
        };
        let content = match serde_json::to_string_pretty(&*self.tokens.read().await) {
            Ok(content) => content,
            Err(e) => {
                warn!("failed to serialize issued tokens for {path:?}: {e}");
                return;
            }
        };
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, &content) {
            warn!("failed to write issued tokens file {tmp:?}: {e}");
            return;
        }
        super::revocation::restrict_to_owner(&tmp);
        if let Err(e) = std::fs::rename(&tmp, path) {
            warn!("failed to replace issued tokens file {path:?}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(jti: &str, service: &str, issued_at: u64, expires_at: u64) -> IssuedToken {
        IssuedToken {
            jti: jti.into(),
            service: service.into(),
            label: String::new(),
            org_id: None,
            role: format!("mcp:{service}"),
            issued_at,
            expires_at,
        }
    }

    // @lat: [[deploy#Service tokens#Registry]]
    #[tokio::test]
    async fn keeps_metadata_per_service_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issued_tokens.json");
        let now = now_secs();
        let store = IssuedTokenStore::persistent(&path);
        // Dropped by the next write, since it has already expired.
        store
            .record(token("expired", "svc", now - 200, now - 1))
            .await;
        store.record(token("old", "svc", now - 10, now + 100)).await;
        store.record(token("new", "svc", now, now + 100)).await;
        store
            .record(token("other", "elsewhere", now, now + 100))
            .await;

        let reloaded = IssuedTokenStore::persistent(&path);
        let jtis: Vec<_> = reloaded
            .list("svc")
            .await
            .into_iter()
            .map(|t| t.jti)
            .collect();
        assert_eq!(jtis, ["new", "old"]);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("expired"));

        assert!(reloaded.remove("elsewhere", "new").await.is_none());
        assert_eq!(reloaded.remove("svc", "new").await.unwrap().jti, "new");
        assert_eq!(
            IssuedTokenStore::persistent(&path).list("svc").await.len(),
            1
        );
    }
}
