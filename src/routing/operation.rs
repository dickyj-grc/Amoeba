//! Maps HTTP method / JSON-RPC body to an operation (read, add, update, delete, custom)
//! and checks it against claims.roles and the service's access form.

use crate::auth::Claims;
use crate::config::schema::{OperationRule, ServiceConfig};

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

/// Picks the operation for a request: the first matching `operation_rules`
/// entry, else the HTTP-method mapping.
pub fn resolve_operation<'a>(service: &'a ServiceConfig, method: &str, subpath: &str) -> &'a str {
    service
        .operation_rules
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|rule| rule_matches(rule, method, subpath))
        .map(|rule| rule.operation.as_str())
        .unwrap_or_else(|| classify_operation(method))
}

fn rule_matches(rule: &OperationRule, method: &str, subpath: &str) -> bool {
    let method_ok = |required: bool| {
        if rule.values.is_empty() {
            !required
        } else {
            rule.values.iter().any(|v| v.eq_ignore_ascii_case(method))
        }
    };
    match rule.match_type.as_str() {
        "http_method" => method_ok(true),
        "path" => {
            method_ok(false)
                && rule
                    .path
                    .as_deref()
                    .is_some_and(|p| path_matches(p, subpath))
        }
        _ => false,
    }
}

/// Segment-wise match. `{name}` and `*` match exactly one segment; a final
/// `**` matches zero or more remaining segments. Leading/trailing slashes
/// are ignored on both sides.
pub fn path_matches(pattern: &str, subpath: &str) -> bool {
    let mut pat = pattern.split('/').filter(|s| !s.is_empty()).peekable();
    let mut path = subpath.split('/').filter(|s| !s.is_empty());
    while let Some(p) = pat.next() {
        if p == "**" && pat.peek().is_none() {
            return true;
        }
        let Some(seg) = path.next() else {
            return false;
        };
        let wildcard = p == "*" || (p.starts_with('{') && p.ends_with('}'));
        if !wildcard && p != seg {
            return false;
        }
    }
    path.next().is_none()
}

/// Load-time checks so a typo'd rule is rejected instead of never matching.
pub fn validate_operation_rules(name: &str, rules: &[OperationRule]) -> Result<(), String> {
    for rule in rules {
        if rule.operation.is_empty() {
            return Err(format!(
                "service '{name}' has an operation rule with no operation"
            ));
        }
        match rule.match_type.as_str() {
            "http_method" if rule.values.is_empty() => {
                return Err(format!(
                    "service '{name}' rule '{}' matches on http_method but lists no methods",
                    rule.operation
                ));
            }
            "http_method" => {}
            "path" => {
                let Some(pattern) = rule.path.as_deref() else {
                    return Err(format!(
                        "service '{name}' rule '{}' matches on path but sets no path",
                        rule.operation
                    ));
                };
                let segs: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
                if segs.iter().rev().skip(1).any(|s| *s == "**") {
                    return Err(format!(
                        "service '{name}' rule '{}': '**' is only allowed as the last segment",
                        rule.operation
                    ));
                }
            }
            other => {
                return Err(format!(
                    "service '{name}' rule '{}' has unknown match_type '{other}'",
                    rule.operation
                ));
            }
        }
    }
    Ok(())
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

    fn path_rule(operation: &str, path: &str, methods: &[&str]) -> OperationRule {
        OperationRule {
            operation: operation.into(),
            match_type: "path".into(),
            values: methods.iter().map(|m| m.to_string()).collect(),
            path: Some(path.into()),
        }
    }

    #[test]
    fn path_patterns_match_segments() {
        assert!(path_matches("loans", "loans"));
        assert!(path_matches("/loans/", "loans"));
        assert!(path_matches("loans/{id}", "loans/42"));
        assert!(path_matches("loans/*/payments", "loans/42/payments"));
        assert!(!path_matches("loans/{id}", "loans"));
        assert!(!path_matches("loans/{id}", "loans/42/payments"));
        assert!(!path_matches("loans", "leases"));
        assert!(path_matches("queries/**", "queries"));
        assert!(path_matches("queries/**", "queries/by-branch/run"));
        assert!(!path_matches("queries/**", "tables/loans"));
    }

    // @lat: [[gateway#Operation resolution]]
    #[test]
    fn resolves_named_operation_from_path_rules() {
        let mut svc = bare_service();
        svc.operation_rules = Some(vec![
            path_rule("loans.create", "tables/loans", &["POST"]),
            path_rule("loans.read", "tables/loans/**", &["get", "HEAD"]),
            path_rule("queries.run", "queries/{name}", &[]),
            OperationRule {
                operation: "purge".into(),
                match_type: "http_method".into(),
                values: vec!["DELETE".into()],
                path: None,
            },
        ]);
        assert_eq!(
            resolve_operation(&svc, "POST", "tables/loans"),
            "loans.create"
        );
        assert_eq!(
            resolve_operation(&svc, "GET", "tables/loans/7"),
            "loans.read"
        );
        assert_eq!(
            resolve_operation(&svc, "POST", "queries/by_branch"),
            "queries.run"
        );
        assert_eq!(resolve_operation(&svc, "DELETE", "tables/loans/7"), "purge");
        // No rule matches: method mapping.
        assert_eq!(resolve_operation(&svc, "PUT", "tables/loans/7"), "update");
        assert_eq!(resolve_operation(&bare_service(), "POST", "x"), "add");
    }

    #[test]
    fn rejects_malformed_operation_rules() {
        let ok = vec![path_rule("a", "x/**", &[])];
        assert!(validate_operation_rules("svc", &ok).is_ok());
        let no_path = vec![OperationRule {
            path: None,
            ..path_rule("a", "x", &[])
        }];
        assert!(validate_operation_rules("svc", &no_path).is_err());
        let mid_glob = vec![path_rule("a", "x/**/y", &[])];
        assert!(validate_operation_rules("svc", &mid_glob).is_err());
        let no_methods = vec![OperationRule {
            match_type: "http_method".into(),
            ..path_rule("a", "x", &[])
        }];
        assert!(validate_operation_rules("svc", &no_methods).is_err());
        let unknown = vec![OperationRule {
            match_type: "regex".into(),
            ..path_rule("a", "x", &[])
        }];
        assert!(validate_operation_rules("svc", &unknown).is_err());
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
