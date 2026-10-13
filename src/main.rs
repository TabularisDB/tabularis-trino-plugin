//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//
// Scaffolded utilities (`ConnectionParams`, `quote_identifier`, `paginate`,
// etc.) are unused until you wire them into your handlers. Remove the
// crate-level `allow(dead_code)` once you start using them — the compiler
// will then correctly flag code you forgot to reach.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};

mod client;
mod error;
mod handlers;
mod models;
mod rpc;
mod utils;

fn main() {
    let stdin = io::stdin();
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let (completed_tx, completed_rx) = mpsc::channel::<String>();
    let mut queries: HashMap<String, (client::QueryCancellation, JoinHandle<()>)> = HashMap::new();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        for finished in completed_rx.try_iter() {
            if let Some((_, task)) = queries.remove(&finished) {
                let _ = task.join();
            }
        }

        let request: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(request) => request,
            Err(_) => {
                if !write_response(&stdout, rpc::handle_line(trimmed)) {
                    break;
                }
                continue;
            }
        };
        let method = request.get("method").and_then(serde_json::Value::as_str);
        if method == Some("cancel") {
            if let Some(id) = request.get("params").and_then(|params| params.get("id")) {
                if let Some((cancellation, _)) = queries.get(&id.to_string()) {
                    if let Err(error) = cancellation.cancel() {
                        eprintln!("failed to cancel Trino query {}: {error}", id);
                    }
                }
            }
            continue;
        }

        if method == Some("execute_query") {
            let id = request
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let key = id.to_string();
            let cancellation = client::QueryCancellation::default();
            let worker_cancellation = cancellation.clone();
            let worker_stdout = Arc::clone(&stdout);
            let worker_completed = completed_tx.clone();
            let request_line = trimmed.to_string();
            let task = thread::spawn(move || {
                let response =
                    rpc::handle_line_with_cancellation(&request_line, Some(worker_cancellation));
                let _ = write_response(&worker_stdout, response);
                let _ = worker_completed.send(key);
            });
            queries.insert(id.to_string(), (cancellation, task));
            continue;
        }

        if !write_response(&stdout, rpc::handle_line(trimmed)) {
            break;
        }
    }

    for (_, (_, task)) in queries {
        let _ = task.join();
    }
}

fn write_response(stdout: &Arc<Mutex<io::Stdout>>, response: serde_json::Value) -> bool {
    let mut body = match serde_json::to_string(&response) {
        Ok(body) => body,
        Err(error) => format!(
            "{{\"jsonrpc\":\"2.0\",\"error\":{{\"code\":-32603,\"message\":\"serialization failed: {error}\"}},\"id\":null}}",
        ),
    };
    body.push('\n');
    let Ok(mut out) = stdout.lock() else {
        return false;
    };
    out.write_all(body.as_bytes()).is_ok() && out.flush().is_ok()
}
