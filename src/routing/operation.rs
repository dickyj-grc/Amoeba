//! Maps HTTP method / JSON-RPC body to an operation (read, add, update, delete, custom)
//! and checks it against claims.roles and service.permissions.

use crate::auth::Claims;

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
}
