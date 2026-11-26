//! What the container drivers share to confine an app (see
//! [`crate::config::schema::Sandbox`]): its own network, the user to run as,
//! the writable scratch paths on a read-only root, and the identity settings
//! every app container gets.

use crate::auth::identity;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::process::Command;

/// Who a root image runs as when it may not run as root: `nobody`.
pub const NON_ROOT_USER: &str = "65534:65534";
/// Writable scratch space on a read-only root filesystem.
pub const SCRATCH_PATHS: [&str; 3] = ["/tmp", "/run", "/var/tmp"];
/// Hard memory limit for a sandboxed app that declares none.
pub const DEFAULT_MEMORY_MB: u64 = 1024;

/// The gateway's own container, which joins each app's network to reach it:
/// `AMOEBA_SELF_CONTAINER`, else the hostname Docker gives it. `None` when
/// the gateway does not run in a container, as in native mode on a Mac.
pub fn gateway_container() -> Option<String> {
    std::env::var("AMOEBA_SELF_CONTAINER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::path::Path::new("/.dockerenv")
                .exists()
                .then(|| std::env::var("HOSTNAME").ok())
                .flatten()
        })
}

/// A compose file with its `amoeba-net` network (the external network apps
/// join to be reached by the gateway) pointed at `network` instead, so the
/// stack shares a network with the gateway only. Unchanged when the file has
/// no such network.
pub fn isolate_compose_network(
    compose: &str,
    network: &str,
    egress: bool,
) -> Result<String, String> {
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(compose).map_err(|e| format!("invalid compose file: {e}"))?;
    let Some(networks) = doc.get_mut("networks").and_then(|n| n.as_mapping_mut()) else {
        return Ok(compose.to_string());
    };
    let mut changed = false;
    let mut gateway_key = None;
    for (key, value) in networks.iter_mut() {
        let Some(spec) = value.as_mapping_mut() else {
            continue;
        };
        let external = spec
            .get("external")
            .and_then(|e| e.as_bool())
            .unwrap_or(false);
        let name = spec
            .get("name")
            .and_then(|n| n.as_str())
            .or(key.as_str())
            .unwrap_or_default();
        if external && name == "amoeba-net" {
            spec.insert("name".into(), network.into());
            changed = true;
            gateway_key = key.as_str().map(str::to_string);
        }
    }
    if !changed {
        return Ok(compose.to_string());
    }
    if egress && let Some(gateway_key) = gateway_key {
        networks.insert(
            EGRESS_NETWORK.into(),
            serde_yaml::from_str(&format!("{{external: true, name: {EGRESS_NETWORK}}}"))
                .expect("static yaml"),
        );
        // Services on the gateway network also get the egress network.
        let services = doc
            .get_mut("services")
            .and_then(|s| s.as_mapping_mut())
            .into_iter()
            .flat_map(|m| m.values_mut());
        for service in services {
            let Some(nets) = service.get_mut("networks") else {
                continue;
            };
            if let Some(list) = nets.as_sequence_mut() {
                if list
                    .iter()
                    .any(|n| n.as_str() == Some(gateway_key.as_str()))
                {
                    list.push(EGRESS_NETWORK.into());
                }
            } else if let Some(map) = nets.as_mapping_mut()
                && map.contains_key(gateway_key.as_str())
            {
                map.insert(EGRESS_NETWORK.into(), serde_yaml::Value::Null);
            }
        }
    }
    serde_yaml::to_string(&doc).map_err(|e| format!("cannot write the compose file: {e}"))
}

/// Where sandboxed apps with `egress` get outbound access. The gateway never
/// joins it (so it can never become the gateway's route), and its bridge has
/// inter-container traffic off, so apps on it cannot reach each other.
pub const EGRESS_NETWORK: &str = "amoeba-egress";
/// Docker bridge option that stops containers on one network talking.
pub const NO_ICC_OPTION: (&str, &str) = ("com.docker.network.bridge.enable_icc", "false");

/// The network only this app and the gateway share. Always internal.
pub fn network_name(service: &str) -> String {
    format!("amoeba-app-{service}")
}

/// Settings every app container gets: the gateway's public key, to verify
/// `x-amoeba-identity`, and the app's own service name, its expected `aud`.
pub fn identity_env(service: &str) -> HashMap<String, String> {
    HashMap::from([
        (identity::PUBLIC_KEY_ENV.to_string(), identity::public_key()),
        (identity::SERVICE_NAME_ENV.to_string(), service.to_string()),
    ])
}

/// Whether an image's configured user (`Config.User`) is root: unset,
/// `root` or uid 0, with or without a group.
pub fn is_root_user(user: &str) -> bool {
    let name = user.split(':').next().unwrap_or("").trim();
    name.is_empty() || name == "root" || name == "0"
}

/// An image's runtime config, from `docker image inspect` or, for Apple
/// containers, `container image inspect`. `None` when it cannot be read.
pub async fn image_config(image: &str, apple: bool) -> Option<serde_json::Value> {
    let program = if apple { "container" } else { "docker" };
    let output = Command::new(program)
        .args(["image", "inspect", image])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let first = json.get(0).unwrap_or(&json);
    // Docker: `[{"Config": {...}}]`. Apple: `[{"variants": [{"config": {"config": {...}}}]}]`.
    first
        .get("Config")
        .or_else(|| {
            first
                .get("variants")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("config"))
                .and_then(|c| c.get("config"))
        })
        .cloned()
}

/// The user to run `image` as: `None` keeps the image's own user, which is
/// what happens when root is allowed or the image already names a non-root
/// user. An image that cannot be inspected runs as [`NON_ROOT_USER`].
pub async fn run_as_user(image: &str, apple: bool, run_as_root: bool) -> Option<String> {
    if run_as_root {
        return None;
    }
    let user = image_config(image, apple)
        .await
        .and_then(|c| c.get("User").and_then(|u| u.as_str()).map(str::to_string))
        .unwrap_or_default();
    is_root_user(&user).then(|| NON_ROOT_USER.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[gateway#Gateway is the only way in#Compose apps get their own network]]
    #[test]
    fn compose_apps_are_moved_off_the_shared_network() {
        let compose = "services:\n  web:\n    image: x\n    networks: [amoeba-net]\nnetworks:\n  amoeba-net:\n    external: true\n";
        let isolated = isolate_compose_network(compose, "amoeba-app-web", false).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&isolated).unwrap();
        assert_eq!(doc["networks"]["amoeba-net"]["name"], "amoeba-app-web");
        assert_eq!(doc["networks"]["amoeba-net"]["external"], true);
        assert_eq!(doc["services"]["web"]["networks"][0], "amoeba-net");

        // A stack's own networks are left alone.
        let own = "services: {}\nnetworks:\n  backend: {}\n";
        assert_eq!(
            isolate_compose_network(own, "amoeba-app-web", false).unwrap(),
            own
        );

        // With egress, services also join the shared egress network.
        let online = isolate_compose_network(compose, "amoeba-app-web", true).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&online).unwrap();
        assert_eq!(doc["networks"][EGRESS_NETWORK]["name"], EGRESS_NETWORK);
        assert_eq!(doc["services"]["web"]["networks"][1], EGRESS_NETWORK);
    }

    #[test]
    fn recognizes_root_users() {
        for root in ["", "root", "0", "0:0", "root:root", " 0 "] {
            assert!(is_root_user(root), "{root:?}");
        }
        for other in ["lightpanda", "1000", "1000:1000", "node", "65534:65534"] {
            assert!(!is_root_user(other), "{other:?}");
        }
    }
}
