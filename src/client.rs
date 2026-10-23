//! Trino's HTTP client protocol.
//!
//! A statement is submitted to `/v1/statement`; result pages are then fetched
//! from the `nextUri` returned by Trino. The client keeps the last `nextUri`
//! so callers can cancel an in-flight query with `cancel_query`.

use std::fs;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client as HttpClient, RequestBuilder, Response};
use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use serde_json::Value;

use crate::error::PluginError;
use crate::models::ConnectionParams;

const DEFAULT_PORT: u16 = 8080;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TRANSIENT_RETRIES: usize = 5;

pub struct Client {
    http: HttpClient,
    endpoint: reqwest::Url,
    params: ConnectionParams,
    username: String,
    cancellation: QueryCancellation,
}

/// Cancellation state shared between the stdio dispatcher and one executing
/// query. Tabularis can request cancellation while the query thread is blocked
/// waiting for the next Trino page.
#[derive(Clone, Default)]
pub struct QueryCancellation {
    cancelled: Arc<AtomicBool>,
    current_query: Arc<Mutex<Option<reqwest::Url>>>,
    cancel_http: Arc<Mutex<Option<CancellationHttpContext>>>,
}

#[derive(Clone)]
struct CancellationHttpContext {
    http: HttpClient,
    username: String,
    password: Option<String>,
}

impl QueryCancellation {
    pub fn cancel(&self) -> Result<(), PluginError> {
        self.cancelled.store(true, Ordering::SeqCst);
        // Keep this lock through DELETE so the query worker cannot race us
        // into sending a second cancellation request for the same nextUri.
        let mut current_query = self
            .current_query
            .lock()
            .map_err(|_| PluginError::internal("query cancellation state is unavailable"))?;
        let Some(next_uri) = current_query.as_ref().cloned() else {
            // The initial POST may still be in flight. The query thread checks
            // this flag as soon as Trino returns a nextUri.
            return Ok(());
        };
        let context = self
            .cancel_http
            .lock()
            .map_err(|_| PluginError::internal("query cancellation state is unavailable"))?
            .clone()
            .ok_or_else(|| PluginError::internal("query cancellation client is unavailable"))?;

        let mut request = context.http.delete(next_uri);
        if let Some(password) = context.password.filter(|value| !value.is_empty()) {
            request = request.basic_auth(context.username, Some(password));
        }
        let response = request.send().map_err(|error| {
            PluginError::internal(format!("could not cancel Trino query: {error}"))
        })?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            *current_query = None;
            Ok(())
        } else {
            Err(PluginError::internal(format!(
                "Trino rejected query cancellation with HTTP {}",
                response.status()
            )))
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn attach_client(&self, http: HttpClient, username: String, password: Option<String>) {
        if let Ok(mut context) = self.cancel_http.lock() {
            *context = Some(CancellationHttpContext {
                http,
                username,
                password,
            });
        }
    }

    fn set_next_uri(&self, next_uri: Option<reqwest::Url>) {
        if let Ok(mut current_query) = self.current_query.lock() {
            *current_query = next_uri;
        }
    }
}

#[derive(Debug, Default)]
pub struct QueryResult {
    /// Column names in the order reported by Trino.
    pub columns: Vec<String>,
    /// Trino type names aligned with `columns` for lossless value handling.
    pub column_types: Vec<Option<String>>,
    /// Rows from every result page, in Trino's response order.
    pub rows: Vec<Value>,
    /// Trino's update count, when the statement reports one.
    pub update_count: Option<u64>,
    /// Trino's query identifier and most recently observed execution state.
    pub query_id: Option<String>,
    pub query_state: Option<String>,
    /// Distinct states observed while following Trino result pages.
    pub observed_states: Vec<String>,
    /// Small, UI-safe subset of Trino's query statistics.
    pub stats: Value,
    /// Wall-clock time spent submitting the query and collecting its pages.
    pub elapsed_time_ms: u64,
}

impl Client {
    /// Validate connection settings and prepare an HTTP client.
    ///
    /// This does not contact the coordinator; call `run_query` (for example
    /// with `SELECT 1`) to check that the endpoint and credentials work.
    pub fn connect(params: ConnectionParams) -> Result<Self, PluginError> {
        let host = params
            .host
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| PluginError::invalid_params("Trino coordinator host is required"))?;

        let ssl_mode = params
            .ssl_mode
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let secure = match ssl_mode.as_str() {
            "" | "disable" | "disabled" => false,
            // Tabularis serializes the SSL-tab choices with underscores, while
            // connection strings and other clients commonly use hyphens.
            "prefer" | "preferred" | "require" | "required" | "verify-ca"
            | "verify_ca" | "verify-full" | "verify_full" | "verify-identity"
            | "verify_identity" => true,
            other => {
                return Err(PluginError::invalid_params(format!(
                "unsupported ssl_mode '{other}'; use disable, prefer, require, verify_ca, or verify_identity"
            )))
            }
        };

        let endpoint = if host.contains("://") {
            reqwest::Url::parse(host).map_err(|err| {
                PluginError::invalid_params(format!("invalid Trino coordinator URL: {err}"))
            })?
        } else {
            let scheme = if secure { "https" } else { "http" };
            let port = params.port.unwrap_or(DEFAULT_PORT);
            reqwest::Url::parse(&format!("{scheme}://{host}:{port}/"))
                .map_err(|err| PluginError::invalid_params(format!("invalid Trino host: {err}")))?
        };

        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(PluginError::invalid_params(
                "Trino coordinator URL must use http or https",
            ));
        }
        if endpoint.host_str().is_none() {
            return Err(PluginError::invalid_params(
                "Trino coordinator URL must include a host",
            ));
        }

        let endpoint = endpoint
            .join("v1/statement")
            .map_err(|err| PluginError::invalid_params(format!("invalid Trino endpoint: {err}")))?;
        let username = params
            .username
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("tabularis")
            .to_string();

        if params
            .password
            .as_ref()
            .is_some_and(|password| !password.is_empty())
        {
            if endpoint.scheme() != "https" {
                return Err(PluginError::invalid_params(
                    "Trino password authentication requires HTTPS",
                ));
            }
            if params
                .username
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            {
                return Err(PluginError::invalid_params(
                    "a Trino username is required when a password is provided",
                ));
            }
        }

        let mut http_builder = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .cookie_store(true);
        if let Some(ca) = params
            .ssl_ca
            .as_deref()
            .map(str::trim)
            .filter(|ca| !ca.is_empty())
        {
            let ca_pem = load_pem(ca, "Trino CA certificate")?;
            if !ca_pem
                .windows(b"-----BEGIN CERTIFICATE-----".len())
                .any(|window| window == b"-----BEGIN CERTIFICATE-----")
            {
                return Err(PluginError::invalid_params(
                    "invalid Trino CA certificate PEM: no CERTIFICATE block was found",
                ));
            }
            let certificate = reqwest::Certificate::from_pem(&ca_pem).map_err(|error| {
                PluginError::invalid_params(format!("invalid Trino CA certificate PEM: {error}"))
            })?;
            http_builder = http_builder.add_root_certificate(certificate);
        }

        let client_certificate = params
            .ssl_cert
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let client_key = params
            .ssl_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (client_certificate, client_key) {
            (Some(certificate), Some(key)) => {
                let mut identity_pem = load_pem(certificate, "Trino client certificate")?;
                identity_pem.push(b'\n');
                identity_pem.extend(load_pem(key, "Trino client private key")?);
                let identity = reqwest::Identity::from_pem(&identity_pem).map_err(|error| {
                    PluginError::invalid_params(format!(
                        "invalid Trino client certificate or key PEM: {error}"
                    ))
                })?;
                http_builder = http_builder.identity(identity);
            }
            (Some(_), None) => {
                return Err(PluginError::invalid_params(
                    "a Trino client certificate requires its private key",
                ));
            }
            (None, Some(_)) => {
                return Err(PluginError::invalid_params(
                    "a Trino client private key requires its certificate",
                ));
            }
            (None, None) => {}
        }

        let http = http_builder
            .build()
            .map_err(|err| PluginError::internal(format!("could not create HTTP client: {err}")))?;

        let cancellation = QueryCancellation::default();
        cancellation.attach_client(http.clone(), username.clone(), params.password.clone());

        Ok(Self {
            http,
            endpoint,
            params,
            username,
            cancellation,
        })
    }

    pub fn set_cancellation(&mut self, cancellation: QueryCancellation) {
        cancellation.attach_client(
            self.http.clone(),
            self.username.clone(),
            self.params.password.clone(),
        );
        self.cancellation = cancellation;
    }

    /// Submit SQL and collect all result pages until Trino removes `nextUri`.
    pub fn run_query(&mut self, sql: &str) -> Result<QueryResult, PluginError> {
        self.run_query_with_schema(sql, None)
    }

    /// Submit SQL with an optional schema context overriding the connection's
    /// default schema. Catalog context comes from `ConnectionParams::database`.
    pub fn run_query_with_schema(
        &mut self,
        sql: &str,
        schema: Option<&str>,
    ) -> Result<QueryResult, PluginError> {
        if sql.trim().is_empty() {
            return Err(PluginError::invalid_params("SQL query must not be empty"));
        }
        if self.cancellation.is_cancelled() {
            return Err(Self::cancelled_error());
        }
        let started_at = Instant::now();

        let response = self
            .send_with_retries(|| {
                let mut request = self
                    .with_auth(self.http.post(self.endpoint.clone()))
                    .header("X-Trino-User", &self.username)
                    .header("X-Trino-Source", "Tabularis")
                    .header("X-Trino-Client-Info", "Tabularis Trino plugin")
                    .header("Content-Type", "text/plain; charset=utf-8")
                    .body(sql.to_string());
                if let Some(catalog) = self
                    .params
                    .database
                    .as_deref()
                    .filter(|value| !value.is_empty())
                {
                    request = request.header("X-Trino-Catalog", catalog);
                }
                if let Some(schema) = schema.filter(|value| !value.is_empty()) {
                    request = request.header("X-Trino-Schema", schema);
                }
                request.send()
            })
            .map_err(|err| {
                PluginError::internal(format!("could not submit query to Trino: {err:?}"))
            })?;

        let mut page = self.read_response(response)?;
        let mut result = QueryResult::default();
        loop {
            let next_url = page
                .get("nextUri")
                .and_then(Value::as_str)
                .map(|next_uri| self.validate_next_uri(next_uri))
                .transpose()?;
            self.cancellation.set_next_uri(next_url.clone());
            if self.cancellation.is_cancelled() {
                self.cancel_query()?;
                return Err(Self::cancelled_error());
            }
            self.collect_page(&page, &mut result)?;
            let Some(next_url) = next_url else {
                preserve_exact_values(&mut result);
                result.elapsed_time_ms =
                    started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
                return Ok(result);
            };

            let response = match self
                .send_with_retries(|| self.with_auth(self.http.get(next_url.clone())).send())
            {
                Ok(response) => response,
                Err(err) => {
                    let _ = self.cancel_query();
                    return Err(PluginError::internal(format!(
                        "failed while fetching Trino query results: {err}"
                    )));
                }
            };
            page = match self.read_response(response) {
                Ok(page) => page,
                Err(err) => {
                    let _ = self.cancel_query();
                    return Err(err);
                }
            };
        }
    }

    /// Cancel the currently tracked Trino query, if it has a `nextUri`.
    pub fn cancel_query(&mut self) -> Result<(), PluginError> {
        self.cancellation.cancel()
    }

    fn cancelled_error() -> PluginError {
        PluginError {
            code: -32800,
            message: "query cancelled".to_string(),
        }
    }

    fn with_auth(&self, request: RequestBuilder) -> RequestBuilder {
        match self
            .params
            .password
            .as_deref()
            .filter(|password| !password.is_empty())
        {
            Some(password) => request.basic_auth(&self.username, Some(password)),
            None => request,
        }
    }

    fn send_with_retries<F>(&self, mut send: F) -> Result<Response, reqwest::Error>
    where
        F: FnMut() -> Result<Response, reqwest::Error>,
    {
        for attempt in 0..=MAX_TRANSIENT_RETRIES {
            let response = send()?;
            let status = response.status();
            let transient = matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            );
            if !transient || attempt == MAX_TRANSIENT_RETRIES {
                return Ok(response);
            }

            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_millis(100));
            drop(response);
            thread::sleep(retry_after.min(Duration::from_secs(2)));
        }

        unreachable!("retry loop returns on its final attempt")
    }

    fn read_response(&self, response: Response) -> Result<Value, PluginError> {
        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .unwrap_or_else(|_| "response body unavailable".to_string());
            return Err(PluginError::internal(format!(
                "Trino returned HTTP {status}: {}",
                message.trim()
            )));
        }

        let page: Value = response
            .json()
            .map_err(|err| PluginError::internal(format!("Trino returned invalid JSON: {err}")))?;
        if let Some(error) = page.get("error") {
            let name = error
                .get("errorName")
                .and_then(Value::as_str)
                .unwrap_or("query failed");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Trino returned an unspecified query error");
            if name == "USER_CANCELED" {
                return Err(Self::cancelled_error());
            }
            return Err(PluginError::internal(format_trino_error(
                &page, error, name, message,
            )));
        }
        Ok(page)
    }

    fn collect_page(&self, page: &Value, result: &mut QueryResult) -> Result<(), PluginError> {
        if result.query_id.is_none() {
            result.query_id = page.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if let Some(stats) = page.get("stats").and_then(Value::as_object) {
            if let Some(state) = stats.get("state").and_then(Value::as_str) {
                result.query_state = Some(state.to_string());
                if result.observed_states.last().map(String::as_str) != Some(state) {
                    result.observed_states.push(state.to_string());
                }
            }
            let mut summary = serde_json::Map::new();
            for key in [
                "queued",
                "scheduled",
                "nodes",
                "totalSplits",
                "completedSplits",
                "runningSplits",
                "queuedSplits",
                "cpuTime",
                "wallTime",
                "queuedTime",
                "elapsedTime",
                "cpuTimeMillis",
                "wallTimeMillis",
                "queuedTimeMillis",
                "elapsedTimeMillis",
                "processedRows",
                "processedBytes",
            ] {
                if let Some(value) = stats.get(key) {
                    summary.insert(key.to_string(), value.clone());
                }
            }
            result.stats = Value::Object(summary);
        }
        if result.columns.is_empty() {
            if let Some(columns) = page.get("columns").and_then(Value::as_array) {
                for column in columns {
                    if let Some(name) = column.get("name").and_then(Value::as_str) {
                        result.columns.push(name.to_string());
                        result.column_types.push(
                            column
                                .get("type")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        );
                    }
                }
            }
        }

        if let Some(rows) = page.get("data").and_then(Value::as_array) {
            result.rows.extend(rows.iter().cloned());
        }

        if result.update_count.is_none() {
            result.update_count = page
                .get("updateCount")
                .and_then(|count| count.as_u64().or_else(|| count.as_str()?.parse().ok()));
        }
        Ok(())
    }

    fn validate_next_uri(&self, next_uri: &str) -> Result<reqwest::Url, PluginError> {
        let url = reqwest::Url::parse(next_uri)
            .or_else(|_| self.endpoint.join(next_uri))
            .map_err(|err| {
                PluginError::internal(format!("Trino returned an invalid nextUri: {err}"))
            })?;

        if url.origin() != self.endpoint.origin() {
            return Err(PluginError::internal(
                "Trino returned a nextUri on a different origin; refusing to forward connection credentials",
            ));
        }
        Ok(url)
    }
}

fn format_trino_error(page: &Value, error: &Value, name: &str, message: &str) -> String {
    let query_id = page
        .get("id")
        .and_then(Value::as_str)
        .map(|id| format!(" query {id}"))
        .unwrap_or_default();
    let error_code = error.get("errorCode");
    let code = error_code
        .and_then(|value| value.get("code"))
        .map(Value::to_string);
    let error_type = error_code
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let location = error
        .get("errorLocation")
        .and_then(Value::as_object)
        .map(|location| {
            let line = location.get("lineNumber").and_then(Value::as_u64);
            let column = location.get("columnNumber").and_then(Value::as_u64);
            match (line, column) {
                (Some(line), Some(column)) => format!(" at line {line}, column {column}"),
                (Some(line), None) => format!(" at line {line}"),
                _ => String::new(),
            }
        })
        .unwrap_or_default();
    let failure_message = error
        .get("failureInfo")
        .and_then(|failure| failure.get("message"))
        .and_then(Value::as_str)
        .filter(|failure| !failure.is_empty() && *failure != message);

    let mut details = vec![name.to_string()];
    if let Some(code) = code {
        details.push(format!("code {code}"));
    }
    if let Some(error_type) = error_type {
        details.push(error_type.to_string());
    }
    let cause = failure_message
        .map(|failure| format!("; cause: {failure}"))
        .unwrap_or_default();
    format!(
        "Trino{query_id} failed ({}){location}: {message}{cause}",
        details.join(", ")
    )
}

fn load_pem(value: &str, description: &str) -> Result<Vec<u8>, PluginError> {
    if value.contains("-----BEGIN ") {
        return Ok(value.as_bytes().to_vec());
    }
    fs::read(value).map_err(|error| {
        PluginError::invalid_params(format!(
            "could not read {description} file '{value}': {error}"
        ))
    })
}

fn preserve_exact_values(result: &mut QueryResult) {
    for row in &mut result.rows {
        let Some(values) = row.as_array_mut() else {
            continue;
        };
        for (index, value) in values.iter_mut().enumerate() {
            if let Some(Some(column_type)) = result.column_types.get(index) {
                preserve_value(value, column_type);
            } else {
                preserve_unsafe_integers(value);
            }
        }
    }
}

fn preserve_value(value: &mut Value, type_name: &str) {
    let normalized = type_name.trim().to_ascii_lowercase();
    if normalized.starts_with("decimal(") {
        if let Value::Number(number) = value {
            *value = Value::String(number.to_string());
        }
        return;
    }

    if let Some(arguments) = type_arguments(&normalized, "array") {
        if let (Some(element_type), Some(values)) = (arguments.first(), value.as_array_mut()) {
            for element in values {
                preserve_value(element, element_type);
            }
            return;
        }
    }

    if let Some(arguments) = type_arguments(&normalized, "map") {
        if arguments.len() == 2 {
            match value {
                Value::Object(entries) => {
                    for item in entries.values_mut() {
                        preserve_value(item, arguments[1]);
                    }
                    return;
                }
                Value::Array(entries) => {
                    for entry in entries {
                        if let Some(pair) = entry.as_array_mut() {
                            if pair.len() == 2 {
                                preserve_value(&mut pair[0], arguments[0]);
                                preserve_value(&mut pair[1], arguments[1]);
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    if let Some(fields) = type_arguments(&normalized, "row") {
        if let Some(values) = value.as_array_mut() {
            for (item, field) in values.iter_mut().zip(fields) {
                preserve_value(item, row_field_type(field));
            }
            return;
        }
        if let Value::Object(values) = value {
            for (name, field_type) in fields
                .iter()
                .map(|field| (row_field_name(field), row_field_type(field)))
            {
                if let Some(item) = name.and_then(|name| values.get_mut(name)) {
                    preserve_value(item, field_type);
                }
            }
            return;
        }
    }

    if normalized == "bigint" || normalized == "ubigint" {
        preserve_unsafe_integer(value);
    } else {
        preserve_unsafe_integers(value);
    }
}

fn type_arguments<'a>(type_name: &'a str, base: &str) -> Option<Vec<&'a str>> {
    let rest = type_name.strip_prefix(base)?.trim_start();
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut in_quotes = false;
    let mut start = 0usize;
    for (position, character) in inner.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => depth += 1,
            ')' if !in_quotes => depth = depth.saturating_sub(1),
            ',' if !in_quotes && depth == 0 => {
                arguments.push(inner[start..position].trim());
                start = position + 1;
            }
            _ => {}
        }
    }
    arguments.push(inner[start..].trim());
    Some(arguments)
}

fn row_field_name(field: &str) -> Option<&str> {
    let field = field.trim_start();
    if let Some(quoted) = field.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(&quoted[..end]);
    }
    let end = field.find(char::is_whitespace)?;
    Some(&field[..end])
}

fn row_field_type(field: &str) -> &str {
    let field = field.trim_start();
    if let Some(quoted) = field.strip_prefix('"') {
        if let Some(end) = quoted.find('"') {
            return quoted[end + 1..].trim_start();
        }
    }
    if let Some(end) = field.find(char::is_whitespace) {
        return field[end..].trim_start();
    }
    field
}

fn preserve_unsafe_integers(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for item in values {
                preserve_unsafe_integers(item);
            }
        }
        Value::Object(values) => {
            for item in values.values_mut() {
                preserve_unsafe_integers(item);
            }
        }
        _ => preserve_unsafe_integer(value),
    }
}

fn preserve_unsafe_integer(value: &mut Value) {
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    let Value::Number(number) = value else {
        return;
    };
    let unsafe_integer = number
        .as_i64()
        .is_some_and(|integer| !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&integer))
        || number
            .as_u64()
            .is_some_and(|integer| integer > MAX_SAFE_INTEGER as u64);
    if unsafe_integer {
        *value = Value::String(number.to_string());
    }
}
