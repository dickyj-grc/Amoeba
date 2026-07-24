//! Local admin CLI for managing `/etc/amoeba/users.json`.
//!
//! Usage:
//!   amoeba-admin add-user <username> <password> --roles r1,r2 --org <org_id> [--users-file <path>]
//!   amoeba-admin delete-user <username> [--users-file <path>]
//!   amoeba-admin update-user <username> [--password <new>] [--roles r1,r2] [--org <org_id>] [--users-file <path>]

use amoeba::auth::users::UserStore;
use std::env;
use std::process::ExitCode;

const DEFAULT_USERS_FILE: &str = "/etc/amoeba/users.json";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        Some("add-user") => run_add_user(&args[1..]),
        Some("delete-user") => run_delete_user(&args[1..]),
        Some("update-user") => run_update_user(&args[1..]),
        _ => {
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n\
         \x20 amoeba-admin add-user <username> <password> --roles r1,r2 --org <org_id> [--users-file <path>]\n\
         \x20 amoeba-admin delete-user <username> [--users-file <path>]\n\
         \x20 amoeba-admin update-user <username> [--password <new>] [--roles r1,r2] [--org <org_id>] [--users-file <path>]"
    );
}

struct Flags {
    positional: Vec<String>,
    users_file: String,
    password: Option<String>,
    roles: Option<Vec<String>>,
    org: Option<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut positional = Vec::new();
    let mut users_file = DEFAULT_USERS_FILE.to_string();
    let mut password = None;
    let mut roles = None;
    let mut org = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--users-file" => users_file = next_value(args, &mut i, "--users-file")?,
            "--password" => password = Some(next_value(args, &mut i, "--password")?),
            "--roles" => {
                let raw = next_value(args, &mut i, "--roles")?;
                roles = Some(raw.split(',').map(|s| s.trim().to_string()).collect());
            }
            "--org" => org = Some(next_value(args, &mut i, "--org")?),
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }

    Ok(Flags {
        positional,
        users_file,
        password,
        roles,
        org,
    })
}

fn next_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    let value = args
        .get(*i + 1)
        .ok_or_else(|| format!("missing value for {flag}"))?
        .clone();
    *i += 2;
    Ok(value)
}

fn run_add_user(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let [username, password] = flags.positional.as_slice() else {
        return Err("usage: add-user <username> <password> --roles r1,r2 --org <org_id>".into());
    };
    let roles = flags.roles.ok_or("missing required --roles")?;
    let org = flags.org.ok_or("missing required --org")?;

    let mut store = UserStore::load(&flags.users_file).map_err(|e| e.to_string())?;
    store
        .add_user(username, password, roles, org)
        .map_err(|e| e.to_string())?;
    store.save(&flags.users_file).map_err(|e| e.to_string())?;

    println!("added user '{username}' to {}", flags.users_file);
    Ok(())
}

fn run_delete_user(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let [username] = flags.positional.as_slice() else {
        return Err("usage: delete-user <username>".into());
    };

    let mut store = UserStore::load(&flags.users_file).map_err(|e| e.to_string())?;
    store.delete_user(username).map_err(|e| e.to_string())?;
    store.save(&flags.users_file).map_err(|e| e.to_string())?;

    println!("deleted user '{username}' from {}", flags.users_file);
    Ok(())
}

fn run_update_user(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let [username] = flags.positional.as_slice() else {
        return Err(
            "usage: update-user <username> [--password <new>] [--roles r1,r2] [--org <org_id>]"
                .into(),
        );
    };

    if flags.password.is_none() && flags.roles.is_none() && flags.org.is_none() {
        return Err("nothing to update: pass --password, --roles, and/or --org".into());
    }

    let mut store = UserStore::load(&flags.users_file).map_err(|e| e.to_string())?;
    store
        .update_user(username, flags.password.as_deref(), flags.roles, flags.org)
        .map_err(|e| e.to_string())?;
    store.save(&flags.users_file).map_err(|e| e.to_string())?;

    println!("updated user '{username}' in {}", flags.users_file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_add_user_flags() {
        let flags = parse_flags(&args(&[
            "dicky", "hunter2", "--roles", "admin,analyst", "--org", "org_hq",
        ]))
        .unwrap();

        assert_eq!(
            flags.positional,
            vec!["dicky".to_string(), "hunter2".to_string()]
        );
        assert_eq!(
            flags.roles,
            Some(vec!["admin".to_string(), "analyst".to_string()])
        );
        assert_eq!(flags.org, Some("org_hq".to_string()));
        assert_eq!(flags.users_file, DEFAULT_USERS_FILE);
    }

    #[test]
    fn parses_custom_users_file() {
        let flags = parse_flags(&args(&["dicky", "--users-file", "/tmp/users.json"])).unwrap();
        assert_eq!(flags.users_file, "/tmp/users.json");
    }

    #[test]
    fn errors_when_flag_value_missing() {
        assert!(parse_flags(&args(&["dicky", "--roles"])).is_err());
    }

    #[test]
    fn update_user_requires_at_least_one_field() {
        let err = run_update_user(&args(&["dicky"])).unwrap_err();
        assert!(err.contains("nothing to update"));
    }
}
