//! Schema metadata: catalogs, schemas, tables, columns, and views are
//! discovered through Trino. Unsupported metadata capabilities return empty
//! collections where the host protocol expects a list.
//!
//! Full reference: https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md#5-required-methods

use serde_json::{json, Value};

use crate::client::{Client, QueryResult};
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};

fn first_column_strings(result: QueryResult, description: &str) -> Result<Vec<String>, String> {
    result
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            row.as_array()
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    format!(
                        "Trino returned an invalid {description} row at position {}",
                        index + 1
                    )
                })
        })
        .collect()
}

pub fn get_databases(id: Value, request_params: &Value) -> Value {
    let params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    match client.run_query("SHOW CATALOGS").and_then(|result| {
        first_column_strings(result, "catalog").map_err(crate::error::PluginError::internal)
    }) {
        Ok(catalogs) => ok_response(id, json!(catalogs)),
        Err(error) => error_response(id, error.code, &error.message),
    }
}

pub fn get_schemas(id: Value, request_params: &Value) -> Value {
    let params = ConnectionParams::from_value(inner_params(request_params));
    if params
        .database
        .as_deref()
        .is_none_or(|catalog| catalog.trim().is_empty())
    {
        return error_response(
            id,
            -32602,
            "select a Trino catalog before requesting its schemas",
        );
    }

    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    match client.run_query("SHOW SCHEMAS").and_then(|result| {
        first_column_strings(result, "schema").map_err(crate::error::PluginError::internal)
    }) {
        Ok(schemas) => ok_response(id, json!(schemas)),
        Err(error) => error_response(id, error.code, &error.message),
    }
}

fn selected_schema(request_params: &Value) -> Result<&str, &'static str> {
    request_params
        .get("schema")
        .and_then(Value::as_str)
        .filter(|schema| !schema.trim().is_empty())
        .ok_or("select a Trino schema before requesting tables or columns")
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn result_cell<'a>(result: &'a QueryResult, row: &'a Value, name: &str) -> Option<&'a Value> {
    let index = result
        .columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case(name))?;
    row.as_array()?.get(index)
}

pub fn get_tables(id: Value, request_params: &Value) -> Value {
    let schema = match selected_schema(request_params) {
        Ok(schema) => schema,
        Err(message) => return error_response(id, -32602, message),
    };
    let params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    match client.run_query_with_schema("SHOW TABLES", Some(schema)) {
        Ok(result) => match first_column_strings(result, "table") {
            Ok(tables) => ok_response(
                id,
                json!(tables
                    .into_iter()
                    .map(|name| json!({"name": name, "schema": schema, "comment": null}))
                    .collect::<Vec<_>>()),
            ),
            Err(message) => error_response(id, -32603, &message),
        },
        Err(error) => error_response(id, error.code, &error.message),
    }
}

pub fn get_columns(id: Value, request_params: &Value) -> Value {
    let schema = match selected_schema(request_params) {
        Ok(schema) => schema,
        Err(message) => return error_response(id, -32602, message),
    };
    let Some(table) = request_params
        .get("table")
        .and_then(Value::as_str)
        .filter(|table| !table.trim().is_empty())
    else {
        return error_response(id, -32602, "table name is required to request columns");
    };

    let sql = format!(
        "SELECT column_name, data_type, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_schema = {} AND table_name = {} \
         ORDER BY ordinal_position",
        sql_string_literal(schema),
        sql_string_literal(table)
    );
    let params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };

    match client.run_query_with_schema(&sql, Some(schema)) {
        Ok(result) => {
            let columns = result
                .rows
                .iter()
                .map(|row| {
                    let Some(name) =
                        result_cell(&result, row, "column_name").and_then(Value::as_str)
                    else {
                        return Err("Trino returned a column without its name".to_string());
                    };
                    let Some(data_type) =
                        result_cell(&result, row, "data_type").and_then(Value::as_str)
                    else {
                        return Err(format!("Trino returned no type for column '{name}'"));
                    };
                    let nullable = result_cell(&result, row, "is_nullable");
                    let is_nullable = nullable.is_some_and(|value| {
                        value.as_bool().unwrap_or_else(|| {
                            value
                                .as_str()
                                .is_some_and(|value| value.eq_ignore_ascii_case("YES"))
                        })
                    });
                    let default_value = result_cell(&result, row, "column_default")
                        .cloned()
                        .unwrap_or(Value::Null);

                    Ok(json!({
                        "name": name,
                        "data_type": data_type,
                        "is_pk": false,
                        "is_nullable": is_nullable,
                        "is_auto_increment": false,
                        "is_generated": false,
                        "default_value": default_value,
                        "character_maximum_length": null,
                        "comment": null
                    }))
                })
                .collect::<Result<Vec<_>, String>>();

            match columns {
                Ok(columns) => ok_response(id, json!(columns)),
                Err(message) => error_response(id, -32603, &message),
            }
        }
        Err(error) => error_response(id, error.code, &error.message),
    }
}

pub fn get_foreign_keys(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_indexes(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_views(id: Value, request_params: &Value) -> Value {
    let schema = match selected_schema(request_params) {
        Ok(schema) => schema,
        Err(message) => return error_response(id, -32602, message),
    };
    let params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };
    let sql = format!(
        "SELECT table_name FROM information_schema.views \
         WHERE table_schema = {} ORDER BY table_name",
        sql_string_literal(schema)
    );

    match client.run_query_with_schema(&sql, Some(schema)) {
        Ok(result) => match first_column_strings(result, "view") {
            Ok(views) => ok_response(
                id,
                json!(views
                    .into_iter()
                    .map(|name| json!({"name": name, "schema": schema}))
                    .collect::<Vec<_>>()),
            ),
            Err(message) => error_response(id, -32603, &message),
        },
        Err(error) => error_response(id, error.code, &error.message),
    }
}

pub fn get_view_definition(id: Value, request_params: &Value) -> Value {
    let schema = match selected_schema(request_params) {
        Ok(schema) => schema,
        Err(message) => return error_response(id, -32602, message),
    };
    let Some(view) = request_params
        .get("view")
        .and_then(Value::as_str)
        .filter(|view| !view.trim().is_empty())
    else {
        return error_response(
            id,
            -32602,
            "view name is required to request its definition",
        );
    };
    let params = ConnectionParams::from_value(inner_params(request_params));
    let mut client = match Client::connect(params) {
        Ok(client) => client,
        Err(error) => return error_response(id, error.code, &error.message),
    };
    let sql = format!(
        "SELECT view_definition FROM information_schema.views \
         WHERE table_schema = {} AND table_name = {}",
        sql_string_literal(schema),
        sql_string_literal(view)
    );

    match client.run_query_with_schema(&sql, Some(schema)) {
        Ok(result) => {
            let definition = result
                .rows
                .first()
                .and_then(|row| row.as_array())
                .and_then(|row| row.first())
                .and_then(Value::as_str);
            match definition {
                Some(definition) => ok_response(id, Value::String(definition.to_string())),
                None => error_response(
                    id,
                    -32603,
                    "Trino returned no definition for this view; it may not exist or may not be visible",
                ),
            }
        }
        Err(error) => error_response(id, error.code, &error.message),
    }
}

pub fn get_view_columns(id: Value, request_params: &Value) -> Value {
    let Some(view) = request_params.get("view").and_then(Value::as_str) else {
        return error_response(id, -32602, "view name is required to request its columns");
    };
    let mut params = request_params.clone();
    if let Some(object) = params.as_object_mut() {
        object.insert("table".to_string(), Value::String(view.to_string()));
    }
    get_columns(id, &params)
}

pub fn get_routines(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_parameters(id: Value, _params: &Value) -> Value {
    ok_response(id, json!([]))
}

pub fn get_routine_definition(id: Value, _params: &Value) -> Value {
    ok_response(id, Value::String(String::new()))
}

pub fn get_schema_snapshot(id: Value, _params: &Value) -> Value {
    // Used for the ER diagram. Return
    // [{ name, columns: [...], foreign_keys: [...] }].
    ok_response(id, json!([]))
}

pub fn get_all_columns_batch(id: Value, _params: &Value) -> Value {
    ok_response(id, json!({}))
}

pub fn get_all_foreign_keys_batch(id: Value, _params: &Value) -> Value {
    ok_response(id, json!({}))
}
