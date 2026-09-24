//! Decisions shared by agent deploys: which image name to register, which
//! access form to write, and whether a cold boot is fast enough to scale to zero.

use std::collections::HashMap;

use crate::routing::operation::scoped_role;

const OPERATIONS: [&str; 4] = ["read", "add", "update", "delete"];

/// Image reference stored in the catalog.
///
/// A blank registry is treated as unset, so the tag stays a local name
/// (`amoeba/<service>:latest`) instead of `//<service>:latest`.
pub fn image_reference(registry: Option<&str>, service: &str) -> String {
    match registry.map(str::trim).filter(|s| !s.is_empty()) {
        Some(registry) => format!("{registry}/{service}:latest"),
        None => format!("amoeba/{service}:latest"),
    }
}

/// `nixpacks build` accepts `--name` and does not accept `--tag`.
pub fn nixpacks_args(tag: &str) -> Vec<String> {
    vec!["build".into(), ".".into(), "--name".into(), tag.into()]
}

/// A local image does not need `docker pull`. Pulling it asks a registry and
/// fails when the tag was only built on this daemon.
pub fn should_pull_image(locally_present: bool) -> bool {
    !locally_present
}

/// Access fields written into an app manifest, plus the org and role the
/// deploy tool mints a token for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployAccess {
    pub tenant: Option<String>,
    pub permissions: HashMap<String, Vec<String>>,
    pub tenant_permissions: Option<HashMap<String, HashMap<String, Vec<String>>>>,
    pub token_org: Option<String>,
    pub token_role: String,
}

/// Builds the access form for a deploy.
///
/// * `tenant_permissions` is the multi-tenant form and must be sent on its own.
/// * `tenant` plus `permissions` is the single-tenant form.
/// * `permissions` alone stays tenant-unaware.
/// * Neither binds the service to `default_org` and grants only the scoped
///   role, so every other caller is still denied.
///
/// The scoped role is added to each operation the caller listed. An empty map
/// receives that role on read, add, update, and delete.
pub fn prepare_deploy_access(
    service: &str,
    tenant: Option<String>,
    permissions: HashMap<String, Vec<String>>,
    tenant_permissions: Option<HashMap<String, HashMap<String, Vec<String>>>>,
    default_org: &str,
) -> Result<DeployAccess, String> {
    let role = scoped_role(service);
    let tenant = tenant.filter(|t| !t.trim().is_empty());

    if let Some(mut by_org) = tenant_permissions {
        if tenant.is_some() || !permissions.is_empty() {
            return Err(
                "tenant_permissions is the whole policy; omit tenant and permissions".into(),
            );
        }
        if by_org.is_empty() {
            return Err("tenant_permissions must name at least one org".into());
        }
        for map in by_org.values_mut() {
            grant_role(map, &role);
        }
        let token_org = if by_org.contains_key(default_org) {
            default_org.to_string()
        } else {
            let mut keys: Vec<_> = by_org.keys().cloned().collect();
            keys.sort();
            keys.remove(0)
        };
        return Ok(DeployAccess {
            tenant: None,
            permissions: HashMap::new(),
            tenant_permissions: Some(by_org),
            token_org: Some(token_org),
            token_role: role,
        });
    }

    if let Some(tenant) = tenant {
        let mut permissions = permissions;
        grant_role(&mut permissions, &role);
        return Ok(DeployAccess {
            tenant: Some(tenant.clone()),
            permissions,
            tenant_permissions: None,
            token_org: Some(tenant),
            token_role: role,
        });
    }

    if !permissions.is_empty() {
        let mut permissions = permissions;
        grant_role(&mut permissions, &role);
        return Ok(DeployAccess {
            tenant: None,
            permissions,
            tenant_permissions: None,
            token_org: None,
            token_role: role,
        });
    }

    let mut permissions = HashMap::new();
    grant_role(&mut permissions, &role);
    Ok(DeployAccess {
        tenant: Some(default_org.to_string()),
        permissions,
        tenant_permissions: None,
        token_org: Some(default_org.to_string()),
        token_role: role,
    })
}

fn grant_role(map: &mut HashMap<String, Vec<String>>, role: &str) {
    if map.is_empty() {
        for op in OPERATIONS {
            map.insert(op.to_string(), vec![role.to_string()]);
        }
        return;
    }
    for roles in map.values_mut() {
        if !roles.iter().any(|existing| existing == role) {
            roles.push(role.to_string());
        }
    }
}

/// What a deploy observed after the service reached ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootObservation {
    pub boot_ms: u64,
    /// `None` when the filesystem check could not be run.
    pub local_writes: Option<u64>,
    /// `None` when the restart check could not be run.
    pub survived_restart: Option<bool>,
}

/// What to tell the caller about scale-to-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootAdvice {
    pub scale_to_zero: bool,
    pub notes: Vec<String>,
}

const SLOW_BOOT_MS: u64 = 10_000;

/// Cold boots slower than 10s, local filesystem writes, or a process that does
/// not come back after restart should stay always-on.
pub fn advise_boot(obs: &BootObservation) -> BootAdvice {
    let mut notes = Vec::new();
    if obs.boot_ms > SLOW_BOOT_MS {
        notes.push(format!(
            "boot took {}ms, past the {SLOW_BOOT_MS}ms cold-boot budget; leave cooldown unset or slim the image",
            obs.boot_ms
        ));
    }
    if let Some(writes) = obs.local_writes {
        if writes > 0 {
            notes.push(format!(
                "the container wrote {writes} paths on its local filesystem; scale-to-zero discards that state"
            ));
        }
    }
    if obs.survived_restart == Some(false) {
        notes.push(
            "the container process did not come back after a restart; leave cooldown unset until it survives a stop/start"
                .into(),
        );
    }
    BootAdvice {
        scale_to_zero: notes.is_empty(),
        notes,
    }
}

/// Counts non-empty lines of `docker diff` output.
pub fn count_diff_lines(diff: &str) -> u64 {
    diff.lines().filter(|line| !line.trim().is_empty()).count() as u64
}

/// Lifecycle state from `GET /admin/apps/:name`. The error text is `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleView {
    Missing,
    Pending,
    Ready,
    Error { message: String },
}

pub fn read_lifecycle(body: &serde_json::Value) -> LifecycleView {
    let state = body["state"]["state"].as_str().unwrap_or("");
    let message = body["state"]["message"].as_str().unwrap_or("").to_string();
    match state {
        "ready" => LifecycleView::Ready,
        "error" => LifecycleView::Error { message },
        "installed" | "pulling" => LifecycleView::Pending,
        "" => LifecycleView::Missing,
        _ => LifecycleView::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blank_registry_stays_a_local_image_name() {
        assert_eq!(image_reference(None, "pdf"), "amoeba/pdf:latest");
        assert_eq!(image_reference(Some(""), "pdf"), "amoeba/pdf:latest");
        assert_eq!(image_reference(Some("   "), "pdf"), "amoeba/pdf:latest");
        assert_eq!(
            image_reference(Some("registry.example/amoeba"), "pdf"),
            "registry.example/amoeba/pdf:latest"
        );
    }

    #[test]
    fn nixpacks_invocation_uses_name_not_tag() {
        let args = nixpacks_args("amoeba/pdf:latest");
        assert_eq!(args, vec!["build", ".", "--name", "amoeba/pdf:latest"]);
        assert!(!args.iter().any(|arg| arg == "--tag"));
    }

    #[test]
    fn local_images_skip_pull() {
        assert!(!should_pull_image(true));
        assert!(should_pull_image(false));
    }

    #[test]
    fn omitted_policy_becomes_a_single_tenant_scoped_role() {
        let access =
            prepare_deploy_access("pdf", None, HashMap::new(), None, "org_injani").unwrap();
        assert_eq!(access.tenant.as_deref(), Some("org_injani"));
        assert!(access.tenant_permissions.is_none());
        assert_eq!(access.token_role, "mcp:pdf");
        assert_eq!(access.token_org.as_deref(), Some("org_injani"));
        for op in ["read", "add", "update", "delete"] {
            assert_eq!(
                access.permissions.get(op).unwrap(),
                &vec!["mcp:pdf".to_string()]
            );
        }
    }

    #[test]
    fn single_tenant_keeps_caller_roles_and_adds_scoped_role() {
        let mut permissions = HashMap::new();
        permissions.insert("read".into(), vec!["analyst".into()]);
        let access = prepare_deploy_access(
            "pdf",
            Some("org_client_alpha".into()),
            permissions,
            None,
            "org_injani",
        )
        .unwrap();
        assert_eq!(access.tenant.as_deref(), Some("org_client_alpha"));
        assert_eq!(access.token_org.as_deref(), Some("org_client_alpha"));
        assert_eq!(
            access.permissions.get("read").unwrap(),
            &vec!["analyst".to_string(), "mcp:pdf".to_string()]
        );
        assert!(access.permissions.get("add").is_none());
    }

    #[test]
    fn multi_tenant_maps_stay_separate() {
        let mut alpha = HashMap::new();
        alpha.insert("read".into(), vec!["viewer".into()]);
        alpha.insert("add".into(), vec!["analyst".into()]);
        let mut beta = HashMap::new();
        beta.insert("read".into(), vec!["analyst".into()]);
        let access = prepare_deploy_access(
            "pdf",
            None,
            HashMap::new(),
            Some(HashMap::from([
                ("org_client_beta".into(), beta),
                ("org_client_alpha".into(), alpha),
            ])),
            "org_client_beta",
        )
        .unwrap();
        assert!(access.tenant.is_none());
        assert!(access.permissions.is_empty());
        assert_eq!(access.token_org.as_deref(), Some("org_client_beta"));
        let by_org = access.tenant_permissions.unwrap();
        assert_eq!(
            by_org.get("org_client_alpha").unwrap().get("add").unwrap(),
            &vec!["analyst".to_string(), "mcp:pdf".to_string()]
        );
        assert!(by_org.get("org_client_beta").unwrap().get("add").is_none());
        assert_eq!(
            by_org.get("org_client_beta").unwrap().get("read").unwrap(),
            &vec!["analyst".to_string(), "mcp:pdf".to_string()]
        );
    }

    #[test]
    fn multi_tenant_rejects_a_second_form() {
        let err = prepare_deploy_access(
            "pdf",
            Some("org_a".into()),
            HashMap::new(),
            Some(HashMap::from([("org_a".into(), HashMap::new())])),
            "org_a",
        );
        assert!(err.is_err());
    }

    #[test]
    fn slow_boot_local_writes_and_failed_restart_stay_always_on() {
        let fast = advise_boot(&BootObservation {
            boot_ms: 400,
            local_writes: Some(0),
            survived_restart: Some(true),
        });
        assert!(fast.scale_to_zero);
        assert!(fast.notes.is_empty());

        let slow = advise_boot(&BootObservation {
            boot_ms: 10_001,
            local_writes: Some(2),
            survived_restart: Some(false),
        });
        assert!(!slow.scale_to_zero);
        assert_eq!(slow.notes.len(), 3);

        let skipped = advise_boot(&BootObservation {
            boot_ms: 100,
            local_writes: None,
            survived_restart: None,
        });
        assert!(skipped.scale_to_zero);
    }

    #[test]
    fn lifecycle_error_uses_message_field() {
        let body = json!({
            "name": "pdf",
            "state": { "state": "error", "message": "image pull failed", "updated_at": 1 }
        });
        assert_eq!(
            read_lifecycle(&body),
            LifecycleView::Error {
                message: "image pull failed".into()
            }
        );
        assert_eq!(
            read_lifecycle(&json!({"state": {"state": "ready"}})),
            LifecycleView::Ready
        );
        assert_eq!(
            read_lifecycle(&json!({"state": {"state": "pulling"}})),
            LifecycleView::Pending
        );
    }

    #[test]
    fn diff_line_count_skips_blank_lines() {
        assert_eq!(count_diff_lines("A /tmp/x\n\nC /app/data\n"), 2);
    }
}
