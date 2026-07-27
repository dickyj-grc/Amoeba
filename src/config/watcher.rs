//! Filesystem watcher (notify) driving lock-free ArcSwap config reloads.

use super::schema::ServiceCatalog;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::Path as FilePath;
use tracing::{error, info};

/// Parses raw JSON content into a `ServiceCatalog`.
pub fn parse_catalog(content: &str) -> Result<ServiceCatalog, serde_json::Error> {
    serde_json::from_str(content)
}

/// Reads and parses the service catalog from disk, filling in defaults and
/// validating referential integrity/exclusivity before handing it back.
pub fn load_catalog(path: &str) -> Result<ServiceCatalog, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let mut catalog = parse_catalog(&content)?;
    normalize_catalog(&mut catalog);
    validate_catalog(&catalog)?;
    Ok(catalog)
}

/// Fills in defaults that depend on context not available to serde alone:
/// a `stack_spec.project_name` left unset defaults to the service's own key
/// in the `services` map.
fn normalize_catalog(catalog: &mut ServiceCatalog) {
    for (name, svc) in catalog.services.iter_mut() {
        if let Some(stack) = svc.stack_spec.as_mut() {
            if stack.project_name.is_none() {
                stack.project_name = Some(name.clone());
            }
        }
    }
}

fn validate_catalog(catalog: &ServiceCatalog) -> Result<(), String> {
    validate_machine_references(catalog)?;
    validate_workload_exclusivity(catalog)?;
    validate_stack_spec_mode(catalog)?;
    validate_placement_host(catalog)?;
    validate_placement_type(catalog)?;
    validate_driver_supported(catalog)?;
    Ok(())
}

/// Fails closed on a typo'd `machine` reference rather than silently skipping
/// capacity gating for that service. A service that omits `placement.machine`
/// is only unambiguous while `machines` has exactly one entry (the "implied"
/// case); once a second machine exists, every service must say which one it's
/// on rather than defaulting to an arbitrary one.
fn validate_machine_references(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, svc) in &catalog.services {
        match &svc.placement.machine {
            Some(machine) => {
                if !catalog.machines.contains_key(machine) {
                    return Err(format!("service '{name}' references undefined machine '{machine}'"));
                }
            }
            None => {
                if catalog.machines.len() != 1 {
                    return Err(format!(
                        "service '{name}' omits placement.machine, which is only implied when \
                         `machines` has exactly one entry ({} present)",
                        catalog.machines.len()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Exactly one of `container`/`stack_spec` per service — these are the two
/// mutually exclusive ways to drive a service's lifecycle (single container
/// via bollard, vs. a compose stack via the `docker compose` CLI).
fn validate_workload_exclusivity(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, svc) in &catalog.services {
        match (&svc.container, &svc.stack_spec) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => {
                return Err(format!("service '{name}' must set exactly one of container/stack_spec, not both"))
            }
            (None, None) => return Err(format!("service '{name}' must set exactly one of container/stack_spec")),
        }
    }
    Ok(())
}

/// Within a present `stack_spec`, exactly one of `compose_file` (a
/// pre-existing file) or `services` (inline, Amoeba-generated) is set.
fn validate_stack_spec_mode(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, svc) in &catalog.services {
        let Some(stack) = &svc.stack_spec else { continue };
        match (&stack.compose_file, &stack.services) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => {
                return Err(format!(
                    "service '{name}' stack_spec must set exactly one of compose_file/services, not both"
                ))
            }
            (None, None) => {
                return Err(format!("service '{name}' stack_spec must set exactly one of compose_file/services"))
            }
        }
    }
    Ok(())
}

/// Whether `svc`'s machine lists `"apple-container"` among its `drivers`.
/// Assumes `validate_machine_references` has already run (so `catalog.machines`
/// lookups here are meaningful, not just defensively `None`-safe).
fn service_uses_apple_container(svc: &super::schema::ServiceConfig, catalog: &ServiceCatalog) -> bool {
    svc.machine_name(catalog)
        .and_then(|m| catalog.machines.get(m))
        .is_some_and(|m| m.drivers.iter().any(|d| d == "apple-container"))
}

/// A `container` workload routes via `placement.ip`; a `stack_spec` workload
/// routes via `placement.primary_service`. Requiring the other field be
/// absent (rather than just ignoring it) fails closed on a config that would
/// otherwise silently route somewhere the operator didn't intend.
///
/// Exception: a `container` workload on an `apple-container` machine must
/// *omit* `placement.ip` instead of requiring it — that driver assigns each
/// container a fresh IP on every start with no stable, host-resolvable name,
/// so the upstream host is resolved dynamically at runtime rather than read
/// from static config (see `lifecycle::apple_container`).
fn validate_placement_host(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, svc) in &catalog.services {
        if svc.container.is_some() {
            if service_uses_apple_container(svc, catalog) {
                if svc.placement.ip.is_some() {
                    return Err(format!(
                        "service '{name}' is on an apple-container machine, which assigns IPs \
                         dynamically at start-time — placement.ip must be omitted"
                    ));
                }
            } else if svc.placement.ip.is_none() {
                return Err(format!("service '{name}' has a container workload but no placement.ip"));
            }
            if svc.placement.primary_service.is_some() {
                return Err(format!(
                    "service '{name}' has a container workload but sets placement.primary_service \
                     (only valid for stack_spec)"
                ));
            }
        }
        if svc.stack_spec.is_some() {
            if svc.placement.primary_service.is_none() {
                return Err(format!(
                    "service '{name}' has a stack_spec workload but no placement.primary_service"
                ));
            }
            if svc.placement.ip.is_some() {
                return Err(format!(
                    "service '{name}' has a stack_spec workload but sets placement.ip (only valid for container)"
                ));
            }
        }
    }
    Ok(())
}

/// `"vm"` is the only supported placement/machine type today; fail closed on
/// anything else since no other driver dispatch exists yet.
fn validate_placement_type(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, machine) in &catalog.machines {
        if machine.r#type != "vm" {
            return Err(format!(
                "machine '{name}' has unsupported type '{}': only \"vm\" is supported today",
                machine.r#type
            ));
        }
    }
    for (name, svc) in &catalog.services {
        if svc.placement.r#type != "vm" {
            return Err(format!(
                "service '{name}' has unsupported placement.type '{}': only \"vm\" is supported today",
                svc.placement.r#type
            ));
        }
    }
    Ok(())
}

/// `"docker"` (bollard + Docker Engine API) and `"apple-container"` (Apple's
/// native `container` CLI, macOS/Apple Silicon only) are the only supported
/// drivers today.
fn validate_driver_supported(catalog: &ServiceCatalog) -> Result<(), String> {
    for (name, machine) in &catalog.machines {
        if !machine.drivers.iter().any(|d| d == "docker" || d == "apple-container") {
            return Err(format!(
                "machine '{name}' has no supported driver (only \"docker\"/\"apple-container\" are \
                 supported today): {:?}",
                machine.drivers
            ));
        }
    }
    Ok(())
}

/// How often the watch loop wakes up with no event pending, purely to check
/// whether `owner` has been dropped. Real file-change events are handled the
/// instant they arrive regardless of this interval — it only bounds how long
/// the watcher can outlive its owner once nothing is left watching for it.
const LIVENESS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Watches `path` for modifications and invokes `on_reload` with the freshly
/// parsed catalog each time the file changes. Stops once `owner` has no more
/// strong references — without this, the watch loop would run forever, since
/// nothing else ever signals it to stop; that's fine for the long-lived
/// production process, but leaks a permanently-blocked thread (and,
/// transitively, everything the closure captured) for every short-lived
/// `AppState` a test constructs.
///
/// Runs on a plain OS thread (`std::thread::spawn`), deliberately **not**
/// `tokio::task::spawn_blocking`: the watch loop below blocks synchronously on
/// a `std::sync::mpsc` receiver with no `.await` points, so it needs a thread
/// tokio isn't managing as part of any async task set either way — but the
/// important reason is that a `spawn_blocking` task would tie this thread's
/// lifetime to whichever `Runtime` spawned it. `Runtime::drop` (as `#[tokio::
/// test]` runs at the end of every test) waits for outstanding `spawn_blocking`
/// work before returning, and this watcher can only exit once every strong
/// `owner` reference is gone — including the one `spawn_reaper_thread` holds,
/// which itself is only released *by that same `Runtime::drop` cancelling it*.
/// Using an independent OS thread breaks that circular wait entirely: the
/// runtime can tear down immediately, and this thread notices `owner` is gone
/// on its own next liveness check shortly after.
pub fn spawn_file_watcher<F, T>(path: String, owner: std::sync::Weak<T>, mut on_reload: F)
where
    F: FnMut(ServiceCatalog) + Send + 'static,
    T: Send + Sync + 'static,
{
    std::thread::spawn(move || {
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

        loop {
            match rx.recv_timeout(LIVENESS_POLL_INTERVAL) {
                Ok(Ok(Event { kind: EventKind::Modify(_), .. })) => {
                    if let Ok(new_catalog) = load_catalog(&path) {
                        info!("🔄 Reloading service catalog from disk (lock-free)");
                        on_reload(new_catalog);
                    }
                }
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if owner.upgrade().is_none() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_catalog(label: &str, content: &str) -> String {
        let path = std::env::temp_dir()
            .join(format!("amoeba-watcher-test-{}-{label}.json", std::process::id()))
            .to_str()
            .unwrap()
            .to_string();
        std::fs::write(&path, content).unwrap();
        path
    }

    const MACHINES_LOCAL_ONLY: &str = r#""machines": { "local": { "type": "vm", "drivers": ["docker"], "resources": {} } }"#;

    fn container_service(name: &str, ip: &str, machine: Option<&str>) -> String {
        let machine_field = machine.map(|m| format!(r#","machine": "{m}""#)).unwrap_or_default();
        format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "{name}": {{
                        "placement": {{ "type": "vm", "ip": "{ip}", "port": 80 {machine_field} }},
                        "container": {{ "image": "x" }}
                    }}
                }}
            }}"#
        )
    }

    #[test]
    fn parse_catalog_parses_valid_json() {
        let catalog = parse_catalog(r#"{"version": 1, "services": {}}"#).unwrap();
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

    #[test]
    fn load_catalog_errors_on_undefined_machine_reference() {
        let path = write_temp_catalog("undefined-machine", &container_service("svc", "svc", Some("ghost-box")));
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_succeeds_when_machine_reference_is_defined() {
        let path = write_temp_catalog("defined-machine", &container_service("svc", "svc", Some("local")));
        assert!(load_catalog(&path).is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_succeeds_when_machine_omitted_and_exactly_one_machine_exists() {
        let path = write_temp_catalog("implicit-machine", &container_service("svc", "svc", None));
        assert!(load_catalog(&path).is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_machine_omitted_and_multiple_machines_exist() {
        let json = r#"{
            "version": 1,
            "machines": {
                "local": { "type": "vm", "drivers": ["docker"], "resources": {} },
                "other": { "type": "vm", "drivers": ["docker"], "resources": {} }
            },
            "services": {
                "svc": {
                    "placement": { "type": "vm", "ip": "svc", "port": 80 },
                    "container": { "image": "x" }
                }
            }
        }"#;
        let path = write_temp_catalog("ambiguous-machine", json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_neither_container_nor_stack_spec_set() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{ "placement": {{ "type": "vm", "ip": "svc", "port": 80 }} }}
                }}
            }}"#
        );
        let path = write_temp_catalog("neither-workload", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_both_container_and_stack_spec_set() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "vm", "ip": "svc", "port": 80 }},
                        "container": {{ "image": "x" }},
                        "stack_spec": {{ "compose_file": "/tmp/x.yml" }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("both-workloads", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_stack_spec_sets_both_compose_file_and_services() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "vm", "primary_service": "svc", "port": 80 }},
                        "stack_spec": {{
                            "compose_file": "/tmp/x.yml",
                            "services": {{ "svc": {{ "image": "x" }} }}
                        }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("both-stack-modes", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_stack_spec_sets_neither_compose_file_nor_services() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "vm", "primary_service": "svc", "port": 80 }},
                        "stack_spec": {{}}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("neither-stack-mode", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_container_workload_has_no_ip() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "vm", "port": 80 }},
                        "container": {{ "image": "x" }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("container-no-ip", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_stack_spec_workload_has_no_primary_service() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "vm", "port": 80 }},
                        "stack_spec": {{ "compose_file": "/tmp/x.yml" }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("stack-no-primary-service", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_defaults_project_name_to_service_key_when_omitted() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "brms": {{
                        "placement": {{ "type": "vm", "primary_service": "brms", "port": 3000 }},
                        "stack_spec": {{
                            "services": {{ "brms": {{ "image": "gorules/brms:latest" }} }}
                        }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("project-name-default", &json);
        let catalog = load_catalog(&path).unwrap();
        let stack = catalog.services.get("brms").unwrap().stack_spec.as_ref().unwrap();
        assert_eq!(stack.project_name.as_deref(), Some("brms"));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_on_unsupported_placement_type() {
        let json = format!(
            r#"{{
                "version": 1,
                {MACHINES_LOCAL_ONLY},
                "services": {{
                    "svc": {{
                        "placement": {{ "type": "serverless", "ip": "svc", "port": 80 }},
                        "container": {{ "image": "x" }}
                    }}
                }}
            }}"#
        );
        let path = write_temp_catalog("unsupported-placement-type", &json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_on_unsupported_driver() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["fly"], "resources": {} } },
            "services": {
                "svc": {
                    "placement": { "type": "vm", "ip": "svc", "port": 80 },
                    "container": { "image": "x" }
                }
            }
        }"#;
        let path = write_temp_catalog("unsupported-driver", json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_accepts_apple_container_driver() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["apple-container"], "resources": {} } },
            "services": {
                "svc": {
                    "placement": { "type": "vm", "port": 11434, "cooldown_seconds": 60 },
                    "container": { "image": "ollama/ollama:latest" }
                }
            }
        }"#;
        let path = write_temp_catalog("apple-container-ok", json);
        assert!(load_catalog(&path).is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_catalog_errors_when_apple_container_service_sets_placement_ip() {
        let json = r#"{
            "version": 1,
            "machines": { "local": { "type": "vm", "drivers": ["apple-container"], "resources": {} } },
            "services": {
                "svc": {
                    "placement": { "type": "vm", "ip": "192.168.64.2", "port": 11434 },
                    "container": { "image": "ollama/ollama:latest" }
                }
            }
        }"#;
        let path = write_temp_catalog("apple-container-with-ip", json);
        assert!(load_catalog(&path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
