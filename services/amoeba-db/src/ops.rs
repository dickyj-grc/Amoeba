//! The operations a caller can perform, shared by the HTTP routes and the MCP
//! tools so both enforce the same rules. Every call is checked against the
//! definitions and the caller's roles, run under the guardrails in
//! [`crate::db`], and written to the audit log.

use crate::db::{Access, DbError};
use crate::defs::{ParamType, QueryMode, TableDef, TableOp, Visibility, allows};
use crate::sql::{self, TableRef};
use crate::{Identity, Loaded, Service};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            axum::Json(json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        use StatusCode as S;
        match e {
            DbError::Unavailable(detail) => {
                tracing::error!("database unavailable: {detail}");
                ApiError::new(
                    S::SERVICE_UNAVAILABLE,
                    "database-unavailable",
                    "database unavailable",
                )
            }
            DbError::Timeout => ApiError::new(
                S::GATEWAY_TIMEOUT,
                "statement-timeout",
                "the statement exceeded its time limit",
            ),
            DbError::ReadOnly => ApiError::new(
                S::FORBIDDEN,
                "read-only",
                "a read definition attempted to write",
            ),
            DbError::PermissionDenied => ApiError::new(
                S::FORBIDDEN,
                "database-permission-denied",
                "the database account may not do this",
            ),
            DbError::InvalidValue(m) => ApiError::new(S::BAD_REQUEST, "invalid-value", m),
            DbError::Conflict(c) => ApiError::new(S::CONFLICT, "conflict", format!("violates {c}")),
            DbError::RowLimit { touched, limit } => ApiError::new(
                S::CONFLICT,
                "row-limit",
                format!("touched {touched} rows, more than the limit of {limit}; rolled back"),
            ),
            DbError::Definition(detail) => {
                tracing::error!("definition does not match the database: {detail}");
                ApiError::new(
                    S::INTERNAL_SERVER_ERROR,
                    "definition-error",
                    "the definition does not match the database",
                )
            }
            DbError::Other(code) => ApiError::new(
                S::INTERNAL_SERVER_ERROR,
                "database-error",
                format!("database error {code}"),
            ),
        }
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad-request", msg)
}

fn forbidden(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, code, msg)
}

fn not_found(code: &'static str, msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, code, msg)
}

/// One request against the definitions.
#[derive(Debug)]
pub enum Call {
    Select {
        table: String,
        filters: Vec<(String, String)>,
        limit: Option<u32>,
        offset: u64,
    },
    Get {
        table: String,
        key: String,
    },
    Insert {
        table: String,
        values: Map<String, Value>,
    },
    Update {
        table: String,
        key: String,
        values: Map<String, Value>,
    },
    Delete {
        table: String,
        key: String,
    },
    Query {
        name: String,
        args: Map<String, Value>,
    },
}

impl Call {
    fn definition(&self) -> String {
        match self {
            Call::Select { table, .. }
            | Call::Get { table, .. }
            | Call::Insert { table, .. }
            | Call::Update { table, .. }
            | Call::Delete { table, .. } => format!("tables/{table}"),
            Call::Query { name, .. } => format!("queries/{name}"),
        }
    }

    fn operation(&self) -> &'static str {
        match self {
            Call::Select { .. } | Call::Get { .. } => "select",
            Call::Insert { .. } => "insert",
            Call::Update { .. } => "update",
            Call::Delete { .. } => "delete",
            Call::Query { .. } => "query",
        }
    }

    /// What the audit record keeps of the input. Values written to a table
    /// are left out (only their column names), since those are often the
    /// sensitive part; filters, keys and query parameters are kept.
    fn audit_params(&self) -> Value {
        match self {
            Call::Select {
                filters,
                limit,
                offset,
                ..
            } => {
                json!({ "filters": filters, "limit": limit, "offset": offset })
            }
            Call::Get { key, .. } | Call::Delete { key, .. } => json!({ "key": key }),
            Call::Insert { values, .. } => json!({ "columns": values.keys().collect::<Vec<_>>() }),
            Call::Update { key, values, .. } => {
                json!({ "key": key, "columns": values.keys().collect::<Vec<_>>() })
            }
            Call::Query { args, .. } => Value::Object(args.clone()),
        }
    }
}

/// Runs `call` for `who` and records it in the audit log.
pub async fn execute(svc: &Service, who: &Identity, call: Call) -> Result<Value, ApiError> {
    let loaded = svc.loaded();
    let result = dispatch(svc, &loaded, who, &call).await;
    let (status, rows) = match &result {
        Ok((_, rows)) => (200, *rows),
        Err(e) => (e.status.as_u16(), 0),
    };
    tracing::info!(
        target: "amoeba_db_audit",
        user = %who.user,
        org = %who.org.as_deref().unwrap_or(""),
        roles = %who.roles.join(","),
        definition = %call.definition(),
        operation = call.operation(),
        params = %call.audit_params(),
        rows,
        status,
        "database call"
    );
    result.map(|(body, _)| body)
}

async fn dispatch(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    call: &Call,
) -> Result<(Value, u64), ApiError> {
    match call {
        Call::Query { name, args } => run_query(svc, loaded, who, name, args).await,
        Call::Select {
            table,
            filters,
            limit,
            offset,
        } => select(svc, loaded, who, table, filters, *limit, *offset).await,
        Call::Get { table, key } => get(svc, loaded, who, table, key).await,
        Call::Insert { table, values } => insert(svc, loaded, who, table, values).await,
        Call::Update { table, key, values } => update(svc, loaded, who, table, key, values).await,
        Call::Delete { table, key } => delete(svc, loaded, who, table, key).await,
    }
}

/// The table's definition, if `who` may perform `op` on it, plus its column
/// types from the database (checked against the definition).
async fn open_table<'a>(
    svc: &Service,
    loaded: &'a Loaded,
    who: &Identity,
    name: &str,
    op: TableOp,
) -> Result<(&'a TableDef, Arc<HashMap<String, String>>), ApiError> {
    let def = loaded
        .defs
        .tables
        .get(name)
        .ok_or_else(|| not_found("unknown-table", format!("no table definition '{name}'")))?;
    let roles = def.operations.get(&op).ok_or_else(|| {
        forbidden(
            "not-opened",
            format!("'{}' is not opened on table '{name}'", op.as_str()),
        )
    })?;
    if !allows(roles, &who.roles) {
        return Err(forbidden(
            "forbidden",
            format!("your roles may not {} table '{name}'", op.as_str()),
        ));
    }
    if def.org_column.is_some() && who.org.is_none() {
        return Err(forbidden(
            "org-required",
            format!("table '{name}' is scoped by organization and the caller has none"),
        ));
    }
    let types = loaded.column_types(svc, name).await?;
    let missing: Vec<&String> = def
        .columns
        .keys()
        .chain([&def.key])
        .chain(def.org_column.iter())
        .filter(|c| !types.contains_key(*c))
        .collect();
    if types.is_empty() || !missing.is_empty() {
        return Err(DbError::Definition(format!(
            "table '{}.{name}' is missing or lacks columns {missing:?}",
            loaded.defs.schema
        ))
        .into());
    }
    Ok((def, types))
}

fn table_ref<'a>(
    loaded: &'a Loaded,
    name: &'a str,
    def: &'a TableDef,
    types: &'a HashMap<String, String>,
) -> TableRef<'a> {
    TableRef {
        schema: &loaded.defs.schema,
        name,
        types,
        org_column: def.org_column.as_deref(),
    }
}

fn org(who: &Identity) -> &str {
    who.org.as_deref().unwrap_or("")
}

/// A JSON value as the text Postgres will cast; `None` is SQL NULL.
fn as_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Values a caller may write: every column must be one they can see in full,
/// and the org column is set by the service, never by the caller.
fn writable(
    def: &TableDef,
    who: &Identity,
    table: &str,
    values: &Map<String, Value>,
) -> Result<Vec<(String, Option<String>)>, ApiError> {
    values
        .iter()
        .map(|(col, v)| {
            let visible = def
                .columns
                .get(col)
                .is_some_and(|p| p.for_roles(&who.roles) == Visibility::Show);
            if !visible || def.org_column.as_deref() == Some(col.as_str()) {
                return Err(forbidden(
                    "column-not-writable",
                    format!("column '{col}' of '{table}' is not writable for your roles"),
                ));
            }
            Ok((col.clone(), as_text(v)))
        })
        .collect()
}

fn parse_rows(json: &str) -> Vec<Value> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Fetches `limit + 1` rows' worth of JSON and reports whether more existed.
fn truncate(mut rows: Vec<Value>, limit: u32) -> (Vec<Value>, bool) {
    let truncated = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    (rows, truncated)
}

async fn select(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    table: &str,
    filters: &[(String, String)],
    limit: Option<u32>,
    offset: u64,
) -> Result<(Value, u64), ApiError> {
    let (def, types) = open_table(svc, loaded, who, table, TableOp::Select).await?;
    let limit = limit.unwrap_or(def.row_limit).min(def.row_limit);
    // Filtering on a column the caller cannot fully see would reveal its
    // values one guess at a time.
    let mut conditions = Vec::new();
    for (col, value) in filters {
        let shown = def
            .columns
            .get(col)
            .is_some_and(|p| p.for_roles(&who.roles) == Visibility::Show);
        if !shown {
            return Err(bad_request(format!(
                "cannot filter on '{col}': not a visible column of '{table}'"
            )));
        }
        conditions.push((col.clone(), Some(value.clone())));
    }
    let projection = sql::projection(&def.columns, &who.roles, "");
    if projection.is_empty() {
        return Err(forbidden(
            "no-visible-columns",
            "your roles see no columns of this table",
        ));
    }
    let tref = table_ref(loaded, table, def, &types);
    let st = sql::select(&tref, &projection, &def.key, &conditions, limit + 1, offset);
    let out = svc
        .db
        .run(
            Access::Read,
            loaded.defs.statement_timeout_ms,
            org(who),
            &st.sql,
            &st.params,
            true,
            None,
        )
        .await?;
    let (rows, truncated) = truncate(parse_rows(out.json.as_deref().unwrap_or("[]")), limit);
    let n = rows.len() as u64;
    Ok((json!({ "rows": rows, "truncated": truncated }), n))
}

async fn get(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    table: &str,
    key: &str,
) -> Result<(Value, u64), ApiError> {
    let (def, types) = open_table(svc, loaded, who, table, TableOp::Select).await?;
    let projection = sql::projection(&def.columns, &who.roles, "");
    if projection.is_empty() {
        return Err(forbidden(
            "no-visible-columns",
            "your roles see no columns of this table",
        ));
    }
    let tref = table_ref(loaded, table, def, &types);
    let st = sql::select(
        &tref,
        &projection,
        &def.key,
        &[(def.key.clone(), Some(key.to_string()))],
        1,
        0,
    );
    let out = svc
        .db
        .run(
            Access::Read,
            loaded.defs.statement_timeout_ms,
            org(who),
            &st.sql,
            &st.params,
            true,
            None,
        )
        .await?;
    let row = parse_rows(out.json.as_deref().unwrap_or("[]"))
        .into_iter()
        .next()
        .ok_or_else(|| not_found("row-not-found", format!("no row '{key}' in '{table}'")))?;
    Ok((json!({ "row": row }), 1))
}

async fn insert(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    table: &str,
    values: &Map<String, Value>,
) -> Result<(Value, u64), ApiError> {
    let (def, types) = open_table(svc, loaded, who, table, TableOp::Insert).await?;
    let values = writable(def, who, table, values)?;
    let projection = sql::projection(&def.columns, &who.roles, "");
    let tref = table_ref(loaded, table, def, &types);
    let st = sql::insert(&tref, &projection, &values);
    let out = svc
        .db
        .run(
            Access::Write,
            loaded.defs.statement_timeout_ms,
            org(who),
            &st.sql,
            &st.params,
            true,
            Some(1),
        )
        .await?;
    let row = parse_rows(out.json.as_deref().unwrap_or("[]"))
        .into_iter()
        .next()
        .unwrap_or(Value::Object(Map::new()));
    Ok((json!({ "row": row }), out.rows))
}

async fn update(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    table: &str,
    key: &str,
    values: &Map<String, Value>,
) -> Result<(Value, u64), ApiError> {
    let (def, types) = open_table(svc, loaded, who, table, TableOp::Update).await?;
    if values.is_empty() {
        return Err(bad_request("an update needs at least one column"));
    }
    let values = writable(def, who, table, values)?;
    let projection = sql::projection(&def.columns, &who.roles, "");
    let tref = table_ref(loaded, table, def, &types);
    let st = sql::update(&tref, &projection, &def.key, key, &values);
    let out = svc
        .db
        .run(
            Access::Write,
            loaded.defs.statement_timeout_ms,
            org(who),
            &st.sql,
            &st.params,
            true,
            Some(def.write_row_limit as u64),
        )
        .await?;
    if out.rows == 0 {
        return Err(not_found(
            "row-not-found",
            format!("no row '{key}' in '{table}'"),
        ));
    }
    let rows = parse_rows(out.json.as_deref().unwrap_or("[]"));
    Ok((json!({ "rows": rows, "rows_affected": out.rows }), out.rows))
}

async fn delete(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    table: &str,
    key: &str,
) -> Result<(Value, u64), ApiError> {
    let (def, types) = open_table(svc, loaded, who, table, TableOp::Delete).await?;
    let tref = table_ref(loaded, table, def, &types);
    let st = sql::delete(&tref, &[], &def.key, key);
    let out = svc
        .db
        .run(
            Access::Write,
            loaded.defs.statement_timeout_ms,
            org(who),
            &st.sql,
            &st.params,
            true,
            Some(def.write_row_limit as u64),
        )
        .await?;
    if out.rows == 0 {
        return Err(not_found(
            "row-not-found",
            format!("no row '{key}' in '{table}'"),
        ));
    }
    Ok((json!({ "rows_affected": out.rows }), out.rows))
}

/// Checks `args` against the declared parameters and returns them in
/// placeholder order.
fn bind_params(
    params: &[crate::defs::ParamDef],
    args: &Map<String, Value>,
) -> Result<Vec<Option<String>>, ApiError> {
    if let Some(extra) = args.keys().find(|k| !params.iter().any(|p| &p.name == *k)) {
        return Err(bad_request(format!("unknown parameter '{extra}'")));
    }
    params
        .iter()
        .map(|p| {
            let value = args.get(&p.name).filter(|v| !v.is_null());
            let Some(value) = value else {
                return if p.required {
                    Err(bad_request(format!("missing parameter '{}'", p.name)))
                } else {
                    Ok(None)
                };
            };
            let ok = match p.kind {
                ParamType::Text => value.is_string(),
                ParamType::Integer => value.is_i64() || value.is_u64(),
                ParamType::Number => value.is_number(),
                ParamType::Boolean => value.is_boolean(),
            };
            if !ok {
                return Err(bad_request(format!(
                    "parameter '{}' must be {:?}",
                    p.name, p.kind
                )));
            }
            Ok(as_text(value))
        })
        .collect()
}

async fn run_query(
    svc: &Service,
    loaded: &Loaded,
    who: &Identity,
    name: &str,
    args: &Map<String, Value>,
) -> Result<(Value, u64), ApiError> {
    let def = loaded
        .defs
        .queries
        .get(name)
        .ok_or_else(|| not_found("unknown-query", format!("no query definition '{name}'")))?;
    if !allows(&def.roles, &who.roles) {
        return Err(forbidden(
            "forbidden",
            format!("your roles may not run '{name}'"),
        ));
    }
    let params = bind_params(&def.params, args)?;
    let approved = def.sql.trim().trim_end_matches(';');
    let timeout = loaded.defs.statement_timeout_ms;
    match def.mode {
        QueryMode::Read => {
            let projection = sql::projection(&def.columns, &who.roles, "q.");
            if projection.is_empty() {
                return Err(forbidden(
                    "no-visible-columns",
                    "your roles see no columns of this query",
                ));
            }
            let text = sql::read_query(approved, &projection, def.row_limit + 1);
            let out = svc
                .db
                .run(Access::Read, timeout, org(who), &text, &params, true, None)
                .await?;
            let (rows, truncated) = truncate(
                parse_rows(out.json.as_deref().unwrap_or("[]")),
                def.row_limit,
            );
            let n = rows.len() as u64;
            Ok((json!({ "rows": rows, "truncated": truncated }), n))
        }
        QueryMode::Write => {
            let out = svc
                .db
                .run(
                    Access::Write,
                    timeout,
                    org(who),
                    approved,
                    &params,
                    false,
                    Some(def.row_limit as u64),
                )
                .await?;
            Ok((json!({ "rows_affected": out.rows }), out.rows))
        }
    }
}
