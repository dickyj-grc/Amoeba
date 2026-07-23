//! Shared application state: config catalog, runtime tracking, auth engine.

use crate::auth::JwtEngine;
use crate::config::schema::ServiceCatalog;
use crate::config::watcher::{load_catalog, spawn_file_watcher};
use crate::lifecycle::container::ServiceRuntimeState;
use crate::lifecycle::reaper::spawn_reaper_thread;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::Arc;

pub struct AppState {
    pub catalog: ArcSwap<ServiceCatalog>,
    pub runtime_states: ArcSwap<HashMap<String, Arc<ServiceRuntimeState>>>,
    pub jwt_engine: Arc<JwtEngine>,
    pub http_client: reqwest::Client,
}

impl AppState {
    pub fn new(config_path: &str, jwt_engine: Arc<JwtEngine>) -> Arc<Self> {
        let initial_catalog = load_catalog(config_path).unwrap_or_else(|_| ServiceCatalog {
            services: HashMap::new(),
        });

        let initial_runtimes = initial_catalog
            .services
            .keys()
            .map(|key| (key.clone(), Arc::new(ServiceRuntimeState::new())))
            .collect();

        let state = Arc::new(Self {
            catalog: ArcSwap::from_pointee(initial_catalog),
            runtime_states: ArcSwap::from_pointee(initial_runtimes),
            jwt_engine,
            http_client: reqwest::Client::new(),
        });

        Self::watch_config(state.clone(), config_path.to_string());
        spawn_reaper_thread(state.clone());

        state
    }

    fn watch_config(state: Arc<Self>, path: String) {
        spawn_file_watcher(path, move |new_catalog| {
            let mut new_runtimes = (*state.runtime_states.load().clone()).clone();
            for key in new_catalog.services.keys() {
                new_runtimes
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(ServiceRuntimeState::new()));
            }

            state.catalog.store(Arc::new(new_catalog));
            state.runtime_states.store(Arc::new(new_runtimes));
        });
    }
}
