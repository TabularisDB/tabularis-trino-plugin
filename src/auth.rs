//! Connection options, credentials, and Trino's OAuth2 external (browser)
//! authentication flow.
//!
//! Tabularis only exposes a fixed set of connection fields, so Trino-specific
//! options are read from a query string appended to the Host field, using the
//! same property names as the Trino JDBC driver where possible:
//!
//! ```text
//! https://trino.example.com?externalAuthentication=true&SSLVerification=NONE
//! ```

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client as HttpClient, RequestBuilder};
use reqwest::header::{HeaderMap, WWW_AUTHENTICATE};
use reqwest::{StatusCode, Url};
use serde_json::Value;

use crate::error::PluginError;

/// Default time to wait for the user to finish the browser login. Kept below
/// Tabularis' default 120 s plugin call timeout so the user gets a useful
/// error instead of a host-side timeout.
pub const DEFAULT_EXTERNAL_AUTH_TIMEOUT: Duration = Duration::from_secs(110);
const TOKEN_POLL_RETRY_DELAY: Duration = Duration::from_secs(1);

const SUPPORTED_OPTIONS: &str = "SSL, SSLVerification, accessToken, externalAuthentication, \
externalAuthenticationTimeout, authentication";

/// How strictly the server's TLS certificate is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslVerification {
    /// Verify the certificate chain and the hostname (default).
    #[default]
    Full,
    /// Verify the certificate chain but not the hostname.
    Ca,
    /// Accept any certificate. Traffic is encrypted but not authenticated.
    None,
}

/// Which authentication scheme the connection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationKind {
    Password,
    Token,
    External,
}

/// Options parsed from the query string of the Host field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostOptions {
    pub ssl: Option<bool>,
    pub ssl_verification: Option<SslVerification>,
    pub access_token: Option<String>,
    pub external_authentication: bool,
    pub external_authentication_timeout: Option<Duration>,
    pub authentication: Option<AuthenticationKind>,
}

/// Split `host?key=value&...` into the host part and its options.
pub fn split_host_options(raw: &str) -> Result<(String, HostOptions), PluginError> {
    let Some((host, query)) = raw.split_once('?') else {
        return Ok((raw.to_string(), HostOptions::default()));
    };
    let parsed = Url::parse(&format!("http://options.invalid/?{query}")).map_err(|error| {
        PluginError::invalid_params(format!("invalid Trino connection options: {error}"))
    })?;

    let mut options = HostOptions::default();
    for (key, value) in parsed.query_pairs() {
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "ssl" => options.ssl = Some(parse_bool("SSL", value)?),
            "sslverification" => {
                options.ssl_verification = Some(match value.to_ascii_uppercase().as_str() {
                    "FULL" => SslVerification::Full,
                    "CA" => SslVerification::Ca,
                    "NONE" => SslVerification::None,
                    other => {
                        return Err(PluginError::invalid_params(format!(
                            "unsupported SSLVerification '{other}'; use FULL, CA, or NONE"
                        )))
                    }
                })
            }
            "accesstoken" => {
                if value.is_empty() {
                    return Err(PluginError::invalid_params(
                        "accessToken must not be empty",
                    ));
                }
                options.access_token = Some(value.to_string());
            }
            "externalauthentication" => {
                options.external_authentication = parse_bool("externalAuthentication", value)?
            }
            "externalauthenticationtimeout" => {
                let seconds = value
                    .trim_end_matches('s')
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .ok_or_else(|| {
                        PluginError::invalid_params(format!(
                            "externalAuthenticationTimeout must be a positive number of seconds, got '{value}'"
                        ))
                    })?;
                options.external_authentication_timeout = Some(Duration::from_secs(seconds));
            }
            "authentication" => {
                options.authentication = Some(match value.to_ascii_lowercase().as_str() {
                    "password" | "basic" => AuthenticationKind::Password,
                    "token" | "jwt" | "bearer" => AuthenticationKind::Token,
                    "external" | "oauth2" | "oauth" | "browser" => AuthenticationKind::External,
                    other => {
                        return Err(PluginError::invalid_params(format!(
                            "unsupported authentication '{other}'; use password, token, or external"
                        )))
                    }
                })
            }
            other => {
                return Err(PluginError::invalid_params(format!(
                    "unsupported Trino connection option '{other}'; supported options: {SUPPORTED_OPTIONS}"
                )))
            }
        }
    }
    Ok((host.to_string(), options))
}

fn parse_bool(name: &str, value: &str) -> Result<bool, PluginError> {
    match value.to_ascii_lowercase().as_str() {
        "" | "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(PluginError::invalid_params(format!(
            "{name} must be true or false, got '{other}'"
        ))),
    }
}

/// Credentials attached to every request, shared with the cancellation path
/// so a token obtained mid-query is also used to cancel it.
#[derive(Clone, Default)]
pub struct Credentials {
    basic: Option<(String, String)>,
    bearer: Arc<Mutex<Option<String>>>,
}

impl Credentials {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn basic(username: String, password: String) -> Self {
        Self {
            basic: Some((username, password)),
            bearer: Arc::default(),
        }
    }

    pub fn bearer(token: Option<String>) -> Self {
        Self {
            basic: None,
            bearer: Arc::new(Mutex::new(token)),
        }
    }

    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some((username, password)) = &self.basic {
            return request.basic_auth(username, Some(password));
        }
        match self.bearer_token() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    pub fn has_basic(&self) -> bool {
        self.basic.is_some()
    }

    pub fn bearer_token(&self) -> Option<String> {
        self.bearer.lock().ok().and_then(|token| token.clone())
    }

    pub fn set_bearer_token(&self, token: Option<String>) {
        if let Ok(mut current) = self.bearer.lock() {
            *current = token;
        }
    }
}

/// The `WWW-Authenticate: Bearer x_redirect_server="…", x_token_server="…"`
/// challenge Trino sends when OAuth2 authentication is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    pub redirect_server: Option<Url>,
    pub token_server: Url,
}

pub fn parse_bearer_challenge(headers: &HeaderMap) -> Option<BearerChallenge> {
    let mut redirect_server = None;
    let mut token_server = None;
    for value in headers.get_all(WWW_AUTHENTICATE) {
        let Ok(value) = value.to_str() else { continue };
        if let Some(url) = challenge_param(value, "x_redirect_server") {
            redirect_server = Url::parse(url).ok();
        }
        if let Some(url) = challenge_param(value, "x_token_server") {
            token_server = Url::parse(url).ok();
        }
    }
    Some(BearerChallenge {
        redirect_server,
        token_server: token_server?,
    })
}

fn challenge_param<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let start = header.find(&format!("{name}=\""))? + name.len() + 2;
    let rest = &header[start..];
    Some(&rest[..rest.find('"')?])
}

/// Process-wide cache of tokens obtained through the browser flow, so every
/// RPC call (each builds a fresh client) does not trigger a new login.
fn token_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// Serializes browser logins so concurrent requests open a single window.
static LOGIN_LOCK: Mutex<()> = Mutex::new(());

pub fn cached_token(key: &str) -> Option<String> {
    token_cache().lock().ok()?.get(key).cloned()
}

fn store_token(key: &str, token: &str) {
    if let Ok(mut cache) = token_cache().lock() {
        cache.insert(key.to_string(), token.to_string());
    }
}

/// Obtain a token for `cache_key`, reusing one another thread obtained while
/// we waited for the login lock. `rejected` is the token Trino just refused.
pub fn obtain_token(
    http: &HttpClient,
    cache_key: &str,
    rejected: Option<&str>,
    challenge: &BearerChallenge,
    timeout: Duration,
) -> Result<String, PluginError> {
    let _guard = LOGIN_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(token) = cached_token(cache_key).filter(|token| Some(token.as_str()) != rejected) {
        return Ok(token);
    }
    // Integration tests compile this file with cfg(test); never launch a
    // real browser from them.
    #[cfg(not(test))]
    let opener = open_in_browser;
    #[cfg(test)]
    let opener = |_: &Url| -> Result<(), String> { Ok(()) };
    let token = run_browser_flow(http, challenge, timeout, &opener)?;
    store_token(cache_key, &token);
    Ok(token)
}

/// Open the login page (if any) and poll Trino's token server until the user
/// finishes logging in.
pub fn run_browser_flow(
    http: &HttpClient,
    challenge: &BearerChallenge,
    timeout: Duration,
    open: &dyn Fn(&Url) -> Result<(), String>,
) -> Result<String, PluginError> {
    let deadline = Instant::now() + timeout;
    let mut open_failure = None;
    if let Some(redirect) = &challenge.redirect_server {
        eprintln!("Trino external authentication: open {redirect} to log in");
        if let Err(error) = open(redirect) {
            eprintln!("could not open a browser for Trino login: {error}");
            open_failure = Some(error);
        }
    }
    let login_hint = || match (&challenge.redirect_server, &open_failure) {
        (Some(redirect), Some(error)) => {
            format!(" A browser could not be opened ({error}); open {redirect} manually and retry.")
        }
        (Some(redirect), None) => format!(" Complete the login at {redirect} and retry."),
        (None, _) => String::new(),
    };

    let mut poll_url = challenge.token_server.clone();
    loop {
        if Instant::now() >= deadline {
            return Err(PluginError::internal(format!(
                "timed out after {}s waiting for Trino external authentication.{}",
                timeout.as_secs(),
                login_hint()
            )));
        }
        let response = match http.get(poll_url.clone()).send() {
            Ok(response) => response,
            Err(error) if error.is_timeout() => continue,
            Err(error) => {
                return Err(PluginError::internal(format!(
                    "could not reach the Trino token server: {error}"
                )))
            }
        };
        let status = response.status();
        if matches!(
            status,
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
        ) {
            thread::sleep(TOKEN_POLL_RETRY_DELAY);
            continue;
        }
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(PluginError::internal(format!(
                "Trino token server returned HTTP {status}: {}",
                body.trim()
            )));
        }
        let body: Value = response.json().map_err(|error| {
            PluginError::internal(format!("Trino token server returned invalid JSON: {error}"))
        })?;
        if let Some(token) = body.get("token").and_then(Value::as_str) {
            return Ok(token.to_string());
        }
        if let Some(error) = body.get("error").and_then(Value::as_str) {
            return Err(PluginError::internal(format!(
                "Trino external authentication failed: {error}"
            )));
        }
        let next = body.get("nextUri").and_then(Value::as_str).ok_or_else(|| {
            PluginError::internal("Trino token server response had no token, nextUri, or error")
        })?;
        poll_url = poll_url.join(next).map_err(|error| {
            PluginError::internal(format!(
                "Trino token server returned an invalid nextUri: {error}"
            ))
        })?;
    }
}

/// Open `url` in the user's default browser without going through a shell.
pub fn open_in_browser(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("refusing to open non-HTTP URL {url}"));
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url.as_str());
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url.as_str()]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url.as_str());
        command
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    // Reap the launcher without blocking the login poll.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
