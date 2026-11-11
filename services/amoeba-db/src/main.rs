//! `amoeba-db` entry point. Configuration comes from the environment:
//!
//! - `DATABASE_URL`: connection string (required unless both of the next two
//!   are set). Usually delivered as an Amoeba app secret.
//! - `DATABASE_URL_READ` / `DATABASE_URL_WRITE`: separate accounts for reads
//!   (a read-only role) and writes (rights only on writable tables).
//! - `DB_DEFINITIONS_FILE`: path to the definitions JSON, re-read when it
//!   changes; or `DB_DEFINITIONS`: the JSON itself.
//! - `PORT` (default 8080).

use amoeba_db::db::Db;
use amoeba_db::defs::Definitions;
use amoeba_db::{Service, router};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::{error, info};

const RELOAD_POLL: Duration = Duration::from_secs(2);

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn load_definitions() -> Result<(Definitions, Option<String>), String> {
    if let Some(path) = env("DB_DEFINITIONS_FILE") {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))?;
        return Ok((Definitions::parse(&text)?, Some(path)));
    }
    if let Some(text) = env("DB_DEFINITIONS") {
        return Ok((Definitions::parse(&text)?, None));
    }
    Err("set DB_DEFINITIONS_FILE or DB_DEFINITIONS".into())
}

fn modified(path: &str) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Re-reads the definitions file when it changes. An invalid file is logged
/// and ignored, so a bad edit never takes the service down.
fn watch_definitions(svc: Arc<Service>, path: String) {
    tokio::spawn(async move {
        let mut last = modified(&path);
        loop {
            tokio::time::sleep(RELOAD_POLL).await;
            let now = modified(&path);
            if now == last {
                continue;
            }
            last = now;
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| Definitions::parse(&t))
            {
                Ok(defs) => {
                    info!(
                        tables = defs.tables.len(),
                        queries = defs.queries.len(),
                        "reloaded definitions"
                    );
                    svc.replace_definitions(defs);
                }
                Err(e) => error!("kept previous definitions; new file is invalid: {e}"),
            }
        }
    });
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if let Err(e) = run().await {
        error!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let base = env("DATABASE_URL");
    let read = env("DATABASE_URL_READ").or_else(|| base.clone());
    let write = env("DATABASE_URL_WRITE").or_else(|| base.clone());
    let (Some(read), Some(write)) = (read, write) else {
        return Err("set DATABASE_URL, or both DATABASE_URL_READ and DATABASE_URL_WRITE".into());
    };

    let (defs, path) = load_definitions()?;
    info!(
        tables = defs.tables.len(),
        queries = defs.queries.len(),
        "loaded definitions"
    );
    let svc = Service::new(Db::new(&read, &write)?, defs);
    if let Some(path) = path {
        watch_definitions(svc.clone(), path);
    }

    let port = env("PORT").unwrap_or_else(|| "8080".into());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("binding {addr}: {e}"))?;
    info!("amoeba-db listening on {addr}");
    axum::serve(listener, router(svc))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}
