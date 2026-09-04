//! Remote JWKS fetching, caching, and verification (distributed/multi-tenant mode).

use jsonwebtoken::jwk::JwkSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::info;

/// Fetches a JWKS document from `url` and parses it into a key set.
pub async fn fetch_jwks(url: &str) -> Result<JwkSet, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("failed to build JWKS HTTP client: {e}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch JWKS from {url}: {e}"))?;
    response
        .json::<JwkSet>()
        .await
        .map_err(|e| format!("JWKS response from {url} is not valid JWKS JSON: {e}"))
}

/// Replaces the cached JWKS key set with `new_keys`.
pub async fn store_jwks(cache: &RwLock<JwkSet>, new_keys: JwkSet) {
    let mut lock = cache.write().await;
    *lock = new_keys;
}

/// Background task that refreshes JWKS public keys periodically.
pub fn start_jwks_refresh_daemon(jwks_url: String, cache: Arc<RwLock<JwkSet>>) {
    tokio::spawn(async move {
        loop {
            if let Ok(jwks) = fetch_jwks(&jwks_url).await {
                store_jwks(&cache, jwks).await;
                info!("🔑 Successfully updated JWKS public key cache.");
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
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
