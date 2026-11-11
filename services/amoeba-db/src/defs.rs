//! Approved definitions: the tables and named queries this service exposes,
//! and who may see which columns. Loaded from JSON and validated before use;
//! nothing a caller sends can add to them.

use serde::Deserialize;
use std::collections::BTreeMap;

/// What a caller gets for one column. Ordered from least to most permissive,
/// so the most permissive of several roles is a plain `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Hide,
    Mask,
    Show,
}

/// Per-column policy: a default for everyone plus roles that get more.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnPolicy {
    #[serde(default = "default_visibility")]
    pub default: Visibility,
    #[serde(default)]
    pub mask: Vec<String>,
    #[serde(default)]
    pub show: Vec<String>,
}

fn default_visibility() -> Visibility {
    Visibility::Hide
}

impl ColumnPolicy {
    /// The most permissive visibility any of `roles` (or the default) grants.
    pub fn for_roles(&self, roles: &[String]) -> Visibility {
        let mut v = self.default;
        if roles.iter().any(|r| self.mask.contains(r)) {
            v = v.max(Visibility::Mask);
        }
        if roles.iter().any(|r| self.show.contains(r)) {
            v = v.max(Visibility::Show);
        }
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableOp {
    Select,
    Insert,
    Update,
    Delete,
}

impl TableOp {
    pub fn as_str(self) -> &'static str {
        match self {
            TableOp::Select => "select",
            TableOp::Insert => "insert",
            TableOp::Update => "update",
            TableOp::Delete => "delete",
        }
    }
}

/// A table opened for specific operations. The service generates the SQL.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableDef {
    #[serde(default)]
    pub description: String,
    /// Column that addresses one row in `/tables/<t>/<key>`.
    #[serde(default = "default_key")]
    pub key: String,
    /// When set, rows are limited to the caller's org: reads filter on it,
    /// inserts fill it, and updates may not change it.
    #[serde(default)]
    pub org_column: Option<String>,
    /// Most rows one select returns.
    #[serde(default = "default_row_limit")]
    pub row_limit: u32,
    /// Most rows one update or delete may touch; more rolls the write back.
    #[serde(default = "default_write_row_limit")]
    pub write_row_limit: u32,
    /// Roles allowed per operation. `"*"` means any caller.
    pub operations: BTreeMap<TableOp, Vec<String>>,
    /// Columns are closed unless listed here.
    pub columns: BTreeMap<String, ColumnPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    #[default]
    Text,
    Integer,
    Number,
    Boolean,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: ParamType,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub description: String,
}

/// A named SQL statement. `$1..$n` bind `params` in order; every value
/// arrives as text, so the SQL casts where it needs another type (`$1::int`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryDef {
    #[serde(default)]
    pub description: String,
    pub mode: QueryMode,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<ParamDef>,
    /// Read: most rows returned. Write: most rows affected before rollback.
    #[serde(default = "default_row_limit")]
    pub row_limit: u32,
    /// Roles allowed to run it. `"*"` means any caller.
    pub roles: Vec<String>,
    /// Output columns of a read query and their policy; required for reads.
    #[serde(default)]
    pub columns: BTreeMap<String, ColumnPolicy>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definitions {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default = "default_timeout")]
    pub statement_timeout_ms: u64,
    #[serde(default)]
    pub tables: BTreeMap<String, TableDef>,
    #[serde(default)]
    pub queries: BTreeMap<String, QueryDef>,
}

fn default_key() -> String {
    "id".into()
}
fn default_row_limit() -> u32 {
    500
}
fn default_write_row_limit() -> u32 {
    1
}
fn default_true() -> bool {
    true
}
fn default_schema() -> String {
    "public".into()
}
fn default_timeout() -> u64 {
    5000
}

/// Whether a role list admits a caller with `roles`.
pub fn allows(allowed: &[String], roles: &[String]) -> bool {
    allowed.iter().any(|a| a == "*" || roles.contains(a))
}

/// Unquoted Postgres identifier this service will quote and embed in SQL.
/// Lowercase only, since Postgres folds unquoted names to lowercase.
pub fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && s.len() <= 63
}

/// Query names appear in URLs and MCP tool names.
fn is_query_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && s.len() <= 64
}

/// Highest `$n` placeholder in `sql`, ignoring quoted text and comments.
pub fn max_placeholder(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut max = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'$' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    max = max.max(sql[start..end].parse().unwrap_or(0));
                }
                i = end.max(i + 1);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    max
}

impl Definitions {
    pub fn parse(json: &str) -> Result<Self, String> {
        let defs: Definitions =
            serde_json::from_str(json).map_err(|e| format!("invalid definitions: {e}"))?;
        defs.validate()?;
        Ok(defs)
    }

    /// Rejects anything that would be unsafe to embed in SQL or that could
    /// never work, so a bad definition fails at load rather than per call.
    pub fn validate(&self) -> Result<(), String> {
        if !is_ident(&self.schema) {
            return Err(format!(
                "schema '{}' is not a valid identifier",
                self.schema
            ));
        }
        if self.statement_timeout_ms == 0 {
            return Err("statement_timeout_ms must be greater than 0".into());
        }
        for (name, t) in &self.tables {
            let ctx = |msg: String| format!("table '{name}': {msg}");
            if !is_ident(name) {
                return Err(ctx("name is not a valid identifier".into()));
            }
            // MCP tool names for queries start with `query_`.
            if name == "query" || name.starts_with("query_") {
                return Err(ctx("name may not be 'query' or start with 'query_'".into()));
            }
            for col in t.columns.keys().chain([&t.key]).chain(t.org_column.iter()) {
                if !is_ident(col) {
                    return Err(ctx(format!("column '{col}' is not a valid identifier")));
                }
            }
            if t.operations.is_empty() {
                return Err(ctx("opens no operations".into()));
            }
            if t.row_limit == 0 || t.write_row_limit == 0 {
                return Err(ctx("row limits must be greater than 0".into()));
            }
        }
        for (name, q) in &self.queries {
            let ctx = |msg: String| format!("query '{name}': {msg}");
            if !is_query_name(name) {
                return Err(ctx(
                    "name must be lowercase letters, digits, '_' or '-'".into()
                ));
            }
            if q.sql.trim().is_empty() {
                return Err(ctx("sql is empty".into()));
            }
            if q.row_limit == 0 {
                return Err(ctx("row_limit must be greater than 0".into()));
            }
            match q.mode {
                QueryMode::Read if q.columns.is_empty() => {
                    return Err(ctx(
                        "a read query must list its output columns; columns are closed unless listed"
                            .into(),
                    ));
                }
                QueryMode::Write if !q.columns.is_empty() => {
                    return Err(ctx("a write query returns no columns".into()));
                }
                _ => {}
            }
            for col in q.columns.keys() {
                if !is_ident(col) {
                    return Err(ctx(format!("column '{col}' is not a valid identifier")));
                }
            }
            let mut seen = std::collections::HashSet::new();
            for p in &q.params {
                if !is_ident(&p.name) || !seen.insert(&p.name) {
                    return Err(ctx(format!(
                        "param '{}' is not a valid, unique identifier",
                        p.name
                    )));
                }
            }
            let used = max_placeholder(&q.sql);
            if used != q.params.len() {
                return Err(ctx(format!(
                    "sql uses ${used} placeholders but {} params are declared",
                    q.params.len()
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    fn policy(json: &str) -> ColumnPolicy {
        serde_json::from_str(json).unwrap()
    }

    // @lat: [[database#Column policy]]
    #[test]
    fn most_permissive_role_wins_and_default_is_hide() {
        let nik = policy(r#"{"mask":["cs_agent"],"show":["compliance"]}"#);
        assert_eq!(nik.for_roles(&roles(&[])), Visibility::Hide);
        assert_eq!(nik.for_roles(&roles(&["viewer"])), Visibility::Hide);
        assert_eq!(nik.for_roles(&roles(&["cs_agent"])), Visibility::Mask);
        assert_eq!(
            nik.for_roles(&roles(&["cs_agent", "compliance"])),
            Visibility::Show
        );
        let name = policy(r#"{"default":"show"}"#);
        assert_eq!(name.for_roles(&roles(&[])), Visibility::Show);
    }

    // @lat: [[database#Definitions#Load-time validation]]
    #[test]
    fn rejects_definitions_that_are_unsafe_or_cannot_work() {
        let base = |extra: &str| format!(r#"{{"tables":{{}},"queries":{{{extra}}}}}"#);
        let cases = [
            (
                r#""q":{"mode":"read","sql":"SELECT 1 AS a","roles":["*"]}"#,
                "output columns",
            ),
            (
                r#""q":{"mode":"read","sql":"SELECT $1 AS a","roles":["*"],"columns":{"a":{}}}"#,
                "placeholders",
            ),
            (
                r#""Bad Name":{"mode":"write","sql":"SELECT 1","roles":["*"]}"#,
                "name must be",
            ),
            (
                r#""q":{"mode":"read","sql":"SELECT 1 AS a","roles":["*"],"columns":{"a\"; drop":{}}}"#,
                "not a valid identifier",
            ),
        ];
        for (q, why) in cases {
            let err = Definitions::parse(&base(q)).unwrap_err();
            assert!(err.contains(why), "{q}: {err}");
        }
        let bad_table =
            r#"{"tables":{"t":{"key":"id; drop","operations":{"select":["*"]},"columns":{}}}}"#;
        assert!(
            Definitions::parse(bad_table)
                .unwrap_err()
                .contains("identifier")
        );
        let unknown_field = r#"{"tables":{},"sqls":{}}"#;
        assert!(Definitions::parse(unknown_field).is_err());
    }

    #[test]
    fn placeholders_inside_strings_and_comments_do_not_count() {
        assert_eq!(max_placeholder("SELECT $1, $2::int"), 2);
        assert_eq!(max_placeholder("SELECT '$9', \"$8\" -- $7\n, $1"), 1);
        assert_eq!(max_placeholder("SELECT 1"), 0);
    }
}

#[cfg(test)]
mod example_package {
    use super::Definitions;

    #[test]
    fn finance_db_example_definitions_are_valid() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/app-packages/finance-db/definitions.json"
        );
        Definitions::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    }
}
