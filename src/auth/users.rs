//! Local user store for local_jwt mode: username -> password hash, roles, org_id.
//! Backs `/etc/amoeba/users.json` (see `config/users.example.json`).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct User {
    pub password_hash: String,
    pub roles: Vec<String>,
    pub org_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct UserStore {
    pub users: HashMap<String, User>,
}

#[derive(Debug)]
pub enum UserStoreError {
    AlreadyExists(String),
    NotFound(String),
    Hash(String),
    Io(String),
}

impl fmt::Display for UserStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserStoreError::AlreadyExists(u) => write!(f, "user '{u}' already exists"),
            UserStoreError::NotFound(u) => write!(f, "user '{u}' not found"),
            UserStoreError::Hash(e) => write!(f, "password hashing failed: {e}"),
            UserStoreError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for UserStoreError {}

/// Hashes a plaintext password using Argon2id with a fresh random salt.
pub fn hash_password(password: &str) -> Result<String, UserStoreError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| UserStoreError::Hash(e.to_string()))
}

/// Verifies a plaintext password against a stored Argon2 hash string.
pub fn verify_password(password: &str, password_hash: &str) -> bool {
    let Ok(parsed_hash) = PasswordHash::new(password_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

impl UserStore {
    /// Loads the store from disk, returning an empty store if the file doesn't exist yet.
    pub fn load(path: &str) -> Result<Self, UserStoreError> {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                serde_json::from_str(&content).map_err(|e| UserStoreError::Io(e.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(UserStoreError::Io(e.to_string())),
        }
    }

    /// Writes the store to disk as pretty-printed JSON, creating parent directories as needed.
    /// The file is written with `0600` permissions (owner read/write only) since it holds
    /// password hashes.
    pub fn save(&self, path: &str) -> Result<(), UserStoreError> {
        let content =
            serde_json::to_string_pretty(self).map_err(|e| UserStoreError::Io(e.to_string()))?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| UserStoreError::Io(e.to_string()))?;
        }
        std::fs::write(path, content).map_err(|e| UserStoreError::Io(e.to_string()))?;
        restrict_to_owner(path)
    }

    /// Registers a new user with a freshly hashed password. Fails if the username is taken.
    pub fn add_user(
        &mut self,
        username: &str,
        password: &str,
        roles: Vec<String>,
        org_id: String,
    ) -> Result<(), UserStoreError> {
        if self.users.contains_key(username) {
            return Err(UserStoreError::AlreadyExists(username.to_string()));
        }
        let password_hash = hash_password(password)?;
        self.users.insert(
            username.to_string(),
            User {
                password_hash,
                roles,
                org_id,
            },
        );
        Ok(())
    }

    /// Removes a user. Fails if the username doesn't exist.
    pub fn delete_user(&mut self, username: &str) -> Result<(), UserStoreError> {
        self.users
            .remove(username)
            .map(|_| ())
            .ok_or_else(|| UserStoreError::NotFound(username.to_string()))
    }

    /// Updates an existing user's password, roles, and/or org_id. Fields left as
    /// `None` are left unchanged. Fails if the username doesn't exist.
    pub fn update_user(
        &mut self,
        username: &str,
        new_password: Option<&str>,
        new_roles: Option<Vec<String>>,
        new_org_id: Option<String>,
    ) -> Result<(), UserStoreError> {
        let user = self
            .users
            .get_mut(username)
            .ok_or_else(|| UserStoreError::NotFound(username.to_string()))?;

        if let Some(password) = new_password {
            user.password_hash = hash_password(password)?;
        }
        if let Some(roles) = new_roles {
            user.roles = roles;
        }
        if let Some(org_id) = new_org_id {
            user.org_id = org_id;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &str) -> Result<(), UserStoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| UserStoreError::Io(e.to_string()))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &str) -> Result<(), UserStoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies_a_password() {
        let hash = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &hash));
        assert!(!verify_password("wrong-password", &hash));
    }

    #[test]
    fn each_hash_uses_a_unique_salt() {
        let a = hash_password("same-password").unwrap();
        let b = hash_password("same-password").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn adds_a_new_user() {
        let mut store = UserStore::default();
        store
            .add_user("dicky", "hunter2", vec!["admin".into()], "org_hq".into())
            .unwrap();

        let user = store.users.get("dicky").unwrap();
        assert_eq!(user.roles, vec!["admin".to_string()]);
        assert_eq!(user.org_id, "org_hq");
        assert!(verify_password("hunter2", &user.password_hash));
    }

    #[test]
    fn rejects_adding_a_duplicate_username() {
        let mut store = UserStore::default();
        store
            .add_user("dicky", "hunter2", vec!["admin".into()], "org_hq".into())
            .unwrap();

        let err = store
            .add_user("dicky", "different", vec!["viewer".into()], "org_hq".into())
            .unwrap_err();

        assert!(matches!(err, UserStoreError::AlreadyExists(u) if u == "dicky"));
    }

    #[test]
    fn deletes_an_existing_user() {
        let mut store = UserStore::default();
        store
            .add_user("dicky", "hunter2", vec!["admin".into()], "org_hq".into())
            .unwrap();

        store.delete_user("dicky").unwrap();
        assert!(!store.users.contains_key("dicky"));
    }

    #[test]
    fn rejects_deleting_an_unknown_user() {
        let mut store = UserStore::default();
        let err = store.delete_user("ghost").unwrap_err();
        assert!(matches!(err, UserStoreError::NotFound(u) if u == "ghost"));
    }

    #[test]
    fn updates_password_roles_and_org_independently() {
        let mut store = UserStore::default();
        store
            .add_user("dicky", "hunter2", vec!["viewer".into()], "org_hq".into())
            .unwrap();
        let old_hash = store.users.get("dicky").unwrap().password_hash.clone();

        store
            .update_user("dicky", None, Some(vec!["admin".into()]), None)
            .unwrap();
        let user = store.users.get("dicky").unwrap();
        assert_eq!(user.roles, vec!["admin".to_string()]);
        assert_eq!(user.password_hash, old_hash);
        assert_eq!(user.org_id, "org_hq");

        store
            .update_user("dicky", Some("new-password"), None, Some("org_new".into()))
            .unwrap();
        let user = store.users.get("dicky").unwrap();
        assert_ne!(user.password_hash, old_hash);
        assert!(verify_password("new-password", &user.password_hash));
        assert_eq!(user.org_id, "org_new");
        assert_eq!(user.roles, vec!["admin".to_string()]);
    }

    #[test]
    fn rejects_updating_an_unknown_user() {
        let mut store = UserStore::default();
        let err = store
            .update_user("ghost", Some("pw"), None, None)
            .unwrap_err();
        assert!(matches!(err, UserStoreError::NotFound(u) if u == "ghost"));
    }

    #[test]
    fn load_returns_empty_store_when_file_missing() {
        let store = UserStore::load("/nonexistent/path/users.json").unwrap();
        assert!(store.users.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn save_restricts_file_permissions_to_owner() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "amoeba-users-test-{}-{}.json",
            std::process::id(),
            "save_restricts_file_permissions_to_owner"
        ));
        let path_str = path.to_str().unwrap();

        let mut store = UserStore::default();
        store
            .add_user("dicky", "hunter2", vec!["admin".into()], "org_hq".into())
            .unwrap();
        store.save(path_str).unwrap();

        let mode = std::fs::metadata(path_str).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_file(path_str).unwrap();
    }
}
