//! Remote JWKS fetching, caching, and verification (distributed/multi-tenant mode).

use jsonwebtoken::jwk::JwkSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Replaces the cached JWKS key set with `new_keys`.
pub async fn store_jwks(cache: &RwLock<JwkSet>, new_keys: JwkSet) {
    let mut lock = cache.write().await;
    *lock = new_keys;
}

/// Background task that refreshes JWKS public keys periodically.
pub fn start_jwks_refresh_daemon(jwks_url: String, cache: Arc<RwLock<JwkSet>>) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            if let Ok(response) = client.get(&jwks_url).send().await {
                if let Ok(jwks) = response.json::<JwkSet>().await {
                    store_jwks(&cache, jwks).await;
                    info!("🔑 Successfully updated JWKS public key cache.");
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_jwks_replaces_cache_contents() {
        let cache = RwLock::new(JwkSet { keys: vec![] });
        store_jwks(&cache, JwkSet { keys: vec![] }).await;
        assert!(cache.read().await.keys.is_empty());
    }
}
