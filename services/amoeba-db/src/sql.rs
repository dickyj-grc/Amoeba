//! SQL generation. Identifiers come only from validated definitions and the
//! database catalog; caller values only ever become bound parameters (`$n`,
//! always sent as text and cast to the column's type).

use crate::defs::{ColumnPolicy, Visibility};
use std::collections::{BTreeMap, HashMap};

/// A statement plus its text parameters (`None` binds SQL NULL).
#[derive(Debug, Default)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Option<String>>,
}

impl Statement {
    /// Adds a parameter and returns its placeholder cast to `pg_type`.
    fn bind(&mut self, value: Option<String>, pg_type: &str) -> String {
        self.params.push(value);
        format!("${}::text::{pg_type}", self.params.len())
    }
}

pub fn ident(name: &str) -> String {
    format!("\"{name}\"")
}

/// Shows the last four characters and stars the rest, computed in the
/// database so the full value never leaves it.
pub fn mask_expr(col: &str) -> String {
    let t = format!("{col}::text");
    format!(
        "CASE WHEN {col} IS NULL THEN NULL \
         WHEN length({t}) <= 4 THEN repeat('*', length({t})) \
         ELSE repeat('*', length({t}) - 4) || right({t}, 4) END"
    )
}

/// The select list for a caller: shown columns as-is, masked ones through
/// [`mask_expr`], hidden ones left out. `qualifier` prefixes column refs.
pub fn projection(
    columns: &BTreeMap<String, ColumnPolicy>,
    roles: &[String],
    qualifier: &str,
) -> Vec<String> {
    columns
        .iter()
        .filter_map(|(name, policy)| {
            let col = format!("{qualifier}{}", ident(name));
            match policy.for_roles(roles) {
                Visibility::Show => Some(format!("{col} AS {}", ident(name))),
                Visibility::Mask => Some(format!("{} AS {}", mask_expr(&col), ident(name))),
                Visibility::Hide => None,
            }
        })
        .collect()
}

/// Aggregates `inner` into one JSON array as text.
fn json_rows(inner: &str) -> String {
    format!("SELECT coalesce(json_agg(t), '[]'::json)::text FROM ({inner}) t")
}

pub struct TableRef<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    /// Column name to Postgres type, from the catalog.
    pub types: &'a HashMap<String, String>,
    pub org_column: Option<&'a str>,
}

impl TableRef<'_> {
    fn qualified(&self) -> String {
        format!("{}.{}", ident(self.schema), ident(self.name))
    }

    fn pg_type(&self, col: &str) -> &str {
        self.types.get(col).map(String::as_str).unwrap_or("text")
    }

    /// `"org" = current_setting('app.org')`, compared as text.
    fn org_condition(&self) -> Option<String> {
        self.org_column
            .map(|c| format!("{}::text = current_setting('app.org')", ident(c)))
    }
}

/// `SELECT` returning one JSON-array text column. `filters` are equality
/// conditions; `limit` rows starting at `offset`, ordered by `key`.
pub fn select(
    table: &TableRef,
    projection: &[String],
    key: &str,
    filters: &[(String, Option<String>)],
    limit: u32,
    offset: u64,
) -> Statement {
    let mut st = Statement::default();
    let mut conditions = Vec::new();
    for (col, value) in filters {
        let placeholder = st.bind(value.clone(), table.pg_type(col));
        conditions.push(format!("{} = {placeholder}", ident(col)));
    }
    conditions.extend(table.org_condition());
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    let inner = format!(
        "SELECT {} FROM {}{where_clause} ORDER BY {} LIMIT {limit} OFFSET {offset}",
        projection.join(", "),
        table.qualified(),
        ident(key),
    );
    st.sql = json_rows(&inner);
    st
}

/// Wraps a data-modifying CTE `r` so the statement returns the caller's
/// view of the touched rows as JSON text and the number of rows touched.
fn returning(cte: &str, projection: &[String]) -> String {
    let rows = if projection.is_empty() {
        "'[]'".to_string()
    } else {
        format!(
            "({})",
            json_rows(&format!("SELECT {} FROM r", projection.join(", ")))
        )
    };
    format!("WITH r AS ({cte} RETURNING *) SELECT {rows}, (SELECT count(*) FROM r)")
}

pub fn insert(
    table: &TableRef,
    projection: &[String],
    values: &[(String, Option<String>)],
) -> Statement {
    let mut st = Statement::default();
    let mut cols = Vec::new();
    let mut exprs = Vec::new();
    for (col, value) in values {
        cols.push(ident(col));
        exprs.push(st.bind(value.clone(), table.pg_type(col)));
    }
    if let Some(org) = table.org_column {
        cols.push(ident(org));
        exprs.push(format!(
            "current_setting('app.org')::text::{}",
            table.pg_type(org)
        ));
    }
    let cte = if cols.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES", table.qualified())
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table.qualified(),
            cols.join(", "),
            exprs.join(", ")
        )
    };
    st.sql = returning(&cte, projection);
    st
}

fn key_condition(st: &mut Statement, table: &TableRef, key: &str, value: &str) -> String {
    let placeholder = st.bind(Some(value.to_string()), table.pg_type(key));
    let mut cond = format!("{} = {placeholder}", ident(key));
    if let Some(org) = table.org_condition() {
        cond = format!("{cond} AND {org}");
    }
    cond
}

pub fn update(
    table: &TableRef,
    projection: &[String],
    key: &str,
    key_value: &str,
    values: &[(String, Option<String>)],
) -> Statement {
    let mut st = Statement::default();
    let sets: Vec<String> = values
        .iter()
        .map(|(col, value)| {
            let placeholder = st.bind(value.clone(), table.pg_type(col));
            format!("{} = {placeholder}", ident(col))
        })
        .collect();
    let cond = key_condition(&mut st, table, key, key_value);
    let cte = format!(
        "UPDATE {} SET {} WHERE {cond}",
        table.qualified(),
        sets.join(", ")
    );
    st.sql = returning(&cte, projection);
    st
}

pub fn delete(table: &TableRef, projection: &[String], key: &str, key_value: &str) -> Statement {
    let mut st = Statement::default();
    let cond = key_condition(&mut st, table, key, key_value);
    let cte = format!("DELETE FROM {} WHERE {cond}", table.qualified());
    st.sql = returning(&cte, projection);
    st
}

/// A read query's approved SQL wrapped so only the caller's projection of its
/// declared columns comes back, capped at `limit` rows.
pub fn read_query(sql: &str, projection: &[String], limit: u32) -> String {
    json_rows(&format!(
        "SELECT {} FROM ({sql}) q LIMIT {limit}",
        projection.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn customers() -> BTreeMap<String, ColumnPolicy> {
        serde_json::from_str(
            r#"{"id":{"default":"show"},"nik":{"mask":["cs_agent"],"show":["compliance"]},"balance":{"show":["finance"]}}"#,
        )
        .unwrap()
    }

    // @lat: [[database#Column policy#Masked in the database]]
    #[test]
    fn projection_masks_in_sql_and_leaves_hidden_columns_out() {
        let roles = vec!["cs_agent".to_string()];
        let proj = projection(&customers(), &roles, "");
        assert_eq!(proj.len(), 2, "balance is hidden: {proj:?}");
        assert_eq!(proj[0], r#""id" AS "id""#);
        assert!(proj[1].starts_with("CASE WHEN \"nik\" IS NULL"));
        assert!(proj[1].ends_with(r#"AS "nik""#));
    }

    // @lat: [[database#Guardrails#Values are bound]]
    #[test]
    fn caller_values_become_parameters_not_sql() {
        let types = HashMap::from([("status".to_string(), "text".to_string())]);
        let table = TableRef {
            schema: "public",
            name: "loans",
            types: &types,
            org_column: Some("org_id"),
        };
        let evil = "x'; DROP TABLE loans; --".to_string();
        let st = select(
            &table,
            &[r#""id" AS "id""#.to_string()],
            "id",
            &[("status".into(), Some(evil.clone()))],
            10,
            0,
        );
        assert!(!st.sql.contains("DROP"), "{}", st.sql);
        assert!(st.sql.contains(r#""status" = $1::text::text"#));
        assert!(
            st.sql
                .contains(r#""org_id"::text = current_setting('app.org')"#)
        );
        assert_eq!(st.params, vec![Some(evil)]);
    }
}
