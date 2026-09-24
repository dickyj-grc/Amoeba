//! Maps HTTP method / JSON-RPC body to an operation (read, add, update, delete, custom)
//! and checks it against claims.roles and the service's access form.

use crate::auth::Claims;
use crate::config::schema::ServiceConfig;

/// Maps an HTTP method to a coarse-grained operation name.
pub fn classify_operation(method: &str) -> &'static str {
    match method {
        "GET" | "HEAD" => "read",
        "POST" => "add",
        "PUT" | "PATCH" => "update",
        "DELETE" => "delete",
        _ => "read",
    }
}

/// Checks whether any of the caller's roles are present in the allowed role list.
pub fn has_permission(claims: &Claims, allowed_roles: &[String]) -> bool {
    claims.roles.iter().any(|r| allowed_roles.contains(r))
}

/// Why a caller was denied. The admin role is not special here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDeny {
    /// The service is bound to one or more orgs and this token's org is not one of them.
    Tenant,
    /// The org is allowed, but no role on the token is granted for this operation.
    Role,
}

/// Selects the permission map for this caller and checks the operation.
///
/// * `tenant_permissions` — that org's map only.
/// * `tenant` — `permissions`, and only for that org.
/// * neither — `permissions` for any org, including a token with no org.
pub fn authorize(
    service: &ServiceConfig,
    claims: &Claims,
    operation: &str,
) -> Result<(), AccessDeny> {
    let allowed = match (&service.tenant_permissions, &service.tenant) {
        (Some(by_org), None) => {
            let org = claims.org_id.as_deref().ok_or(AccessDeny::Tenant)?;
            let map = by_org.get(org).ok_or(AccessDeny::Tenant)?;
            map.get(operation).map(Vec::as_slice).unwrap_or(&[])
        }
        (None, Some(tenant)) => {
            match claims.org_id.as_deref() {
                Some(org) if org == tenant => {}
                _ => return Err(AccessDeny::Tenant),
            }
            service
                .permissions
                .get(operation)
                .map(Vec::as_slice)
                .unwrap_or(&[])
        }
        (None, None) => service
            .permissions
            .get(operation)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        (Some(_), Some(_)) => return Err(AccessDeny::Tenant),
    };

    if has_permission(claims, allowed) {
        Ok(())
    } else {
        Err(AccessDeny::Role)
    }
}

/// Role minted for the caller that deployed a service. It is an ordinary role:
/// access still depends on the service granting it.
pub fn scoped_role(service: &str) -> String {
    format!("mcp:{service}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_standard_http_methods() {
        assert_eq!(classify_operation("GET"), "read");
        assert_eq!(classify_operation("HEAD"), "read");
        assert_eq!(classify_operation("POST"), "add");
        assert_eq!(classify_operation("PUT"), "update");
        assert_eq!(classify_operation("PATCH"), "update");
        assert_eq!(classify_operation("DELETE"), "delete");
    }

    #[test]
    fn defaults_unknown_methods_to_read() {
        assert_eq!(classify_operation("OPTIONS"), "read");
    }

    fn claims_with_roles(roles: &[&str]) -> Claims {
        Claims {
            sub: "user".into(),
            org_id: None,
            roles: roles.iter().map(|r| r.to_string()).collect(),
            exp: 0,
            jti: "jti-test".into(),
        }
    }

    #[test]
    fn grants_permission_when_a_role_matches() {
        let claims = claims_with_roles(&["admin"]);
        let allowed = vec!["admin".to_string(), "analyst".to_string()];
        assert!(has_permission(&claims, &allowed));
    }

    #[test]
    fn denies_permission_when_no_role_matches() {
        let claims = claims_with_roles(&["viewer"]);
        let allowed = vec!["admin".to_string()];
        assert!(!has_permission(&claims, &allowed));
    }

    #[test]
    fn denies_permission_when_allowed_list_is_empty() {
        let claims = claims_with_roles(&["admin"]);
        assert!(!has_permission(&claims, &[]));
    }

    fn claims_for(org: Option<&str>, roles: &[&str]) -> Claims {
        let mut claims = claims_with_roles(roles);
        claims.org_id = org.map(str::to_string);
        claims
    }

    fn bare_service() -> ServiceConfig {
        ServiceConfig {
            placement: crate::config::schema::Placement {
                r#type: "vm".into(),
                ip: Some("svc".into()),
                primary_service: None,
                port: 80,
                cooldown_seconds: None,
                machine: None,
            },
            container: None,
            stack_spec: None,
            permissions: HashMap::new(),
            tenant: None,
            tenant_permissions: None,
            upstream_auth: None,
            public: false,
            operation_rules: None,
            env_from_secret: HashMap::new(),
            header_from_secret: HashMap::new(),
        }
    }

    use std::collections::HashMap;

    #[test]
    fn tenant_unaware_service_ignores_org() {
        let mut svc = bare_service();
        svc.permissions
            .insert("read".into(), vec!["analyst".into()]);
        let alpha = claims_for(Some("org_client_alpha"), &["analyst"]);
        let beta = claims_for(Some("org_client_beta"), &["analyst"]);
        assert!(authorize(&svc, &alpha, "read").is_ok());
        assert!(authorize(&svc, &beta, "read").is_ok());
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_alpha"), &["admin"]),
                "read"
            ),
            Err(AccessDeny::Role)
        );
    }

    #[test]
    fn single_tenant_service_rejects_other_orgs() {
        let mut svc = bare_service();
        svc.tenant = Some("org_client_alpha".into());
        svc.permissions
            .insert("read".into(), vec!["analyst".into()]);
        svc.permissions
            .insert("delete".into(), vec!["admin".into()]);

        assert!(
            authorize(
                &svc,
                &claims_for(Some("org_client_alpha"), &["analyst"]),
                "read"
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_beta"), &["analyst"]),
                "read"
            ),
            Err(AccessDeny::Tenant)
        );
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_alpha"), &["analyst"]),
                "delete"
            ),
            Err(AccessDeny::Role)
        );
        assert_eq!(
            authorize(&svc, &claims_for(None, &["admin"]), "read"),
            Err(AccessDeny::Tenant)
        );
    }

    #[test]
    fn multi_tenant_service_uses_each_orgs_own_map() {
        let mut svc = bare_service();
        let mut alpha = HashMap::new();
        alpha.insert("read".into(), vec!["viewer".into(), "analyst".into()]);
        alpha.insert("add".into(), vec!["analyst".into()]);
        let mut beta = HashMap::new();
        beta.insert("read".into(), vec!["analyst".into()]);
        svc.tenant_permissions = Some(HashMap::from([
            ("org_client_alpha".into(), alpha),
            ("org_client_beta".into(), beta),
        ]));

        assert!(
            authorize(
                &svc,
                &claims_for(Some("org_client_alpha"), &["viewer"]),
                "read"
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_beta"), &["viewer"]),
                "read"
            ),
            Err(AccessDeny::Role)
        );
        assert!(
            authorize(
                &svc,
                &claims_for(Some("org_client_beta"), &["analyst"]),
                "read"
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_beta"), &["analyst"]),
                "add"
            ),
            Err(AccessDeny::Role)
        );
        assert!(
            authorize(
                &svc,
                &claims_for(Some("org_client_alpha"), &["analyst"]),
                "add"
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                &svc,
                &claims_for(Some("org_client_gamma"), &["admin"]),
                "read"
            ),
            Err(AccessDeny::Tenant)
        );
    }
}
