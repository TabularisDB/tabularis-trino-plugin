//! Connection and query execution.
//!
//! `test_connection` submits a real `SELECT 1` request. `ping` currently
//! returns null and falls back to `test_connection` in the host if needed.

use serde_json::{json, Value};

use crate::client::{Client, QueryCancellation};
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, not_implemented, ok_response};

pub fn test_connection(id: Value, request_params: &Value) -> Value {
    let connection_params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(connection_params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    match client.run_query("SELECT 1") {
        Ok(_) => ok_response(id, json!({ "success": true })),
        Err(error) => error_response(id, error.code, &error.message),
    }
}

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a non-negative integer")),
    }
}

pub fn execute_query(id: Value, request_params: &Value) -> Value {
    execute_query_with_cancellation(id, request_params, QueryCancellation::default())
}

pub fn execute_query_with_cancellation(
    id: Value,
    request_params: &Value,
    cancellation: QueryCancellation,
) -> Value {
    let Some(sql) = request_params
        .get("query")
        .and_then(Value::as_str)
        .filter(|query| !query.trim().is_empty())
    else {
        return error_response(id, -32602, "a non-empty SQL query is required");
    };
    let limit = match optional_u64(request_params, "limit") {
        Ok(limit) if limit != Some(0) => limit,
        Ok(_) => return error_response(id, -32602, "'limit' must be greater than zero"),
        Err(message) => return error_response(id, -32602, &message),
    };
    let page = match optional_u64(request_params, "page") {
        Ok(Some(0)) => {
            return error_response(id, -32602, "'page' is one-based and must be at least 1")
        }
        Ok(Some(page)) => page,
        Ok(None) => 1,
        Err(message) => return error_response(id, -32602, &message),
    };
    let explicit_offset = match optional_u64(request_params, "offset") {
        Ok(offset) => offset,
        Err(message) => return error_response(id, -32602, &message),
    };
    let offset = explicit_offset.unwrap_or_else(|| {
        limit
            .map(|page_size| page.saturating_sub(1).saturating_mul(page_size))
            .unwrap_or(0)
    });
    let page = match (explicit_offset, limit) {
        (Some(offset), Some(page_size)) => offset / page_size + 1,
        _ => page,
    };

    let connection_params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(connection_params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };
    client.set_cancellation(cancellation);
    let schema = request_params.get("schema").and_then(Value::as_str);
    let result = match client.run_query_with_schema(sql, schema) {
        Ok(result) => result,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    let total_rows = result.rows.len() as u64;
    let rows = result
        .rows
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(
            limit
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(usize::MAX),
        )
        .collect::<Vec<_>>();
    let pagination = limit.map(|page_size| {
        json!({
            "page": page,
            "page_size": page_size,
            "total_rows": total_rows,
            "has_more": offset.saturating_add(page_size) < total_rows
        })
    });

    ok_response(
        id,
        json!({
            "columns": result.columns,
            "rows": rows,
            "affected_rows": result.update_count.unwrap_or(0),
            "truncated": false,
            "pagination": pagination
        }),
    )
}

pub fn explain_query(id: Value, _params: &Value) -> Value {
    // TODO: return the same shape as execute_query but for EXPLAIN.
    not_implemented(id, "explain_query")
}
