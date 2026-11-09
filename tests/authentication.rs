//! Connection options, token authentication, and the OAuth2 external
//! (browser) authentication flow.
#![allow(dead_code)]

#[path = "../src/auth.rs"]
mod auth;
#[path = "../src/client.rs"]
mod client;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/models.rs"]
mod models;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use auth::{split_host_options, AuthenticationKind, SslVerification};
use client::Client;
use models::ConnectionParams;
use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};
use serde_json::json;

/// `{addr}` in headers and bodies is replaced with the server address.
struct Reply {
    method: &'static str,
    path: &'static str,
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

fn reply(method: &'static str, path: &'static str, status: u16, body: serde_json::Value) -> Reply {
    Reply {
        method,
        path,
        status,
        headers: Vec::new(),
        body: body.to_string(),
    }
}

const CHALLENGE: &str = "Bearer x_redirect_server=\"http://{addr}/oauth2/token/initiate/abc\", x_token_server=\"http://{addr}/oauth2/token/abc\"";

struct Server {
    address: String,
    task: JoinHandle<Vec<String>>,
}

impl Server {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("address").to_string();
        let server_address = address.clone();
        let task = thread::spawn(move || {
            let mut requests = Vec::new();
            for expected in replies {
                let (mut stream, _) = listener.accept().expect("accept");
                let request = read_request(&mut stream);
                let mut parts = request.lines().next().unwrap_or("").split_whitespace();
                assert_eq!(parts.next(), Some(expected.method), "request: {request}");
                assert_eq!(parts.next(), Some(expected.path), "request: {request}");
                requests.push(request);

                let body = expected.body.replace("{addr}", &server_address);
                let mut head = format!(
                    "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    expected.status,
                    body.len()
                );
                for (name, value) in &expected.headers {
                    head.push_str(&format!(
                        "{name}: {}\r\n",
                        value.replace("{addr}", &server_address)
                    ));
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).expect("write head");
                stream.write_all(body.as_bytes()).expect("write body");
            }
            requests
        });
        Self { address, task }
    }

    fn params(
        &self,
        options: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> ConnectionParams {
        ConnectionParams {
            driver: Some("trino".into()),
            host: Some(format!("http://{}{options}", self.address)),
            port: None,
            database: Some("memory".into()),
            username: username.map(Into::into),
            password: password.map(Into::into),
            ssl_mode: None,
            ssl_ca: None,
            ssl_cert: None,
            ssl_key: None,
        }
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
        reader.read_line(&mut line).expect("read header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().expect("content length");
            }
        }
        request.push_str(&line);
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read body");
    request.push_str(&String::from_utf8_lossy(&body));
    request
}

fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn params(host: &str, username: Option<&str>, password: Option<&str>) -> ConnectionParams {
    ConnectionParams {
        driver: Some("trino".into()),
        host: Some(host.into()),
        port: None,
        database: None,
        username: username.map(Into::into),
        password: password.map(Into::into),
        ssl_mode: None,
        ssl_ca: None,
        ssl_cert: None,
        ssl_key: None,
    }
}

fn connect_error(params: ConnectionParams) -> String {
    match Client::connect(params) {
        Ok(_) => panic!("connection settings should be rejected"),
        Err(error) => error.message,
    }
}

const RESULT: &str = r#"{"columns":[{"name":"_col0","type":"integer"}],"data":[[1]]}"#;

#[test]
fn parses_jdbc_style_options_from_the_host_field() {
    let (host, options) = split_host_options(
        "https://trino.example.com:8443?SSL=true&SSLVerification=none&externalAuthentication=true&externalAuthenticationTimeout=30",
    )
    .expect("options");
    assert_eq!(host, "https://trino.example.com:8443");
    assert_eq!(options.ssl, Some(true));
    assert_eq!(options.ssl_verification, Some(SslVerification::None));
    assert!(options.external_authentication);
    assert_eq!(
        options.external_authentication_timeout,
        Some(Duration::from_secs(30))
    );

    let (host, options) =
        split_host_options("trino.example.com?accessToken=a.b%2Bc&authentication=token")
            .expect("options");
    assert_eq!(host, "trino.example.com");
    assert_eq!(options.access_token.as_deref(), Some("a.b+c"));
    assert_eq!(options.authentication, Some(AuthenticationKind::Token));

    let (host, options) = split_host_options("trino.example.com").expect("no options");
    assert_eq!(host, "trino.example.com");
    assert_eq!(options, Default::default());
}

#[test]
fn rejects_unknown_or_invalid_options() {
    let error = split_host_options("host?user=alice").unwrap_err();
    assert!(error
        .message
        .contains("unsupported Trino connection option 'user'"));
    let error = split_host_options("host?SSLVerification=maybe").unwrap_err();
    assert!(error.message.contains("FULL, CA, or NONE"));
    let error = split_host_options("host?externalAuthentication=perhaps").unwrap_err();
    assert!(error.message.contains("true or false"));
}

#[test]
fn parses_trino_oauth2_bearer_challenge() {
    let mut headers = HeaderMap::new();
    headers.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Trino\""),
    );
    headers.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static(
            "Bearer x_redirect_server=\"https://trino/oauth2/token/initiate/1\", x_token_server=\"https://trino/oauth2/token/1\"",
        ),
    );
    let challenge = auth::parse_bearer_challenge(&headers).expect("challenge");
    assert_eq!(
        challenge.redirect_server.unwrap().as_str(),
        "https://trino/oauth2/token/initiate/1"
    );
    assert_eq!(
        challenge.token_server.as_str(),
        "https://trino/oauth2/token/1"
    );

    let mut basic_only = HeaderMap::new();
    basic_only.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Trino\""),
    );
    assert!(auth::parse_bearer_challenge(&basic_only).is_none());
}

#[test]
fn ssl_verification_and_ssl_options_build_a_client() {
    for options in ["?SSLVerification=NONE", "?SSLVerification=CA", "?SSL=true"] {
        let result = Client::connect(params(
            &format!("trino.example.com{options}"),
            Some("alice"),
            Some("secret"),
        ));
        assert!(result.is_ok(), "{options} should be accepted");
    }

    let error = connect_error(params(
        "http://trino.example.com?SSLVerification=NONE",
        None,
        None,
    ));
    assert!(error.contains("SSLVerification requires HTTPS"), "{error}");
}

#[test]
fn token_and_external_authentication_require_https_off_loopback() {
    let error = connect_error(params(
        "http://trino.example.com?accessToken=abc",
        None,
        None,
    ));
    assert!(
        error.contains("token authentication requires HTTPS"),
        "{error}"
    );
    let error = connect_error(params(
        "http://trino.example.com?externalAuthentication=true",
        None,
        None,
    ));
    assert!(
        error.contains("external authentication requires HTTPS"),
        "{error}"
    );
}

#[test]
fn rejects_conflicting_authentication_settings() {
    let error = connect_error(params(
        "https://trino.example.com?externalAuthentication=true",
        Some("alice"),
        Some("secret"),
    ));
    assert!(error.contains("clear the password field"), "{error}");

    let error = connect_error(params(
        "https://trino.example.com?accessToken=abc",
        None,
        Some("secret"),
    ));
    assert!(error.contains("not both"), "{error}");

    let error = connect_error(params(
        "https://trino.example.com?authentication=token",
        None,
        None,
    ));
    assert!(error.contains("requires a token"), "{error}");
}

#[test]
fn access_token_option_is_sent_as_bearer_without_a_default_user() {
    let server = Server::start(vec![Reply {
        body: RESULT.into(),
        ..reply("POST", "/v1/statement", 200, json!(null))
    }]);
    let mut client = Client::connect(server.params("?accessToken=jwt-1", None, None)).unwrap();
    client.run_query("SELECT 1").expect("query");

    let requests = server.finish();
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer jwt-1"));
    assert_eq!(header(&requests[0], "x-trino-user"), None);
}

#[test]
fn password_field_is_used_as_token_with_authentication_token() {
    let server = Server::start(vec![Reply {
        body: RESULT.into(),
        ..reply("POST", "/v1/statement", 200, json!(null))
    }]);
    let mut client =
        Client::connect(server.params("?authentication=token", Some("alice"), Some("jwt-2")))
            .unwrap();
    client.run_query("SELECT 1").expect("query");

    let requests = server.finish();
    assert_eq!(header(&requests[0], "authorization"), Some("Bearer jwt-2"));
    assert_eq!(header(&requests[0], "x-trino-user"), Some("alice"));
}

#[test]
fn external_authentication_runs_the_browser_flow_and_caches_the_token() {
    let server = Server::start(vec![
        Reply {
            headers: vec![("WWW-Authenticate", CHALLENGE)],
            ..reply("POST", "/v1/statement", 401, json!(null))
        },
        reply(
            "GET",
            "/oauth2/token/abc",
            200,
            json!({"nextUri": "http://{addr}/oauth2/token/abc/2"}),
        ),
        reply("GET", "/oauth2/token/abc/2", 503, json!(null)),
        reply(
            "GET",
            "/oauth2/token/abc/2",
            200,
            json!({"token": "oauth-token"}),
        ),
        Reply {
            body: r#"{"id":"q1","nextUri":"http://{addr}/v1/statement/q1/1"}"#.into(),
            ..reply("POST", "/v1/statement", 200, json!(null))
        },
        Reply {
            body: RESULT.into(),
            ..reply("GET", "/v1/statement/q1/1", 200, json!(null))
        },
        // A second client for the same coordinator reuses the cached token.
        Reply {
            body: RESULT.into(),
            ..reply("POST", "/v1/statement", 200, json!(null))
        },
    ]);

    let mut client =
        Client::connect(server.params("?externalAuthentication=true", None, None)).unwrap();
    let result = client.run_query("SELECT 1").expect("query after login");
    assert_eq!(result.rows, vec![json!([1])]);

    let mut second =
        Client::connect(server.params("?externalAuthentication=true", None, None)).unwrap();
    second
        .run_query("SELECT 1")
        .expect("query with cached token");

    let requests = server.finish();
    assert_eq!(header(&requests[0], "authorization"), None);
    assert_eq!(header(&requests[0], "x-trino-user"), None);
    assert_eq!(header(&requests[1], "authorization"), None);
    for index in [4, 5, 6] {
        assert_eq!(
            header(&requests[index], "authorization"),
            Some("Bearer oauth-token"),
            "request {index}"
        );
    }
}

#[test]
fn external_authentication_reports_token_server_errors() {
    let server = Server::start(vec![
        Reply {
            headers: vec![("WWW-Authenticate", CHALLENGE)],
            ..reply("POST", "/v1/statement", 401, json!(null))
        },
        reply(
            "GET",
            "/oauth2/token/abc",
            200,
            json!({"error": "access denied"}),
        ),
    ]);
    let mut client =
        Client::connect(server.params("?externalAuthentication=true", Some("alice"), None))
            .unwrap();
    let error = client.run_query("SELECT 1").unwrap_err();
    assert!(error.message.contains("access denied"), "{}", error.message);
    server.finish();
}

#[test]
fn unauthenticated_oauth2_server_suggests_external_authentication() {
    let server = Server::start(vec![Reply {
        headers: vec![("WWW-Authenticate", CHALLENGE)],
        body: "Unauthorized".into(),
        ..reply("POST", "/v1/statement", 401, json!(null))
    }]);
    let mut client = Client::connect(server.params("", Some("alice"), None)).unwrap();
    let error = client.run_query("SELECT 1").unwrap_err();
    assert!(
        error.message.contains("externalAuthentication=true"),
        "{}",
        error.message
    );
    server.finish();
}
