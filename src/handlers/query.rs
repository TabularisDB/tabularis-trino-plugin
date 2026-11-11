//! Connection and query execution.
//!
//! `test_connection` submits a real `SELECT 1` request. `ping` currently
//! returns null and falls back to `test_connection` in the host if needed.
//!
//! Paginated `execute_query` calls keep only the requested page. Each page
//! request re-executes the SQL (Tabularis sends `limit`/`page` per page), and
//! collection stops once a row past the page shows that more rows exist. The
//! total row count is then unknown and reported as `null`; it is exact only
//! when Trino finished returning rows within the scanned range.

use serde_json::{json, Value};

use crate::client::{QueryCancellation, RowWindow};
use crate::handlers::connect_client;
use crate::rpc::{error_response, not_implemented, ok_response};

pub fn test_connection(id: Value, request_params: &Value) -> Value {
    let mut client = match connect_client(request_params) {
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

    let mut client = match connect_client(request_params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };
    client.set_cancellation(cancellation);
    let schema = request_params.get("schema").and_then(Value::as_str);
    // Only the requested page is kept; see `Client::run_query_windowed`.
    let window = (limit.is_some() || offset > 0).then_some(RowWindow { offset, limit });
    let result = match client.run_query_windowed(sql, schema, window) {
        Ok(result) => result,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    // An exact total is only known when Trino finished returning rows.
    let total_rows = (!result.stopped_early).then_some(result.rows_seen);
    let pagination = limit.map(|page_size| {
        json!({
            "page": page,
            "page_size": page_size,
            "total_rows": total_rows,
            "has_more": result.stopped_early
                || offset.saturating_add(page_size) < result.rows_seen
        })
    });
    let rows = result.rows;

    ok_response(
        id,
        json!({
            "columns": result.columns,
            "rows": rows,
            "affected_rows": result.update_count.unwrap_or(0),
            "truncated": false,
            "pagination": pagination,
            "query_info": {
                "query_id": result.query_id,
                "state": result.query_state,
                "observed_states": result.observed_states,
                "elapsed_time_ms": result.elapsed_time_ms,
                "fetch_stopped_early": result.stopped_early,
                "trino_stats": result.stats
            }
        }),
    )
}

pub fn explain_query(id: Value, _params: &Value) -> Value {
    // TODO: return the same shape as execute_query but for EXPLAIN.
    not_implemented(id, "explain_query")
}
