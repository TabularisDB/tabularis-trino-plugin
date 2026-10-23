//! Test suite for the "query progress and diagnostics" feature:
//! Trino query IDs, queued/running state, elapsed time, and server error details.
#![allow(dead_code)]

#[path = "../src/client.rs"]
mod client;
#[path = "../src/error.rs"]
mod error;
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
use std::thread::{self, JoinHandle};
use std::time::Duration;

use client::Client;
use models::ConnectionParams;
use serde_json::{json, Value};

const QID: &str = "20261023_123456_00001_abcd";

// ---------------------------------------------------------------- mock server

struct Step {
    method: &'static str,
    path: &'static str,
    body: String,
    delay: Duration,
}

fn step(method: &'static str, path: &'static str, body: Value) -> Step {
    Step {
        method,
        path,
        body: body.to_string(),
        delay: Duration::ZERO,
    }
}

fn post(body: Value) -> Step {
    step("POST", "/v1/statement", body)
}

fn get(n: u32, body: Value) -> Step {
    let path: &'static str = Box::leak(format!("/v1/statement/q/{n}").into_boxed_str());
    step("GET", path, body)
}

struct Server {
    address: String,
    task: JoinHandle<()>,
}

impl Server {
    fn start(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().unwrap().to_string();
        let task = thread::spawn(move || {
            for expected in steps {
                let (mut stream, _) = listener.accept().expect("accept");
                let request = read_request(&mut stream);
                let mut parts = request.lines().next().unwrap().split_whitespace();
                assert_eq!(parts.next().unwrap(), expected.method);
                assert_eq!(parts.next().unwrap(), expected.path);
                thread::sleep(expected.delay);
                let body = expected.body.as_bytes();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        Self { address, task }
    }

    fn client(&self) -> Client {
        Client::connect(self.params()).expect("construct client")
    }

    fn params(&self) -> ConnectionParams {
        ConnectionParams {
            driver: Some("trino".into()),
            host: Some(format!("http://{}", self.address)),
            port: None,
            database: Some("memory".into()),
            username: Some("tester".into()),
            password: None,
            ssl_mode: None,
            ssl_ca: None,
            ssl_cert: None,
            ssl_key: None,
        }
    }

    fn rpc_params(&self, sql: &str) -> Value {
        json!({
            "params": {
                "driver": "trino",
                "host": format!("http://{}", self.address),
                "database": "memory",
                "username": "tester"
            },
            "query": sql
        })
    }

    fn finish(self) {
        self.task.join().expect("mock server thread");
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse().unwrap();
            }
        }
        request.push_str(&line);
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    request.push_str(&String::from_utf8_lossy(&body));
    request
}

/// Run a single-statement query that returns the given pages and give back the result.
fn run_pages(steps: Vec<Step>) -> client::QueryResult {
    let server = Server::start(steps);
    let result = server.client().run_query("SELECT 1").expect("query ok");
    server.finish();
    result
}

/// Run a query whose first POST returns the given error body; return the message.
fn error_message(error_page: Value) -> String {
    let server = Server::start(vec![post(error_page)]);
    let err = server.client().run_query("SELECT 1").expect_err("fails");
    server.finish();
    err.message
}

// ------------------------------------------------------- query id & states

#[test]
fn captures_query_id_and_all_states_in_order() {
    let r = run_pages(vec![
        post(json!({"id": QID, "stats": {"state": "QUEUED"}, "nextUri": "/v1/statement/q/1"})),
        get(
            1,
            json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/2"}),
        ),
        get(
            2,
            json!({"id": QID, "columns": [{"name": "a", "type": "integer"}],
                      "data": [[1]], "stats": {"state": "FINISHED"}}),
        ),
    ]);
    assert_eq!(r.query_id.as_deref(), Some(QID));
    assert_eq!(r.query_state.as_deref(), Some("FINISHED"));
    assert_eq!(r.observed_states, ["QUEUED", "RUNNING", "FINISHED"]);
    assert_eq!(r.rows, [json!([1])]);
}

#[test]
fn fast_query_may_only_report_finished() {
    let r = run_pages(vec![post(json!({
        "id": QID,
        "columns": [{"name": "a", "type": "integer"}],
        "data": [[1]],
        "stats": {"state": "FINISHED"}
    }))]);
    assert_eq!(r.observed_states, ["FINISHED"]);
    assert_eq!(r.query_state.as_deref(), Some("FINISHED"));
}

#[test]
fn consecutive_duplicate_states_are_collapsed() {
    let r = run_pages(vec![
        post(json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/1"})),
        get(
            1,
            json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/2"}),
        ),
        get(
            2,
            json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/3"}),
        ),
        get(3, json!({"id": QID, "stats": {"state": "FINISHED"}})),
    ]);
    assert_eq!(r.observed_states, ["RUNNING", "FINISHED"]);
}

#[test]
fn query_id_comes_from_first_page_and_is_not_overwritten() {
    let r = run_pages(vec![
        post(
            json!({"id": "first_id", "stats": {"state": "QUEUED"}, "nextUri": "/v1/statement/q/1"}),
        ),
        get(1, json!({"id": "other_id", "stats": {"state": "FINISHED"}})),
    ]);
    assert_eq!(r.query_id.as_deref(), Some("first_id"));
}

#[test]
fn query_id_is_found_even_when_only_a_later_page_has_it() {
    let r = run_pages(vec![
        post(json!({"stats": {"state": "QUEUED"}, "nextUri": "/v1/statement/q/1"})),
        get(1, json!({"id": QID, "stats": {"state": "FINISHED"}})),
    ]);
    assert_eq!(r.query_id.as_deref(), Some(QID));
}

#[test]
fn missing_id_and_stats_yield_empty_diagnostics() {
    let r = run_pages(vec![post(json!({
        "columns": [{"name": "a", "type": "integer"}],
        "data": [[1]]
    }))]);
    assert_eq!(r.query_id, None);
    assert_eq!(r.query_state, None);
    assert!(r.observed_states.is_empty());
    assert!(r.stats.is_null());
}

#[test]
fn state_non_string_is_ignored() {
    let r = run_pages(vec![post(json!({"id": QID, "stats": {"state": 5}}))]);
    assert_eq!(r.query_state, None);
    assert!(r.observed_states.is_empty());
}

#[test]
fn page_without_stats_does_not_clear_previous_state() {
    let r = run_pages(vec![
        post(json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/1"})),
        get(
            1,
            json!({"columns": [{"name": "a", "type": "integer"}], "data": [[1]]}),
        ),
    ]);
    assert_eq!(r.query_state.as_deref(), Some("RUNNING"));
    assert_eq!(r.observed_states, ["RUNNING"]);
}

// ------------------------------------------------------------------- stats

#[test]
fn stats_only_keep_the_ui_safe_allowlist() {
    let r = run_pages(vec![post(json!({
        "id": QID,
        "stats": {
            "state": "FINISHED",
            "queued": false,
            "scheduled": true,
            "nodes": 1,
            "totalSplits": 4,
            "completedSplits": 4,
            "runningSplits": 0,
            "queuedSplits": 0,
            "cpuTimeMillis": 7,
            "wallTimeMillis": 9,
            "queuedTimeMillis": 1,
            "elapsedTimeMillis": 12,
            "processedRows": 100,
            "processedBytes": 2048,
            "rootStage": {"huge": "tree"},
            "peakMemoryBytes": 1,
            "somethingUnknown": "x"
        }
    }))]);
    let s = r.stats.as_object().expect("stats object");
    for key in [
        "queued",
        "scheduled",
        "nodes",
        "totalSplits",
        "completedSplits",
        "runningSplits",
        "queuedSplits",
        "cpuTimeMillis",
        "wallTimeMillis",
        "queuedTimeMillis",
        "elapsedTimeMillis",
        "processedRows",
        "processedBytes",
    ] {
        assert!(s.contains_key(key), "missing {key}");
    }
    for key in ["state", "rootStage", "peakMemoryBytes", "somethingUnknown"] {
        assert!(!s.contains_key(key), "unexpected {key}");
    }
    assert_eq!(s["totalSplits"], 4);
    assert_eq!(s["processedBytes"], 2048);
}

#[test]
fn string_duration_stats_are_preserved() {
    let r = run_pages(vec![post(json!({
        "id": QID,
        "stats": {"state": "FINISHED", "cpuTime": "1.5ms", "wallTime": "2ms",
                  "queuedTime": "3ms", "elapsedTime": "4ms"}
    }))]);
    assert_eq!(r.stats["cpuTime"], "1.5ms");
    assert_eq!(r.stats["wallTime"], "2ms");
    assert_eq!(r.stats["queuedTime"], "3ms");
    assert_eq!(r.stats["elapsedTime"], "4ms");
}

#[test]
fn stats_reflect_the_latest_page() {
    let r = run_pages(vec![
        post(
            json!({"id": QID, "stats": {"state": "QUEUED", "queued": true, "queuedTime": "3ms"},
                    "nextUri": "/v1/statement/q/1"}),
        ),
        get(
            1,
            json!({"id": QID, "stats": {"state": "FINISHED", "completedSplits": 1,
                                           "totalSplits": 1}}),
        ),
    ]);
    assert_eq!(r.stats["completedSplits"], 1);
    assert!(
        r.stats.get("queuedTime").is_none(),
        "stale stats must be replaced"
    );
}

// ------------------------------------------------------------ elapsed time

#[test]
fn elapsed_time_covers_submission_and_all_pages() {
    let mut slow_post = post(json!({"id": QID, "stats": {"state": "QUEUED"},
                                    "nextUri": "/v1/statement/q/1"}));
    slow_post.delay = Duration::from_millis(60);
    let mut slow_get = get(1, json!({"id": QID, "stats": {"state": "FINISHED"}}));
    slow_get.delay = Duration::from_millis(60);
    let r = run_pages(vec![slow_post, slow_get]);
    assert!(
        r.elapsed_time_ms >= 120,
        "elapsed {}ms should include both delays",
        r.elapsed_time_ms
    );
    assert!(r.elapsed_time_ms < 10_000);
}

#[test]
fn elapsed_time_is_reported_for_instant_queries() {
    let r = run_pages(vec![post(
        json!({"id": QID, "stats": {"state": "FINISHED"}}),
    )]);
    assert!(r.elapsed_time_ms < 5_000);
}

// ---------------------------------------------------- execute_query handler

#[test]
fn handler_returns_query_info_block() {
    let server = Server::start(vec![
        post(
            json!({"id": QID, "stats": {"state": "QUEUED", "queuedTime": "3ms"},
                    "nextUri": "/v1/statement/q/1"}),
        ),
        get(
            1,
            json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/2"}),
        ),
        get(
            2,
            json!({"id": QID, "columns": [{"name": "answer", "type": "bigint"}],
                      "data": [[42]],
                      "stats": {"state": "FINISHED", "completedSplits": 1, "totalSplits": 1}}),
        ),
    ]);
    let response = query_handler::execute_query(json!(1), &server.rpc_params("SELECT 42"));
    let info = &response["result"]["query_info"];

    assert_eq!(info["query_id"], QID);
    assert_eq!(info["state"], "FINISHED");
    assert_eq!(
        info["observed_states"],
        json!(["QUEUED", "RUNNING", "FINISHED"])
    );
    assert!(info["elapsed_time_ms"].is_u64());
    assert_eq!(info["trino_stats"]["completedSplits"], 1);
    assert_eq!(response["result"]["rows"], json!([[42]]));
    assert_eq!(response["result"]["columns"], json!(["answer"]));
    server.finish();
}

#[test]
fn handler_query_info_is_present_with_null_fields_when_trino_sends_nothing() {
    let server = Server::start(vec![post(json!({
        "columns": [{"name": "a", "type": "integer"}],
        "data": [[1]]
    }))]);
    let response = query_handler::execute_query(json!(2), &server.rpc_params("SELECT 1"));
    let info = &response["result"]["query_info"];
    assert!(info["query_id"].is_null());
    assert!(info["state"].is_null());
    assert_eq!(info["observed_states"], json!([]));
    assert!(info["trino_stats"].is_null());
    assert!(info["elapsed_time_ms"].is_u64());
    server.finish();
}

#[test]
fn handler_query_info_survives_result_pagination() {
    let server = Server::start(vec![post(json!({
        "id": QID,
        "columns": [{"name": "n", "type": "integer"}],
        "data": [[1], [2], [3]],
        "stats": {"state": "FINISHED"}
    }))]);
    let mut params = server.rpc_params("SELECT n FROM t");
    params["page"] = json!(1);
    params["limit"] = json!(2);
    let response = query_handler::execute_query(json!(3), &params);
    assert_eq!(response["result"]["rows"], json!([[1], [2]]));
    assert_eq!(response["result"]["pagination"]["total_rows"], 3);
    assert_eq!(response["result"]["query_info"]["query_id"], QID);
    assert_eq!(response["result"]["query_info"]["state"], "FINISHED");
    server.finish();
}

#[test]
fn update_count_and_diagnostics_coexist() {
    let server = Server::start(vec![post(json!({
        "id": QID,
        "updateType": "INSERT",
        "updateCount": 5,
        "stats": {"state": "FINISHED"}
    }))]);
    let response =
        query_handler::execute_query(json!(4), &server.rpc_params("INSERT INTO t VALUES 1"));
    assert_eq!(response["result"]["affected_rows"], 5);
    assert_eq!(response["result"]["query_info"]["query_id"], QID);
    server.finish();
}

#[test]
fn handler_error_response_carries_query_id_and_details() {
    let server = Server::start(vec![post(json!({
        "id": QID,
        "error": {
            "errorName": "SYNTAX_ERROR",
            "message": "mismatched input 'FROM'",
            "errorCode": {"code": 1, "name": "SYNTAX_ERROR", "type": "USER_ERROR"},
            "errorLocation": {"lineNumber": 1, "columnNumber": 8}
        }
    }))]);
    let response = query_handler::execute_query(json!(5), &server.rpc_params("SELECT FROM"));
    assert!(response.get("result").is_none());
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains(QID));
    assert!(message.contains("SYNTAX_ERROR"));
    assert!(message.contains("line 1, column 8"));
    assert_eq!(response["id"], 5);
    server.finish();
}

// ----------------------------------------------------------- error details

#[test]
fn error_with_every_detail() {
    let m = error_message(json!({
        "id": QID,
        "error": {
            "errorName": "SYNTAX_ERROR",
            "message": "mismatched input 'FROM'",
            "errorCode": {"code": 1, "name": "SYNTAX_ERROR", "type": "USER_ERROR"},
            "errorLocation": {"lineNumber": 3, "columnNumber": 14},
            "failureInfo": {"message": "parser rejected the statement"}
        }
    }));
    assert_eq!(
        m,
        format!(
            "Trino query {QID} failed (SYNTAX_ERROR, code 1, USER_ERROR) at line 3, column 14: \
             mismatched input 'FROM'; cause: parser rejected the statement"
        )
    );
}

#[test]
fn error_with_only_name_and_message_keeps_legacy_content() {
    let m =
        error_message(json!({"error": {"errorName": "USER_ERROR", "message": "Access denied"}}));
    assert!(m.contains("USER_ERROR"));
    assert!(m.contains("Access denied"));
    assert!(!m.contains("query "), "no id means no query segment: {m}");
    assert!(!m.contains("code"));
    assert!(!m.contains("line"));
    assert!(!m.contains("cause"));
}

#[test]
fn error_with_line_but_no_column() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "SYNTAX_ERROR", "message": "bad",
                  "errorLocation": {"lineNumber": 7}}
    }));
    assert!(m.contains("at line 7"));
    assert!(!m.contains("column"));
}

#[test]
fn error_with_column_only_omits_location() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "SYNTAX_ERROR", "message": "bad",
                  "errorLocation": {"columnNumber": 7}}
    }));
    assert!(!m.contains("column"));
    assert!(!m.contains(" at line"));
}

#[test]
fn error_without_code_still_shows_type() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "TABLE_NOT_FOUND", "message": "Table 'x' does not exist",
                  "errorCode": {"type": "USER_ERROR"}}
    }));
    assert!(m.contains("TABLE_NOT_FOUND, USER_ERROR"));
    assert!(!m.contains("code "));
}

#[test]
fn error_type_is_shown_for_internal_and_external_errors() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "GENERIC_INTERNAL_ERROR", "message": "boom",
                  "errorCode": {"code": 65536, "type": "INTERNAL_ERROR"}}
    }));
    assert!(m.contains("code 65536"));
    assert!(m.contains("INTERNAL_ERROR"));
}

#[test]
fn failure_info_identical_to_message_is_not_repeated() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "USER_ERROR", "message": "Access denied",
                  "failureInfo": {"message": "Access denied"}}
    }));
    assert!(!m.contains("cause"));
    assert_eq!(m.matches("Access denied").count(), 1);
}

#[test]
fn empty_failure_info_message_is_ignored() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "USER_ERROR", "message": "Access denied",
                  "failureInfo": {"message": ""}}
    }));
    assert!(!m.contains("cause"));
}

#[test]
fn failure_info_message_is_shown_as_cause_when_different() {
    let m = error_message(json!({
        "id": QID,
        "error": {"errorName": "USER_ERROR", "message": "Query failed",
                  "failureInfo": {"message": "root cause text"}}
    }));
    assert!(m.ends_with("; cause: root cause text"));
}

#[test]
fn error_in_a_later_page_includes_that_pages_query_id() {
    let server = Server::start(vec![
        post(json!({"id": QID, "stats": {"state": "RUNNING"}, "nextUri": "/v1/statement/q/1"})),
        get(
            1,
            json!({
                "id": QID,
                "error": {"errorName": "EXCEEDED_TIME_LIMIT", "message": "Query exceeded time limit",
                          "errorCode": {"code": 131079, "type": "INSUFFICIENT_RESOURCES"}}
            }),
        ),
    ]);
    let err = server.client().run_query("SELECT 1").expect_err("fails");
    assert!(err.message.contains(QID));
    assert!(err.message.contains("EXCEEDED_TIME_LIMIT"));
    assert!(err.message.contains("INSUFFICIENT_RESOURCES"));
    server.finish();
}

#[test]
fn user_canceled_is_still_a_cancellation_not_a_diagnostic_error() {
    let server = Server::start(vec![post(json!({
        "id": QID,
        "error": {"errorName": "USER_CANCELED", "message": "Query was canceled"}
    }))]);
    let err = server
        .client()
        .run_query("SELECT 1")
        .expect_err("cancelled");
    assert_eq!(err.code, -32800);
    assert_eq!(err.message, "query cancelled");
    server.finish();
}
