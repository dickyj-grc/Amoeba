//! End-to-end tests against a real PostgreSQL. Set `AMOEBA_DB_TEST_URL`
//! (CI does); without it these tests print a note and pass. Each test gets
//! its own schema, so they run in parallel.

use amoeba_db::db::Db;
use amoeba_db::defs::Definitions;
use amoeba_db::{Service, router};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    schema: String,
    admin: tokio_postgres::Client,
}

fn test_url() -> Option<String> {
    std::env::var("AMOEBA_DB_TEST_URL").ok()
}

async fn setup() -> Option<Fixture> {
    let Some(url) = test_url() else {
        eprintln!("AMOEBA_DB_TEST_URL not set; skipping database test");
        return None;
    };
    let (admin, conn) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(conn);
    // Unique per test and per run: tests in one process share the clock.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let schema = format!(
        "t_{}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    admin
        .batch_execute(&format!(
            "CREATE SCHEMA {s};
             CREATE TABLE {s}.customers (
               id int PRIMARY KEY, name text, nik text, balance numeric, org_id text NOT NULL);
             INSERT INTO {s}.customers VALUES
               (1, 'Ani', '3174012345678901', 1500.50, 'org_a'),
               (2, 'Budi', '3174019876543210', 20.00, 'org_a'),
               (3, 'Citra', '3275011111222233', 99.00, 'org_b');
             CREATE TABLE {s}.loans (
               id serial PRIMARY KEY, branch text, status text, amount numeric);
             INSERT INTO {s}.loans (branch, status, amount) VALUES
               ('JKT-01', 'overdue', 1250000), ('JKT-01', 'overdue', 400000),
               ('BDG-02', 'overdue', 990000), ('BDG-02', 'current', 10);
             CREATE SEQUENCE {s}.seq;",
            s = schema
        ))
        .await
        .unwrap();

    let defs = json!({
        "schema": schema,
        "statement_timeout_ms": 1500,
        "tables": {
            "customers": {
                "key": "id",
                "org_column": "org_id",
                "operations": {
                    "select": ["cs_agent", "compliance"],
                    "insert": ["compliance"],
                    "update": ["compliance", "cs_agent"],
                    "delete": ["compliance"]
                },
                "columns": {
                    "id": { "default": "show" },
                    "name": { "default": "show" },
                    "nik": { "mask": ["cs_agent"], "show": ["compliance"] },
                    "balance": { "show": ["compliance"] }
                }
            },
            "loans": {
                "key": "branch",
                "write_row_limit": 1,
                "operations": { "select": ["*"], "update": ["officer"] },
                "columns": {
                    "id": { "default": "show" }, "branch": { "default": "show" },
                    "status": { "default": "show" }, "amount": { "default": "show" }
                }
            }
        },
        "queries": {
            "overdue-loans": {
                "mode": "read",
                "sql": format!("SELECT id, branch, amount FROM {schema}.loans WHERE status = 'overdue' AND branch = $1 ORDER BY id"),
                "params": [{ "name": "branch", "type": "text" }],
                "roles": ["analyst", "finance"],
                "columns": {
                    "id": { "default": "show" },
                    "branch": { "default": "show" },
                    "amount": { "mask": ["analyst"], "show": ["finance"] }
                }
            },
            "sneaky-write": {
                "mode": "read",
                "sql": format!("SELECT nextval('{schema}.seq') AS v"),
                "roles": ["*"],
                "columns": { "v": { "default": "show" } }
            },
            "slow": {
                "mode": "read",
                "sql": "SELECT 1 AS v FROM pg_sleep(5)",
                "roles": ["*"],
                "columns": { "v": { "default": "show" } }
            },
            "close-branch": {
                "mode": "write",
                "sql": format!("UPDATE {schema}.loans SET status = 'closed' WHERE branch = $1"),
                "params": [{ "name": "branch" }],
                "row_limit": 1,
                "roles": ["officer"]
            }
        }
    });
    let defs = Definitions::parse(&defs.to_string()).unwrap();
    let svc = Service::new(Db::new(&url, &url).unwrap(), defs);
    Some(Fixture {
        app: router(svc),
        schema,
        admin,
    })
}

impl Fixture {
    async fn call(
        &self,
        method: &str,
        uri: &str,
        roles: &str,
        org: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-amoeba-user", "tester")
            .header("x-amoeba-roles", roles)
            .header("content-type", "application/json");
        if let Some(org) = org {
            req = req.header("x-amoeba-org", org);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn count(&self, sql: &str) -> i64 {
        let sql = sql.replace("{s}", &self.schema);
        self.admin.query_one(&sql, &[]).await.unwrap().get(0)
    }
}

// @lat: [[database#Named queries]]
#[tokio::test]
async fn analyst_runs_an_approved_query_and_gets_a_masked_column() {
    let Some(f) = setup().await else { return };

    let (status, body) = f
        .call(
            "POST",
            "/queries/overdue-loans",
            "analyst",
            None,
            Some(json!({"branch": "JKT-01"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["branch"], "JKT-01");
    assert_eq!(rows[0]["amount"], "***0000", "amount is masked for analyst");
    assert_eq!(body["truncated"], false);

    let (_, body) = f
        .call(
            "POST",
            "/queries/overdue-loans",
            "finance",
            None,
            Some(json!({"branch": "JKT-01"})),
        )
        .await;
    assert_eq!(body["rows"][0]["amount"], 1250000, "finance sees the value");

    let (status, _) = f
        .call(
            "POST",
            "/queries/overdue-loans",
            "viewer",
            None,
            Some(json!({"branch": "JKT-01"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = f
        .call(
            "POST",
            "/queries/overdue-loans",
            "analyst",
            None,
            Some(json!({"branch": "JKT-01", "x": 1})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = f
        .call(
            "POST",
            "/queries/overdue-loans",
            "analyst",
            None,
            Some(json!({"branch": 7})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "wrong parameter type");
}

// @lat: [[database#Column policy#Per role on tables]]
#[tokio::test]
async fn table_reads_apply_column_policy_and_org_scope() {
    let Some(f) = setup().await else { return };

    let (status, body) = f
        .call(
            "GET",
            "/tables/customers/1",
            "cs_agent",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = &body["row"];
    assert_eq!(row["name"], "Ani");
    assert_eq!(row["nik"], "************8901");
    assert!(row.get("balance").is_none(), "balance is hidden: {row}");
    assert!(row.get("org_id").is_none(), "unlisted columns are closed");

    let (_, body) = f
        .call(
            "GET",
            "/tables/customers/1",
            "compliance",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(body["row"]["nik"], "3174012345678901");
    assert_eq!(body["row"]["balance"], 1500.50);

    // Another org's row does not exist for this caller.
    let (status, _) = f
        .call(
            "GET",
            "/tables/customers/3",
            "compliance",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = f
        .call(
            "GET",
            "/tables/customers",
            "compliance",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);

    let (status, _) = f
        .call("GET", "/tables/customers/1", "compliance", None, None)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "org-scoped table needs an org"
    );

    // Filtering on a column the caller can't fully see would leak it.
    let (status, _) = f
        .call(
            "GET",
            "/tables/customers?nik=3174012345678901",
            "cs_agent",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, body) = f
        .call(
            "GET",
            "/tables/customers?name=Budi",
            "cs_agent",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(body["rows"][0]["id"], 2);

    let (_, body) = f
        .call(
            "GET",
            "/tables/customers?limit=1",
            "compliance",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["truncated"], true);
}

// @lat: [[database#Tables#Writes]]
#[tokio::test]
async fn writes_respect_roles_columns_and_org() {
    let Some(f) = setup().await else { return };

    let new = json!({"id": 10, "name": "Dewi", "nik": "1111222233334444", "balance": 5});
    let (status, _) = f
        .call(
            "POST",
            "/tables/customers",
            "cs_agent",
            Some("org_a"),
            Some(new.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "cs_agent may not insert");

    let (status, body) = f
        .call(
            "POST",
            "/tables/customers",
            "compliance",
            Some("org_a"),
            Some(new),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["row"]["name"], "Dewi");
    assert_eq!(
        f.count("SELECT count(*) FROM {s}.customers WHERE id = 10 AND org_id = 'org_a'")
            .await,
        1
    );

    // The org column is the service's to set.
    let (status, _) = f
        .call(
            "POST",
            "/tables/customers",
            "compliance",
            Some("org_a"),
            Some(json!({"id": 11, "org_id": "org_b"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // cs_agent may update, but not a column it only sees masked.
    let (status, body) = f
        .call(
            "PATCH",
            "/tables/customers/1",
            "cs_agent",
            Some("org_a"),
            Some(json!({"nik": "0"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = f
        .call(
            "PATCH",
            "/tables/customers/1",
            "cs_agent",
            Some("org_a"),
            Some(json!({"name": "Ani S."})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rows_affected"], 1);
    assert_eq!(
        body["rows"][0]["nik"], "************8901",
        "returned row is masked too"
    );

    // Updating another org's row finds nothing.
    let (status, _) = f
        .call(
            "PATCH",
            "/tables/customers/3",
            "compliance",
            Some("org_a"),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = f
        .call(
            "DELETE",
            "/tables/customers/2",
            "compliance",
            Some("org_a"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        f.count("SELECT count(*) FROM {s}.customers WHERE id = 2")
            .await,
        0
    );
}

// @lat: [[database#Guardrails]]
#[tokio::test]
async fn guardrails_hold_against_a_real_database() {
    let Some(f) = setup().await else { return };

    // A read definition runs in a read-only transaction.
    let (status, body) = f
        .call("POST", "/queries/sneaky-write", "x", None, None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "read-only");

    let (status, body) = f.call("POST", "/queries/slow", "x", None, None).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");

    // A write that touches more rows than allowed is rolled back.
    let (status, body) = f
        .call(
            "PATCH",
            "/tables/loans/JKT-01",
            "officer",
            None,
            Some(json!({"status": "closed"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "row-limit");
    let (status, _) = f
        .call(
            "POST",
            "/queries/close-branch",
            "officer",
            None,
            Some(json!({"branch": "BDG-02"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        f.count("SELECT count(*) FROM {s}.loans WHERE status = 'closed'")
            .await,
        0
    );

    // Values are bound, so an injection attempt is just a value that matches nothing.
    let (status, body) = f
        .call(
            "GET",
            "/tables/loans?branch=x'%20OR%20'1'%3D'1",
            "anyone",
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["rows"].as_array().unwrap().len(), 0);
    assert_eq!(f.count("SELECT count(*) FROM {s}.loans").await, 4);

    let (status, _) = f.call("GET", "/tables/nope", "anyone", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// @lat: [[database#MCP tools]]
#[tokio::test]
async fn mcp_lists_only_permitted_tools_and_calls_them() {
    let Some(f) = setup().await else { return };

    let rpc = |method: &str, params: Value| json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let (_, body) = f
        .call(
            "POST",
            "/mcp",
            "analyst",
            None,
            Some(rpc("tools/list", json!({}))),
        )
        .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"query_overdue-loans"), "{names:?}");
    assert!(
        names.contains(&"loans_list"),
        "loans select is open to everyone"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("customers_")),
        "{names:?}"
    );
    assert!(!names.contains(&"query_close-branch"));

    let (_, body) = f
        .call(
            "POST",
            "/mcp",
            "analyst",
            None,
            Some(rpc(
                "tools/call",
                json!({"name": "query_overdue-loans", "arguments": {"branch": "BDG-02"}}),
            )),
        )
        .await;
    let result = &body["result"];
    assert_eq!(result["isError"], false, "{body}");
    let rows: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(rows["rows"][0]["amount"], "**0000");

    // Calling a tool that isn't listed still goes through the same checks.
    let (_, body) = f
        .call(
            "POST",
            "/mcp",
            "analyst",
            None,
            Some(rpc(
                "tools/call",
                json!({"name": "query_close-branch", "arguments": {"branch": "JKT-01"}}),
            )),
        )
        .await;
    assert_eq!(body["result"]["isError"], true);
}
