//! First run with nothing set up, for the one-command quick start
//! (`docker-compose.quickstart.yml`). Opt-in with `AMOEBA_BOOTSTRAP=1`:
//! without it a missing signing secret stays a startup error.
//!
//! With it, and with no `config.toml` or `AMOEBA_LOCAL_JWT_SECRET`, Amoeba
//! keeps a generated secret next to its other state, creates an `admin` user
//! when there are no users, and writes an empty service catalog when there is
//! none. Nothing that already exists is changed.

use crate::auth::users::UserStore;
use rand_core::{OsRng, RngCore};
use std::path::Path;

pub const BOOTSTRAP_ENV: &str = "AMOEBA_BOOTSTRAP";
/// The first admin's password, readable only by the owner.
pub const ADMIN_PASSWORD_FILE: &str = "admin-password";
const SECRET_FILE: &str = "jwt_secret";

pub fn enabled() -> bool {
    std::env::var(BOOTSTRAP_ENV).is_ok_and(|v| v == "1" || v == "true")
}

/// The signing secret kept in `dir`, created on first use.
pub fn jwt_secret(dir: &Path) -> Result<String, String> {
    let path = dir.join(SECRET_FILE);
    match std::fs::read_to_string(&path) {
        Ok(secret) if !secret.trim().is_empty() => return Ok(secret.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    }
    let secret = random_hex(32);
    write_private(&path, &secret)?;
    Ok(secret)
}

/// Creates `admin` (roles `admin`, org `default`) when `users_file` has no
/// users. Returns the new password, also written to `dir/admin-password`.
pub fn ensure_admin(users_file: &str, dir: &Path) -> Result<Option<String>, String> {
    let mut store = UserStore::load(users_file).map_err(|e| e.to_string())?;
    if !store.users.is_empty() {
        return Ok(None);
    }
    let password = random_password(20);
    store
        .add_user("admin", &password, vec!["admin".into()], "default".into())
        .map_err(|e| e.to_string())?;
    store.save(users_file).map_err(|e| e.to_string())?;
    write_private(&dir.join(ADMIN_PASSWORD_FILE), &password)?;
    Ok(Some(password))
}

/// Writes an empty catalog for one local Docker machine when there is none.
/// Returns whether it wrote one.
pub fn ensure_catalog(services_file: &str) -> Result<bool, String> {
    let path = Path::new(services_file);
    if path.exists() {
        return Ok(false);
    }
    let catalog = r#"{
  "version": 1,
  "machines": {
    "local": { "type": "vm", "drivers": ["docker"], "resources": {} }
  },
  "services": {}
}
"#;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, catalog).map_err(|e| format!("cannot write {services_file}: {e}"))?;
    Ok(true)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_password(len: usize) -> String {
    // No look-alike characters, so it can be read off a log and typed.
    const CHARSET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let byte = (OsRng.next_u32() & 0xff) as usize;
        // Reject the tail of the byte range to keep every character equally likely.
        if byte < 256 - 256 % CHARSET.len() {
            out.push(CHARSET[byte % CHARSET.len()] as char);
        }
    }
    out
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[dev#Quick start#First run]]
    #[test]
    fn first_run_creates_secret_admin_and_catalog_once() {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users.json");
        let users = users.to_str().unwrap();
        let catalog = dir.path().join("services.json");
        let catalog = catalog.to_str().unwrap();

        let secret = jwt_secret(dir.path()).unwrap();
        assert_eq!(secret.len(), 64);
        assert_eq!(
            jwt_secret(dir.path()).unwrap(),
            secret,
            "kept across restarts"
        );

        let password = ensure_admin(users, dir.path())
            .unwrap()
            .expect("admin created");
        assert_eq!(password.len(), 20);
        let store = UserStore::load(users).unwrap();
        assert_eq!(store.users["admin"].roles, ["admin"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(ADMIN_PASSWORD_FILE)).unwrap(),
            password
        );
        assert_eq!(
            ensure_admin(users, dir.path()).unwrap(),
            None,
            "never twice"
        );

        assert!(ensure_catalog(catalog).unwrap());
        assert!(!ensure_catalog(catalog).unwrap());
        let loaded = crate::config::watcher::load_catalog(catalog).unwrap();
        assert!(loaded.machines.contains_key("local"));
    }
}
