//! Filesystem watcher (notify) driving lock-free ArcSwap config reloads.

use super::schema::ServiceCatalog;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::Path as FilePath;
use tracing::{error, info};

/// Parses raw JSON content into a `ServiceCatalog`.
pub fn parse_catalog(content: &str) -> Result<ServiceCatalog, serde_json::Error> {
    serde_json::from_str(content)
}

/// Reads and parses the service catalog from disk.
pub fn load_catalog(path: &str) -> Result<ServiceCatalog, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    Ok(parse_catalog(&content)?)
}

/// Watches `path` for modifications and invokes `on_reload` with the freshly
/// parsed catalog each time the file changes.
///
/// Runs on tokio's blocking thread pool (`spawn_blocking`), not a regular async
/// task: the watch loop below blocks synchronously on a `std::sync::mpsc`
/// receiver with no `.await` points, so spawning it via plain `tokio::spawn`
/// would permanently occupy an async worker thread and starve every other task
/// scheduled on it once the watched path actually exists.
pub fn spawn_file_watcher<F>(path: String, mut on_reload: F)
where
    F: FnMut(ServiceCatalog) + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                error!("failed to create config file watcher: {e}");
                return;
            }
        };

        if let Err(e) = watcher.watch(FilePath::new(&path), RecursiveMode::NonRecursive) {
            error!("failed to watch {path}: {e}");
            return;
        }

        for res in rx {
            if let Ok(Event { kind: EventKind::Modify(_), .. }) = res {
                if let Ok(new_catalog) = load_catalog(&path) {
                    info!("🔄 Reloading service catalog from disk (lock-free)");
                    on_reload(new_catalog);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_catalog_parses_valid_json() {
        let catalog = parse_catalog(r#"{"services": {}}"#).unwrap();
        assert!(catalog.services.is_empty());
    }

    #[test]
    fn parse_catalog_rejects_invalid_json() {
        assert!(parse_catalog("not json").is_err());
    }

    #[test]
    fn load_catalog_errors_on_missing_file() {
        assert!(load_catalog("/nonexistent/path/services.json").is_err());
    }
}
