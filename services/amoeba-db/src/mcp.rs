//! MCP endpoint (`POST /mcp`, JSON responses). Each approved definition is a
//! tool, listed only to callers whose roles may use it; calls go through the
//! same [`crate::ops`] checks as the HTTP routes.

use crate::defs::{ParamType, TableOp, Visibility, allows};
use crate::ops::{self, Call};
use crate::{Identity, Service};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};
use std::sync::Arc;

const PROTOCOL_VERSION: &str = "2025-03-26";

pub async fn handle(
    State(svc): State<Arc<Service>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let who = Identity::from_headers(&headers);
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // Notifications (no id) get no response body.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "amoeba-db", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": list_tools(&svc, &who) })),
        "tools/call" => Ok(call_tool(&svc, &who, &params).await),
        _ => Err((-32601, format!("method '{method}' not found"))),
    };

    let body = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Json(body).into_response()
}

fn schema(properties: Map<String, Value>, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

fn visible_columns(
    columns: &std::collections::BTreeMap<String, crate::defs::ColumnPolicy>,
    roles: &[String],
    at_least: Visibility,
) -> Vec<String> {
    columns
        .iter()
        .filter(|(_, p)| p.for_roles(roles) >= at_least)
        .map(|(c, _)| c.clone())
        .collect()
}

/// The tools `who` may call.
pub fn list_tools(svc: &Service, who: &Identity) -> Vec<Value> {
    let loaded = svc.loaded();
    let mut tools = Vec::new();

    for (name, q) in &loaded.defs.queries {
        if !allows(&q.roles, &who.roles) {
            continue;
        }
        let mut props = Map::new();
        let mut required = Vec::new();
        for p in &q.params {
            let kind = match p.kind {
                ParamType::Text => "string",
                ParamType::Integer => "integer",
                ParamType::Number => "number",
                ParamType::Boolean => "boolean",
            };
            props.insert(
                p.name.clone(),
                json!({ "type": kind, "description": p.description }),
            );
            if p.required {
                required.push(p.name.as_str());
            }
        }
        tools.push(json!({
            "name": format!("query_{name}"),
            "description": if q.description.is_empty() { format!("Run the approved query '{name}'.") } else { q.description.clone() },
            "inputSchema": schema(props, &required),
        }));
    }

    for (table, t) in &loaded.defs.tables {
        let can = |op: TableOp| t.operations.get(&op).is_some_and(|r| allows(r, &who.roles));
        let shown = visible_columns(&t.columns, &who.roles, Visibility::Show);
        let readable = visible_columns(&t.columns, &who.roles, Visibility::Mask);
        let about = if t.description.is_empty() {
            String::new()
        } else {
            format!("{} ", t.description)
        };
        let key = json!({ "type": "string", "description": format!("Value of '{}'", t.key) });
        let values = json!({
            "type": "object",
            "description": format!("Column values; writable columns: {}", shown.join(", ")),
        });

        if can(TableOp::Select) && !readable.is_empty() {
            let mut props = Map::new();
            for c in &shown {
                props.insert(
                    c.clone(),
                    json!({ "type": "string", "description": "Equals filter" }),
                );
            }
            props.insert("limit".into(), json!({ "type": "integer" }));
            props.insert("offset".into(), json!({ "type": "integer" }));
            tools.push(json!({
                "name": format!("{table}_list"),
                "description": format!("{about}List rows of '{table}'. Columns: {}.", readable.join(", ")),
                "inputSchema": schema(props, &[]),
            }));
            tools.push(json!({
                "name": format!("{table}_get"),
                "description": format!("{about}Get one row of '{table}' by '{}'.", t.key),
                "inputSchema": schema(Map::from_iter([("key".to_string(), key.clone())]), &["key"]),
            }));
        }
        if can(TableOp::Insert) {
            tools.push(json!({
                "name": format!("{table}_insert"),
                "description": format!("{about}Insert a row into '{table}'."),
                "inputSchema": schema(Map::from_iter([("values".to_string(), values.clone())]), &["values"]),
            }));
        }
        if can(TableOp::Update) {
            tools.push(json!({
                "name": format!("{table}_update"),
                "description": format!("{about}Update the row of '{table}' with this '{}'.", t.key),
                "inputSchema": schema(
                    Map::from_iter([("key".to_string(), key.clone()), ("values".to_string(), values.clone())]),
                    &["key", "values"],
                ),
            }));
        }
        if can(TableOp::Delete) {
            tools.push(json!({
                "name": format!("{table}_delete"),
                "description": format!("{about}Delete the row of '{table}' with this '{}'.", t.key),
                "inputSchema": schema(Map::from_iter([("key".to_string(), key)]), &["key"]),
            }));
        }
    }
    tools
}

fn arg_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Maps a tool name and its arguments to a [`Call`].
fn to_call(name: &str, args: &Map<String, Value>) -> Result<Call, String> {
    if let Some(query) = name.strip_prefix("query_") {
        return Ok(Call::Query {
            name: query.to_string(),
            args: args.clone(),
        });
    }
    let (table, op) = name
        .rsplit_once('_')
        .ok_or_else(|| format!("unknown tool '{name}'"))?;
    let table = table.to_string();
    let key = || {
        args.get("key")
            .map(arg_text)
            .ok_or_else(|| "missing 'key'".to_string())
    };
    let values = || match args.get("values") {
        Some(Value::Object(m)) => Ok(m.clone()),
        _ => Err("'values' must be an object".to_string()),
    };
    Ok(match op {
        "list" => {
            let mut filters = Vec::new();
            let mut limit = None;
            let mut offset = 0;
            for (k, v) in args {
                match k.as_str() {
                    "limit" => limit = v.as_u64().map(|n| n.min(u32::MAX as u64) as u32),
                    "offset" => offset = v.as_u64().unwrap_or(0),
                    _ => filters.push((k.clone(), arg_text(v))),
                }
            }
            Call::Select {
                table,
                filters,
                limit,
                offset,
            }
        }
        "get" => Call::Get { table, key: key()? },
        "insert" => Call::Insert {
            table,
            values: values()?,
        },
        "update" => Call::Update {
            table,
            key: key()?,
            values: values()?,
        },
        "delete" => Call::Delete { table, key: key()? },
        _ => return Err(format!("unknown tool '{name}'")),
    })
}

/// Tool errors are results with `isError`, so the agent sees the reason.
async fn call_tool(svc: &Service, who: &Identity, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let text_result = |text: String, is_error: bool| json!({ "content": [{ "type": "text", "text": text }], "isError": is_error });
    let call = match to_call(name, &args) {
        Ok(call) => call,
        Err(e) => return text_result(e, true),
    };
    match ops::execute(svc, who, call).await {
        Ok(body) => text_result(body.to_string(), false),
        Err(e) => text_result(format!("{}: {}", e.code, e.message), true),
    }
}
