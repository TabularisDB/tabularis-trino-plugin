#![allow(dead_code)]

#[path = "../src/client.rs"]
mod client;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/handlers/metadata.rs"]
mod metadata_handler;
#[path = "../src/models.rs"]
mod models;
#[path = "../src/handlers/query.rs"]
mod query_handler;

mod rpc {
    use serde_json::{json, Value};

    pub fn ok_response(id: Value, result: Value) -> Value {
        json!({"jsonrpc": "2.0", "result": result, "id": id})
    }

    pub fn error_response(id: Value, code: i64, message: &str) -> Value {
        json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": id})
    }

    #[allow(dead_code)]
    pub fn not_implemented(id: Value, method: &str) -> Value {
        error_response(id, -32601, &format!("{method} is not implemented"))
    }
}

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use client::{Client, QueryCancellation};
use models::ConnectionParams;
use serde_json::{json, Value};

struct MockResponse {
    method: &'static str,
    path: &'static str,
    status: u16,
    reason: &'static str,
    body: String,
}

impl MockResponse {
    fn json(method: &'static str, path: &'static str, value: serde_json::Value) -> Self {
        Self {
            method,
            path,
            status: 200,
            reason: "OK",
            body: value.to_string(),
        }
    }

    fn raw(
        method: &'static str,
        path: &'static str,
        status: u16,
        reason: &'static str,
        body: &str,
    ) -> Self {
        Self {
            method,
            path,
            status,
            reason,
            body: body.to_string(),
        }
    }
}

struct MockServer {
    address: String,
    task: JoinHandle<Vec<String>>,
}

impl MockServer {
    fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener
            .local_addr()
            .expect("mock server address")
            .to_string();
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            for expected in responses {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_request(&mut stream);
                let (method, path) = request
                    .lines()
                    .next()
                    .and_then(|line| {
                        let mut parts = line.split_whitespace();
                        Some((parts.next()?, parts.next()?))
                    })
                    .expect("request line");
                assert_eq!(method, expected.method);
                assert_eq!(path, expected.path);
                requests.push(request);
                write_response(&mut stream, &expected);
            }
            requests
        });
        Self { address, task }
    }

    fn client(&self) -> Client {
        Client::connect(ConnectionParams {
            driver: Some("trino".into()),
            host: Some(format!("http://{}", self.address)),
            port: None,
            database: Some("memory".into()),
            username: Some("tester".into()),
            password: None,
            ssl_mode: None,
        })
        .expect("construct Trino client")
    }

    fn finish(self) -> Vec<String> {
        self.task.join().expect("mock server thread")
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    let mut content_length = 0usize;

    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read request headers");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().expect("valid content length");
            }
        }
        request.push_str(&line);
    }

    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read request body");
    request.push_str(&String::from_utf8_lossy(&body));

    request
}

fn write_response(stream: &mut TcpStream, response: &MockResponse) {
    let body = response.body.as_bytes();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        body.len()
    )
    .expect("write response headers");
    stream.write_all(body).expect("write response body");
}

#[test]
fn collects_ordered_pages_even_when_the_first_page_is_empty() {
    let server = MockServer::start(vec![
        MockResponse::json(
            "POST",
            "/v1/statement",
            json!({
                "columns": [{"name": "n", "type": "integer"}],
                "nextUri": "/v1/statement/q/1"
            }),
        ),
        MockResponse::json(
            "GET",
            "/v1/statement/q/1",
            json!({"data": [[1], [2]], "nextUri": "/v1/statement/q/2"}),
        ),
        MockResponse::json("GET", "/v1/statement/q/2", json!({"data": [[3]]})),
    ]);
    let mut client = server.client();
    let result = client.run_query("SELECT n").expect("query succeeds");

    assert_eq!(result.columns, ["n"]);
    assert_eq!(result.rows, [json!([1]), json!([2]), json!([3])]);
    server.finish();
}

#[test]
fn query_values_preserve_decimal_and_large_integer_precision_in_nested_types() {
    let body = r#"{
        "columns": [
            {"name":"dec_col","type":"decimal(38,9)"},
            {"name":"big_col","type":"bigint"},
            {"name":"arr_col","type":"array(decimal(20,9))"},
            {"name":"map_col","type":"map(varchar,decimal(20,9))"},
            {"name":"row_col","type":"row(id bigint, name varchar)"},
            {"name":"null_col","type":"varchar"}
        ],
        "data": [[
            12345678901234567890.123456789,
            9223372036854775807,
            [1.123456789, 2.987654321],
            {"a":1.123456789,"b":2.987654321},
            [9223372036854775807,"x"],
            null
        ]]
    }"#;
    let server = MockServer::start(vec![MockResponse::raw(
        "POST",
        "/v1/statement",
        200,
        "OK",
        body,
    )]);
    let mut client = server.client();
    let result = client
        .run_query("SELECT typed_values")
        .expect("query succeeds");

    assert_eq!(
        result.rows,
        [json!([
            "12345678901234567890.123456789",
            "9223372036854775807",
            ["1.123456789", "2.987654321"],
            {"a": "1.123456789", "b": "2.987654321"},
            ["9223372036854775807", "x"],
            null
        ])]
    );
    server.finish();
}

#[test]
fn retries_transient_http_statuses() {
    let server = MockServer::start(vec![
        MockResponse::raw("POST", "/v1/statement", 503, "Service Unavailable", ""),
        MockResponse::json(
            "POST",
            "/v1/statement",
            json!({"columns": [{"name": "ok"}], "data": [[true]]}),
        ),
    ]);
    let mut client = server.client();
    let result = client.run_query("SELECT true").expect("retry succeeds");

    assert_eq!(result.rows, [json!([true])]);
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn reports_trino_query_errors() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({"error": {"errorName": "USER_ERROR", "message": "Access denied"}}),
    )]);
    let mut client = server.client();
    let error = client.run_query("SELECT secret").expect_err("query fails");

    assert!(error.to_string().contains("USER_ERROR"));
    assert!(error.to_string().contains("Access denied"));
    server.finish();
}

#[test]
fn refuses_cross_origin_next_uri() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({"nextUri": "http://example.invalid/v1/statement/q/1"}),
    )]);
    let mut client = server.client();
    let error = client
        .run_query("SELECT 1")
        .expect_err("origin is rejected");

    assert!(error.to_string().contains("different origin"));
    server.finish();
}

#[test]
fn cancels_query_if_fetching_a_result_page_fails() {
    let server = MockServer::start(vec![
        MockResponse::json(
            "POST",
            "/v1/statement",
            json!({"nextUri": "/v1/statement/q/1"}),
        ),
        MockResponse::raw("GET", "/v1/statement/q/1", 500, "Server Error", "failed"),
        MockResponse::raw("DELETE", "/v1/statement/q/1", 204, "No Content", ""),
    ]);
    let mut client = server.client();
    assert!(client.run_query("SELECT 1").is_err());

    let requests = server.finish();
    assert!(requests[2].starts_with("DELETE /v1/statement/q/1 "));
}

#[test]
fn rejects_password_authentication_over_plain_http() {
    let result = Client::connect(ConnectionParams {
        driver: Some("trino".into()),
        host: Some("http://127.0.0.1:8080".into()),
        port: None,
        database: None,
        username: Some("tester".into()),
        password: Some("secret".into()),
        ssl_mode: None,
    });
    let error = match result {
        Ok(_) => panic!("password authentication over HTTP should be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("requires HTTPS"));
}

#[test]
fn test_connection_runs_a_query_and_returns_success() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({"columns": [{"name": "_col0"}], "data": [[1]]}),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        }
    });

    let response = query_handler::test_connection(json!(9), &params);

    assert_eq!(response["result"]["success"], true);
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn test_connection_returns_an_actionable_coordinator_error() {
    let server = MockServer::start(vec![MockResponse::raw(
        "POST",
        "/v1/statement",
        401,
        "Unauthorized",
        "invalid credentials",
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "username": "tester"
        }
    });

    let response = query_handler::test_connection(json!(10), &params);

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("HTTP 401"));
    assert!(response["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("invalid credentials"));
    server.finish();
}

#[test]
fn get_databases_returns_visible_trino_catalogs() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "Catalog", "type": "varchar"}],
            "data": [["memory"], ["tpch"]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "username": "tester"
        }
    });

    let response = metadata_handler::get_databases(json!(11), &params);

    assert_eq!(response["result"], json!(["memory", "tpch"]));
    server.finish();
}

#[test]
fn get_schemas_uses_the_selected_catalog_and_returns_its_schemas() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "Schema", "type": "varchar"}],
            "data": [["demo"], ["information_schema"]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        }
    });

    let response = metadata_handler::get_schemas(json!(12), &params);

    assert_eq!(response["result"], json!(["demo", "information_schema"]));
    let requests = server.finish();
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("x-trino-catalog: memory"));
}

#[test]
fn get_tables_returns_table_names_with_the_selected_schema() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "Table", "type": "varchar"}],
            "data": [["types_test"], ["many_rows"]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo"
    });

    let response = metadata_handler::get_tables(json!(13), &params);

    assert_eq!(response["result"][0]["name"], "types_test");
    assert_eq!(response["result"][0]["schema"], "demo");
    assert!(response["result"][0]["comment"].is_null());
    let requests = server.finish();
    let request = requests[0].to_ascii_lowercase();
    assert!(request.contains("x-trino-schema: demo"));
    assert!(request.ends_with("show tables"));
}

#[test]
fn get_columns_returns_type_nullability_and_default_without_interpolating_identifiers() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [
                {"name": "column_name", "type": "varchar"},
                {"name": "data_type", "type": "varchar"},
                {"name": "is_nullable", "type": "varchar"},
                {"name": "column_default", "type": "varchar"}
            ],
            "data": [
                ["amount", "decimal(38, 9)", "NO", null],
                ["notes", "varchar", "YES", "'n/a'"]
            ]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo",
        "table": "O'Brien"
    });

    let response = metadata_handler::get_columns(json!(14), &params);

    assert_eq!(response["result"][0]["name"], "amount");
    assert_eq!(response["result"][0]["data_type"], "decimal(38, 9)");
    assert_eq!(response["result"][0]["is_nullable"], false);
    assert_eq!(response["result"][0]["default_value"], Value::Null);
    assert_eq!(response["result"][1]["is_nullable"], true);
    assert_eq!(response["result"][1]["default_value"], "'n/a'");

    let requests = server.finish();
    assert!(requests[0].contains("table_name = 'O''Brien'"));
}

#[test]
fn get_views_returns_view_names_in_the_selected_schema() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "table_name", "type": "varchar"}],
            "data": [["v_types"]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo"
    });

    let response = metadata_handler::get_views(json!(15), &params);

    assert_eq!(
        response["result"],
        json!([{"name": "v_types", "schema": "demo"}])
    );
    server.finish();
}

#[test]
fn get_view_definition_returns_the_definition_string() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "view_definition", "type": "varchar"}],
            "data": [["SELECT dec_col FROM types_test"]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo",
        "view": "v_types"
    });

    let response = metadata_handler::get_view_definition(json!(16), &params);

    assert_eq!(response["result"], "SELECT dec_col FROM types_test");
    server.finish();
}

#[test]
fn get_view_columns_uses_the_view_name_for_column_discovery() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [
                {"name": "column_name", "type": "varchar"},
                {"name": "data_type", "type": "varchar"},
                {"name": "is_nullable", "type": "varchar"},
                {"name": "column_default", "type": "varchar"}
            ],
            "data": [["dec_col", "decimal(38, 9)", "YES", null]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo",
        "view": "v_types"
    });

    let response = metadata_handler::get_view_columns(json!(17), &params);

    assert_eq!(response["result"][0]["name"], "dec_col");
    assert_eq!(response["result"][0]["is_nullable"], true);
    let requests = server.finish();
    assert!(requests[0].contains("table_name = 'v_types'"));
}

#[test]
fn execute_query_collects_all_trino_pages_then_applies_requested_page() {
    let server = MockServer::start(vec![
        MockResponse::json(
            "POST",
            "/v1/statement",
            json!({
                "columns": [{"name": "n", "type": "bigint"}],
                "data": [[1], [2], [3]],
                "nextUri": "/v1/statement/q/1"
            }),
        ),
        MockResponse::json("GET", "/v1/statement/q/1", json!({"data": [[4], [5]]})),
    ]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "schema": "demo",
        "query": "SELECT n FROM many_rows ORDER BY n",
        "limit": 2,
        "page": 2
    });

    let response = query_handler::execute_query(json!(18), &params);

    assert_eq!(response["result"]["columns"], json!(["n"]));
    assert_eq!(response["result"]["rows"], json!([[3], [4]]));
    assert_eq!(response["result"]["affected_rows"], 0);
    assert_eq!(response["result"]["truncated"], false);
    assert_eq!(response["result"]["pagination"]["page"], 2);
    assert_eq!(response["result"]["pagination"]["page_size"], 2);
    assert_eq!(response["result"]["pagination"]["total_rows"], 5);
    assert_eq!(response["result"]["pagination"]["has_more"], true);
    let requests = server.finish();
    assert!(requests[0].ends_with("SELECT n FROM many_rows ORDER BY n"));
}

#[test]
fn execute_query_honors_an_explicit_offset() {
    let server = MockServer::start(vec![MockResponse::json(
        "POST",
        "/v1/statement",
        json!({
            "columns": [{"name": "n", "type": "bigint"}],
            "data": [[1], [2], [3], [4], [5]]
        }),
    )]);
    let params = json!({
        "params": {
            "driver": "trino",
            "host": format!("http://{}", server.address),
            "database": "memory",
            "username": "tester"
        },
        "query": "SELECT n FROM many_rows ORDER BY n",
        "limit": 2,
        "offset": 3
    });

    let response = query_handler::execute_query(json!(19), &params);

    assert_eq!(response["result"]["rows"], json!([[4], [5]]));
    assert_eq!(response["result"]["pagination"]["total_rows"], 5);
    assert_eq!(response["result"]["pagination"]["has_more"], false);
    server.finish();
}

#[test]
fn query_cancellation_reaches_trino_and_returns_cancelled_error() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellation server");
    let address = listener.local_addr().expect("server address").to_string();
    let (get_started_tx, get_started_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut submit, _) = listener.accept().expect("accept statement");
        assert!(read_request(&mut submit).starts_with("POST /v1/statement "));
        let submit_response = MockResponse::json(
            "POST",
            "/v1/statement",
            json!({
                "columns": [{"name": "n", "type": "bigint"}],
                "nextUri": "/v1/statement/q/1"
            }),
        );
        write_response(&mut submit, &submit_response);

        let (mut page, _) = listener.accept().expect("accept result page");
        assert!(read_request(&mut page).starts_with("GET /v1/statement/q/1 "));
        get_started_tx.send(()).expect("notify query test");

        let (mut cancel, _) = listener.accept().expect("accept cancellation");
        assert!(read_request(&mut cancel).starts_with("DELETE /v1/statement/q/1 "));
        write_response(
            &mut cancel,
            &MockResponse::raw("DELETE", "/v1/statement/q/1", 204, "No Content", ""),
        );
        write_response(
            &mut page,
            &MockResponse::json("GET", "/v1/statement/q/1", json!({"data": [[1]]})),
        );
    });

    let cancellation = QueryCancellation::default();
    let worker_cancellation = cancellation.clone();
    let query = thread::spawn(move || {
        let mut client = Client::connect(ConnectionParams {
            driver: Some("trino".into()),
            host: Some(format!("http://{address}")),
            port: None,
            database: Some("memory".into()),
            username: Some("tester".into()),
            password: None,
            ssl_mode: None,
        })
        .expect("construct Trino client");
        client.set_cancellation(worker_cancellation);
        client.run_query("SELECT n")
    });

    get_started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("query should start fetching a page");
    cancellation.cancel().expect("Trino accepts cancellation");
    server.join().expect("cancellation server completes");
    let error = query
        .join()
        .expect("query worker completes")
        .expect_err("cancelled query should not return results");
    assert_eq!(
        error.code, -32800,
        "unexpected cancellation response: {error}"
    );
    assert_eq!(error.message, "query cancelled");
}

#[test]
fn cancellation_during_statement_submission_cancels_after_trino_returns_next_uri() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellation server");
    let address = listener.local_addr().expect("server address").to_string();
    let (submit_started_tx, submit_started_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut submit, _) = listener.accept().expect("accept statement");
        assert!(read_request(&mut submit).starts_with("POST /v1/statement "));
        submit_started_tx.send(()).expect("notify query test");
        write_response(
            &mut submit,
            &MockResponse::json(
                "POST",
                "/v1/statement",
                json!({"nextUri": "/v1/statement/q/1"}),
            ),
        );

        let (mut cancel, _) = listener.accept().expect("accept cancellation");
        assert!(read_request(&mut cancel).starts_with("DELETE /v1/statement/q/1 "));
        write_response(
            &mut cancel,
            &MockResponse::raw("DELETE", "/v1/statement/q/1", 204, "No Content", ""),
        );
    });

    let cancellation = QueryCancellation::default();
    let worker_cancellation = cancellation.clone();
    let query = thread::spawn(move || {
        let mut client = Client::connect(ConnectionParams {
            driver: Some("trino".into()),
            host: Some(format!("http://{address}")),
            port: None,
            database: Some("memory".into()),
            username: Some("tester".into()),
            password: None,
            ssl_mode: None,
        })
        .expect("construct Trino client");
        client.set_cancellation(worker_cancellation);
        client.run_query("SELECT count(*) FROM slow_table")
    });

    submit_started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("statement should be submitted");
    cancellation.cancel().expect("mark query as cancelled");
    server.join().expect("cancellation server completes");
    let error = query
        .join()
        .expect("query worker completes")
        .expect_err("cancelled query should not return results");
    assert_eq!(
        error.code, -32800,
        "unexpected cancellation response: {error}"
    );
    assert_eq!(error.message, "query cancelled");
}

#[test]
fn stdio_cancel_notification_is_handled_while_execute_query_is_running() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind cancellation server");
    let address = listener.local_addr().expect("server address").to_string();
    let (get_started_tx, get_started_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut submit, _) = listener.accept().expect("accept statement");
        assert!(read_request(&mut submit).starts_with("POST /v1/statement "));
        write_response(
            &mut submit,
            &MockResponse::json(
                "POST",
                "/v1/statement",
                json!({"nextUri": "/v1/statement/q/1"}),
            ),
        );

        let (mut page, _) = listener.accept().expect("accept result page");
        assert!(read_request(&mut page).starts_with("GET /v1/statement/q/1 "));
        get_started_tx.send(()).expect("notify plugin test");

        let (mut cancel, _) = listener.accept().expect("accept cancellation");
        assert!(read_request(&mut cancel).starts_with("DELETE /v1/statement/q/1 "));
        write_response(
            &mut cancel,
            &MockResponse::raw("DELETE", "/v1/statement/q/1", 204, "No Content", ""),
        );
        write_response(
            &mut page,
            &MockResponse::json("GET", "/v1/statement/q/1", json!({"data": [[1]]})),
        );
    });

    let mut plugin = Command::new(env!("CARGO_BIN_EXE_trino-plugin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start plugin process");
    let request = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "execute_query",
        "params": {
            "params": {
                "driver": "trino",
                "host": format!("http://{address}"),
                "database": "memory",
                "username": "tester"
            },
            "query": "SELECT count(*) FROM slow_table"
        }
    });
    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "cancel",
        "params": {"id": 42}
    });
    let mut stdin = plugin.stdin.take().expect("plugin stdin");
    writeln!(stdin, "{request}").expect("send query request");
    get_started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("plugin should request a result page");
    writeln!(stdin, "{cancel}").expect("send cancellation notification");

    let stdout = plugin.stdout.take().expect("plugin stdout");
    let (response_tx, response_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read plugin response");
        response_tx.send(line).expect("deliver plugin response");
    });
    let response = response_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("plugin should reply to the original request");
    let response: Value = serde_json::from_str(&response).expect("valid JSON-RPC response");
    assert_eq!(response["id"], json!(42));
    assert_eq!(response["error"]["code"], json!(-32800));

    drop(stdin);
    assert!(plugin.wait().expect("plugin exits").success());
    reader.join().expect("stdout reader completes");
    server.join().expect("mock Trino server completes");
}
