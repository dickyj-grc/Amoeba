//! Shared application state: config catalog, runtime tracking, auth engine.

use crate::apps::state::{AppLifecycleSnapshot, AppLifecycleState};
use crate::auth::JwtEngine;
use crate::auth::revocation::InMemoryRevocationStore;
use crate::config::schema::ServiceCatalog;
use crate::config::watcher::{load_catalog, spawn_file_watcher};
use crate::lifecycle::container::ServiceRuntimeState;
use crate::lifecycle::driver::{self, ServiceDriver};
use crate::lifecycle::reaper::spawn_reaper_thread;
use arc_swap::ArcSwap;
use bollard::Docker;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub struct AppState {
    pub catalog: ArcSwap<ServiceCatalog>,
    pub runtime_states: ArcSwap<HashMap<String, Arc<ServiceRuntimeState>>>,
    pub app_states: ArcSwap<HashMap<String, Arc<AppLifecycleState>>>,
    pub drivers: ArcSwap<HashMap<String, Arc<ServiceDriver>>>,
    pub jwt_engine: Arc<JwtEngine>,
    pub revocation_store: Option<InMemoryRevocationStore>,
    pub http_client: reqwest::Client,
    pub users_file: String,
    pub catalog_path: String,
}

impl AppState {
    pub fn new(
        config_path: &str,
        users_file: &str,
        jwt_engine: Arc<JwtEngine>,
        revocation_store: Option<InMemoryRevocationStore>,
    ) -> Arc<Self> {
        let initial_catalog = load_catalog(config_path).unwrap_or_else(|_| ServiceCatalog {
            version: 1,
            machines: HashMap::new(),
            scheduling: None,
            services: HashMap::new(),
        });

        let initial_runtimes = initial_catalog
            .services
            .keys()
            .map(|key| (key.clone(), Arc::new(ServiceRuntimeState::new())))
            .collect();

        let initial_app_states = initial_catalog
            .services
            .keys()
            .map(|key| (key.clone(), Arc::new(AppLifecycleState::installed())))
            .collect();

        // Connecting is best-effort: a missing/unreachable Docker socket must not
        // stop the whole gateway from starting (e.g. proxying to already-running,
        // externally-managed backends still works). Container-workload drivers
        // simply fail closed on first use when this is `None`.
        let docker = Docker::connect_with_local_defaults()
            .inspect_err(|e| warn!("failed to connect to local Docker Engine: {e}; container/compose drivers will be unavailable until this is fixed"))
            .ok();

        let config_dir = config_dir_of(config_path);
        let initial_drivers = driver::build_all(&initial_catalog, &config_dir, docker.clone());

        // A bare `reqwest::Client::new()` has no timeout at all, so a backend that
        // never responds (or a connection that's silently dropped rather than
        // actively refused) would hang the proxied request forever.
        // Use a read (inactivity) timeout, not a total wall-clock timeout: a total
        // timeout would guillotine long-lived SSE streams once the deadline is hit,
        // whereas a read timeout only fires when the upstream sends nothing for the
        // configured window — exactly what scale-to-zero needs to know.
        let http_client = reqwest::Client::builder()
            .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
            .read_timeout(UPSTREAM_READ_TIMEOUT)
            .build()
            .expect("failed to build upstream HTTP client");

        let state = Arc::new(Self {
            catalog: ArcSwap::from_pointee(initial_catalog),
            runtime_states: ArcSwap::from_pointee(initial_runtimes),
            app_states: ArcSwap::from_pointee(initial_app_states),
            drivers: ArcSwap::from_pointee(initial_drivers),
            jwt_engine,
            revocation_store,
            http_client,
            users_file: users_file.to_string(),
            catalog_path: config_path.to_string(),
        });

        Self::watch_config(state.clone(), config_path.to_string(), config_dir, docker);
        spawn_reaper_thread(state.clone());
        Self::spawn_initial_readiness_probes(state.clone());

        state
    }

    fn watch_config(state: Arc<Self>, path: String, config_dir: PathBuf, docker: Option<Docker>) {
        let owner = Arc::downgrade(&state);
        spawn_file_watcher(path, owner, move |new_catalog| {
            let mut new_runtimes = (*state.runtime_states.load().clone()).clone();
            let mut new_app_states = (*state.app_states.load().clone()).clone();
            for key in new_catalog.services.keys() {
                new_runtimes
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(ServiceRuntimeState::new()));
                new_app_states
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(AppLifecycleState::installed()));
            }

            let new_drivers = driver::build_all(&new_catalog, &config_dir, docker.clone());

            state.catalog.store(Arc::new(new_catalog));
            state.runtime_states.store(Arc::new(new_runtimes));
            state.app_states.store(Arc::new(new_app_states));
            state.drivers.store(Arc::new(new_drivers));
        });
    }
}

impl AppState {
    /// Read the lifecycle state for a service, if one exists.
    pub fn app_state(&self, name: &str) -> Option<Arc<AppLifecycleState>> {
        self.app_states.load().get(name).cloned()
    }

    /// Set or replace the lifecycle state for a service.
    pub fn set_app_state(&self, name: &str, app_state: Arc<AppLifecycleState>) {
        let mut new_states = (*self.app_states.load().clone()).clone();
        new_states.insert(name.to_string(), app_state);
        self.app_states.store(Arc::new(new_states));
    }

    /// Return serializable snapshots of all tracked app states.
    pub fn app_state_snapshots(&self) -> HashMap<String, AppLifecycleSnapshot> {
        self.app_states
            .load()
            .iter()
            .map(|(k, v)| (k.clone(), v.snapshot()))
            .collect()
    }

    fn spawn_initial_readiness_probes(state: Arc<Self>) {
        let names: Vec<String> = state.app_states.load().keys().cloned().collect();
        for name in names {
            crate::apps::manager::spawn_readiness_probe(state.clone(), name);
        }
    }
}

fn config_dir_of(config_path: &str) -> PathBuf {
    Path::new(config_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
