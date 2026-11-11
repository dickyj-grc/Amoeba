//! Connection pools and the transaction every call runs in: read-only for
//! reads, a statement timeout, the caller's org in `app.org`, and a rollback
//! when a write touches more rows than its definition allows.

use deadpool_postgres::{Config, Pool, Runtime};
use std::collections::HashMap;
use tokio_postgres::NoTls;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{ToSql, Type};

pub struct Db {
    read: Pool,
    write: Pool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read pool, `BEGIN READ ONLY`.
    Read,
    /// Write pool, read-write transaction.
    Write,
}

/// Why a call failed at the database. Messages are safe to return to the
/// caller; driver details are logged, not returned.
#[derive(Debug)]
pub enum DbError {
    Unavailable(String),
    Timeout,
    ReadOnly,
    PermissionDenied,
    InvalidValue(String),
    Conflict(String),
    RowLimit { touched: u64, limit: u64 },
    Definition(String),
    Other(String),
}

impl From<tokio_postgres::Error> for DbError {
    fn from(e: tokio_postgres::Error) -> Self {
        let Some(db) = e.as_db_error() else {
            return DbError::Unavailable(e.to_string());
        };
        tracing::warn!(code = %db.code().code(), "database error: {}", db.message());
        let code = db.code();
        if *code == SqlState::QUERY_CANCELED {
            DbError::Timeout
        } else if *code == SqlState::READ_ONLY_SQL_TRANSACTION {
            DbError::ReadOnly
        } else if *code == SqlState::INSUFFICIENT_PRIVILEGE {
            DbError::PermissionDenied
        } else if code.code().starts_with("22") {
            DbError::InvalidValue(db.message().to_string())
        } else if code.code().starts_with("23") {
            DbError::Conflict(db.constraint().unwrap_or("constraint").to_string())
        } else if code.code().starts_with("42") {
            DbError::Definition(db.message().to_string())
        } else {
            DbError::Other(db.code().code().to_string())
        }
    }
}

impl From<deadpool_postgres::PoolError> for DbError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        DbError::Unavailable(e.to_string())
    }
}

fn pool(url: &str) -> Result<Pool, String> {
    let cfg = Config {
        url: Some(url.to_string()),
        ..Default::default()
    };
    cfg.create_pool(Some(Runtime::Tokio1), NoTls)
        .map_err(|e| format!("invalid database URL: {e}"))
}

/// What one statement produced.
pub struct Outcome {
    /// JSON array text, for statements that return rows.
    pub json: Option<String>,
    pub rows: u64,
}

impl Db {
    /// Pools connect lazily, so the service starts even while the database
    /// is down; calls then fail with `Unavailable`.
    pub fn new(read_url: &str, write_url: &str) -> Result<Self, String> {
        Ok(Self {
            read: pool(read_url)?,
            write: pool(write_url)?,
        })
    }

    /// Runs one statement in its own transaction with the guardrails set.
    ///
    /// `returns_json`: the statement yields one row whose first column is
    /// JSON text and whose optional second column is a row count. Otherwise
    /// it is executed and the affected-row count is used. When `max_rows` is
    /// set and the count exceeds it, the transaction is rolled back.
    #[allow(clippy::too_many_arguments)] // each is one guardrail input
    pub async fn run(
        &self,
        access: Access,
        timeout_ms: u64,
        org: &str,
        sql: &str,
        params: &[Option<String>],
        returns_json: bool,
        max_rows: Option<u64>,
    ) -> Result<Outcome, DbError> {
        let pool = match access {
            Access::Read => &self.read,
            Access::Write => &self.write,
        };
        let mut client = pool.get().await?;
        let tx = client
            .build_transaction()
            .read_only(access == Access::Read)
            .start()
            .await?;
        tx.batch_execute(&format!("SET LOCAL statement_timeout = {timeout_ms}"))
            .await?;
        tx.execute("SELECT set_config('app.org', $1, true)", &[&org])
            .await?;

        let types = vec![Type::TEXT; params.len()];
        let stmt = tx.prepare_typed(sql, &types).await?;
        let args: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();

        let outcome = if returns_json {
            let row = tx.query_one(&stmt, &args).await?;
            let json: String = row.get(0);
            let rows = if row.len() > 1 {
                row.get::<_, i64>(1) as u64
            } else {
                0
            };
            Outcome {
                json: Some(json),
                rows,
            }
        } else {
            Outcome {
                json: None,
                rows: tx.execute(&stmt, &args).await?,
            }
        };

        if let Some(limit) = max_rows
            && outcome.rows > limit
        {
            tx.rollback().await?;
            return Err(DbError::RowLimit {
                touched: outcome.rows,
                limit,
            });
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Column name to type (as `format_type` spells it) for one table.
    /// Empty when the table does not exist.
    pub async fn column_types(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<HashMap<String, String>, DbError> {
        let client = self.read.get().await?;
        let rows = client
            .query(
                "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped",
                &[&schema, &table],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
            .collect())
    }
}
