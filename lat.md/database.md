# Database

`amoeba-db` (`services/amoeba-db/`) is an installable service that holds one PostgreSQL connection and serves only approved definitions: tables opened for specific operations, and named queries.

It is its own crate and image (`ghcr.io/dickyj-grc/amoeba-db`), deployed as an app package, so the gateway binary stays small. Callers reach it through the gateway, which sets `x-amoeba-user`, `x-amoeba-org` and `x-amoeba-roles` ([[gateway#Caller identity]]); [[services/amoeba-db/src/lib.rs#Identity]] reads them. A request without roles can use only definitions open to `"*"`, so the service must be reachable only from the gateway.

Every call, over HTTP or MCP, goes through [[services/amoeba-db/src/ops.rs#execute]], which checks the definition and roles, runs it under the guardrails, and writes one audit record.

## Definitions

A JSON document from `DB_DEFINITIONS_FILE` (re-read when it changes) or `DB_DEFINITIONS`. Nothing a caller sends can add to it.

[[services/amoeba-db/src/defs.rs#Definitions]] has `schema`, `statement_timeout_ms`, `tables` and `queries`. Unknown fields are rejected. An invalid file at startup stops the service; an invalid edit later is logged and the previous definitions stay in force.

### Load-time validation

[[services/amoeba-db/src/defs.rs#Definitions#validate]] rejects anything unsafe to embed in SQL or that could never work, so a bad definition fails at load instead of per call.

Table, column, key and schema names must be lowercase identifiers ([[services/amoeba-db/src/defs.rs#is_ident]]). A read query must list its output columns. A write query lists none. The `$n` placeholders in the SQL must match the declared params ([[services/amoeba-db/src/defs.rs#max_placeholder]]). Table names may not start with `query_`, which is reserved for MCP tool names.

## Tables

A table entry opens operations (`select`, `insert`, `update`, `delete`), each to a list of roles, and the service generates the SQL ([[services/amoeba-db/src/sql.rs]]).

Routes: `GET /tables/<t>` (equality filters as query parameters, plus `limit` and `offset`), `GET /tables/<t>/<key>`, `POST /tables/<t>`, `PATCH /tables/<t>/<key>`, `DELETE /tables/<t>/<key>`. Column types come from the catalog the first time a table is used, and the definition's columns are checked against them.

With `org_column` set, reads filter on the caller's org, inserts fill it in, and a caller without an org is refused.

### Writes

A caller may write only columns they can see in full; a column they see masked or not at all is refused. The org column is never theirs to set.

Insert and update return the touched rows through the caller's projection. An update or delete touching more than `write_row_limit` rows (default 1) is rolled back. Values written are left out of the audit record; only their column names are kept.

## Named queries

A query is approved SQL with typed params; the caller sends `POST /queries/<name>` with a JSON object of values.

Values are checked against the declared params (unknown names, missing required values and wrong JSON types are 400) and bound as text, so the SQL casts where it needs another type. A read query is wrapped as `SELECT <projection> FROM (<sql>) q LIMIT n` ([[services/amoeba-db/src/sql.rs#read_query]]), so only declared columns come back, masked per role. A write query returns `rows_affected` and is rolled back above its `row_limit`.

## Column policy

Each column is `show`, `mask` or `hide` per role. Columns not listed are closed, the default is `hide`, and with several roles the most permissive applies ([[services/amoeba-db/src/defs.rs#ColumnPolicy#for_roles]]).

The gateway decides whether a caller may use a table or query at all; this policy decides which columns they get back.

### Masked in the database

Masking is part of the generated SQL ([[services/amoeba-db/src/sql.rs#mask_expr]]): the value is starred except for its last four characters, so the full value never leaves the database. A hidden column is not selected at all.

### Per role on tables

The same table URL returns different columns for different roles, and filters are allowed only on columns the caller sees in full, since filtering on a masked column would reveal it one guess at a time.

## Guardrails

Each call runs in its own transaction set up by [[services/amoeba-db/src/db.rs#Db#run]].

- Reads use the read pool (`DATABASE_URL_READ`, else `DATABASE_URL`) in a `READ ONLY` transaction, so a read definition that writes fails with 403 `read-only`.
- Writes use the write pool (`DATABASE_URL_WRITE`, else `DATABASE_URL`), so writes can use an account with rights only on writable tables.
- `SET LOCAL statement_timeout`; exceeding it is 504.
- `set_config('app.org', <caller org>, true)`, for `org_column` filters and for row-level security policies.
- Row limits: reads fetch one extra row to report `truncated`; writes above their limit roll back with 409 `row-limit`.
- Database errors are mapped to safe messages; driver details are logged, not returned.

### Values are bound

Caller values only ever become `$n` parameters, cast from text to the column's catalog type. Identifiers come only from validated definitions and the catalog, so an injection attempt is just a value that matches nothing.

## MCP tools

`POST /mcp` is a JSON-RPC endpoint ([[services/amoeba-db/src/mcp.rs#handle]]) listing each definition the caller's roles may use as a tool, and running tools through the same checks.

Tools are `query_<name>` and `<table>_list`, `_get`, `_insert`, `_update`, `_delete`. A tool missing from the caller's list is still refused if called directly. Tool errors come back as results with `isError`, so the agent sees the reason.

## Gateway setup

The service is installed as an app package. `examples/app-packages/finance-db` is a working example with demo data in `demo.sql`.

The package declares `DATABASE_URL` as a secret, so only the database service's container receives it, and ships its definitions as the `DB_DEFINITIONS` env default (kept equal to `definitions.json`). Its `operation_rules` name each table and query (see [[gateway#Operation resolution#From an app package]]) and `permissions` grants them, so the gateway refuses a caller before the service is involved.
