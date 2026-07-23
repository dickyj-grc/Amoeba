//! Periodic sweep that scales idle services down to zero.

use super::container::{now_unix, should_scale_to_zero};
use crate::state::AppState;
use std::sync::{atomic::Ordering, Arc};
use tracing::info;

/// Spawns a background loop that checks each service's idle time against its
/// configured cooldown and scales it to zero once exceeded.
pub fn spawn_reaper_thread(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            let catalog = state.catalog.load();
            let runtimes = state.runtime_states.load();
            let now = now_unix();

            for (name, service) in &catalog.services {
                let Some(cooldown) = service.cooldown_seconds else {
                    continue;
                };
                let Some(runtime) = runtimes.get(name) else {
                    continue;
                };

                let active = runtime.active_connections.load(Ordering::Relaxed);
                let last = runtime.last_accessed_unix.load(Ordering::Relaxed);

                if should_scale_to_zero(active, last, now, cooldown) {
                    info!(
                        service = %name,
                        idle_secs = %now.saturating_sub(last),
                        "💤 Scaling service to zero (stopping container)"
                    );
                    // TODO: Call Docker API (`/var/run/docker.sock`) or Fly Machine API to stop container
                }
            }
        }
    });
}
