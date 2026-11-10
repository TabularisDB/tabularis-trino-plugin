pub mod crud;
pub mod ddl;
pub mod metadata;
pub mod query;

use serde_json::Value;

use crate::client::Client;
use crate::error::PluginError;
use crate::models::{inner_params, ConnectionParams};

/// Open a Trino client and attach any host session identifier present on the
/// RPC request. This keeps response-header session properties available to
/// metadata and query calls made in the same host session.
pub fn connect_client(request_params: &Value) -> Result<Client, PluginError> {
    let mut client = Client::connect(ConnectionParams::from_value(inner_params(request_params)))?;
    let session_id = request_params
        .get("session_id")
        .or_else(|| inner_params(request_params).get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty());
    if let Some(session_id) = session_id {
        client.set_session_id(session_id)?;
    }
    Ok(client)
}
