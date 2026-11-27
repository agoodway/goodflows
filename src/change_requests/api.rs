//! The change request API client: one request per call, no retries, with the
//! `[reporting]` project, URL and key that `history` uses.
//!
//! Responses are untrusted. A failure names the command, the HTTP status and
//! the server's error body (bounded and escaped), which the server never
//! fills with submitted values; it never names the URL or the key.

use crate::config::Reporting;
use crate::reporting::publish::{self, Credentials};
use anyhow::{anyhow, bail, Result};
use reqwest::{header, Method, StatusCode, Url};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::watch;

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Longest server error body quoted in a failure message.
const MAX_ERROR_BYTES: usize = 2048;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// The whole exchange; long enough for a 16 MiB Git facts body and a
/// 30-second event long-poll.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub enum Body {
    None,
    Json(Value),
    Markdown(Vec<u8>),
}

pub struct Api {
    target: publish::Target,
    client: reqwest::Client,
    /// The command named in failures, such as `cr open`.
    command: String,
    cancel: watch::Receiver<Option<i32>>,
    token_env: String,
}

impl Api {
    pub fn new(
        reporting: Option<&Reporting>,
        command: &str,
        cancel: watch::Receiver<Option<i32>>,
    ) -> Result<Self> {
        let Some(reporting) = reporting else {
            bail!("{command} requires a top-level [reporting] block naming the project; add [reporting] with project = \"<slug>\" to the config");
        };
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let key = read(&reporting.token_env);
        if key.is_some() {
            crate::reporting::protect_process();
        }
        let target = match publish::resolve(
            read(&reporting.url_env).as_deref(),
            key.as_deref(),
            &reporting.project,
        ) {
            Credentials::Ready(target) => target,
            Credentials::Missing => bail!(
                "{command} requires {} and {} to be set (reporting url_env and token_env)",
                reporting.url_env,
                reporting.token_env
            ),
            Credentials::InvalidUrl => bail!(
                "{} must be an origin-only https URL (http only for loopback)",
                reporting.url_env
            ),
            Credentials::InvalidProject => bail!("reporting project is not a valid slug"),
        };
        let client = target
            .client(CONNECT_TIMEOUT)
            .map_err(|_| anyhow!("{command} failed: could not create an HTTP client"))?;
        Ok(Self {
            target,
            client,
            command: command.to_owned(),
            cancel,
            token_env: reporting.token_env.clone(),
        })
    }

    /// The variable holding the API key, which git child processes never get.
    pub fn token_env(&self) -> &str {
        &self.token_env
    }

    /// The command named in failures.
    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn cancel(&self) -> watch::Receiver<Option<i32>> {
        self.cancel.clone()
    }

    /// `/api/v1/projects/<project>/<segments...>`, each segment escaped.
    fn url(&self, segments: &[&str], query: &[(&str, String)]) -> Url {
        let mut url = self.target.api("/api/v1/projects/");
        url.path_segments_mut()
            .expect("an https or loopback http URL has path segments")
            .pop_if_empty()
            .push(self.target.project())
            .extend(segments);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        url
    }

    pub async fn get(&self, segments: &[&str], query: &[(&str, String)]) -> Result<Value> {
        Ok(self.send(Method::GET, segments, query, Body::None).await?.1)
    }

    pub async fn post(&self, segments: &[&str], body: Body) -> Result<Value> {
        Ok(self.send(Method::POST, segments, &[], body).await?.1)
    }

    /// One request. A `2xx` returns its status and JSON body (`Null` when
    /// empty); anything else fails with an [`HttpError`].
    pub async fn send(
        &self,
        method: Method,
        segments: &[&str],
        query: &[(&str, String)],
        body: Body,
    ) -> Result<(u16, Value)> {
        let command = &self.command;
        let mut request = self
            .client
            .request(method, self.url(segments, query))
            .header(header::ACCEPT, "application/json")
            .bearer_auth(self.target.key());
        request = match body {
            Body::None => request,
            Body::Json(value) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&value)?),
            Body::Markdown(bytes) => request
                .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
                .body(bytes),
        };
        let exchange = async {
            let mut response = request.send().await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Ok((status, retry_after, None));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, reqwest::Error>((status, retry_after, Some(bytes)))
        };
        let mut cancel = self.cancel.clone();
        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(Option::is_some) => bail!("{command} interrupted"),
            result = tokio::time::timeout(REQUEST_TIMEOUT, exchange) => result,
        };
        let (status, retry_after, bytes) = match result {
            Err(_) => bail!("{command} failed: timed out"),
            Ok(Err(error)) if error.is_timeout() => bail!("{command} failed: timed out"),
            Ok(Err(_)) => bail!("{command} failed: could not reach the server"),
            Ok(Ok(parts)) => parts,
        };
        let Some(bytes) = bytes else {
            bail!("{command} failed: the response exceeds 16 MiB");
        };
        let parsed = if bytes.is_empty() {
            Some(Value::Null)
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        if status.is_success() {
            return parsed
                .map(|body| (status.as_u16(), body))
                .ok_or_else(|| anyhow!("{command} failed: the server's response is not JSON"));
        }
        let body = parsed.unwrap_or(Value::Null);
        Err(HttpError {
            message: http_error(command, status, &body),
            status: status.as_u16(),
            body,
            retry_after,
        }
        .into())
    }
}

/// A non-`2xx` response. Callers downcast to it to act on the status or
/// the parsed body; its message is the generic failure.
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    /// The parsed body, `Null` when empty or not JSON. Untrusted.
    pub body: Value,
    /// The `retry-after` header in seconds, when it is one.
    pub retry_after: Option<u64>,
    message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

impl HttpError {
    /// The failure's HTTP error, when it is one with this status.
    pub fn of(error: &anyhow::Error, status: u16) -> Option<&HttpError> {
        error
            .downcast_ref::<HttpError>()
            .filter(|e| e.status == status)
    }
}

/// Replaces a `409` failure with `explain`'s message when it recognizes
/// the server's `errors` object; any other failure stays as it is.
pub fn explain_conflict(
    error: anyhow::Error,
    explain: impl FnOnce(&Value) -> Option<String>,
) -> anyhow::Error {
    match HttpError::of(&error, 409).and_then(|e| explain(&e.body["errors"])) {
        Some(message) => anyhow!(message),
        None => error,
    }
}

/// A value from a server body for a message: bounded and escaped.
pub fn quoted(value: &Value) -> String {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Null => "unknown".to_owned(),
        other => other.to_string(),
    };
    let mut end = text.len().min(256);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    crate::output::visible(&text[..end]).into_owned()
}

/// The failure message: the command, the status, and the server's body.
fn http_error(command: &str, status: StatusCode, body: &Value) -> String {
    let code = status.as_u16();
    let reason = match code {
        300..=399 => "redirect not followed",
        400 => "bad request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not found",
        406 => "not acceptable",
        409 => "conflict",
        410 => "gone",
        413 => "body too large",
        415 => "unsupported media type",
        422 => "request rejected",
        500..=599 => "server error",
        _ => "unexpected response",
    };
    let mut message = format!("{command} failed: {reason} (HTTP {code})");
    if !body.is_null() {
        let mut detail = body.to_string();
        if detail.len() > MAX_ERROR_BYTES {
            let mut end = MAX_ERROR_BYTES;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
            detail.push('…');
        }
        message.push_str(": ");
        message.push_str(&crate::output::visible(&detail));
    }
    message
}

/// The `data` of a response, which every documented response has.
pub fn data(mut body: Value, command: &str) -> Result<Value> {
    match body.get_mut("data") {
        Some(data) if !data.is_null() => Ok(data.take()),
        _ => bail!("{command} failed: the server's response has no data"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_quote_the_server_error_escaped_and_bounded() {
        let error = http_error(
            "cr claims",
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({"errors": {"head": ["does not match\u{1b}[2J"]}}),
        );
        assert_eq!(
            error,
            r#"cr claims failed: request rejected (HTTP 422): {"errors":{"head":["does not match\u001b[2J"]}}"#
        );
        let long = http_error("cr open", StatusCode::CONFLICT, &json!("é".repeat(3000)));
        assert!(long.ends_with('…'));
        assert!(long.len() < MAX_ERROR_BYTES + 100);
        let empty = http_error("cr ready", StatusCode::NOT_FOUND, &Value::Null);
        assert_eq!(empty, "cr ready failed: not found (HTTP 404)");
    }

    #[test]
    fn every_path_segment_is_escaped_under_the_project() {
        let Credentials::Ready(target) =
            publish::resolve(Some("http://127.0.0.1:1"), Some("sk_test"), "goodcases")
        else {
            panic!("loopback credentials resolve");
        };
        let api = Api {
            target,
            client: reqwest::Client::new(),
            command: "change show".to_owned(),
            cancel: watch::channel(None).1,
            token_env: "GOODFLOWS_API_KEY".to_owned(),
        };
        let url = api.url(&["changes", "a/b?c d", "versions", "1#2"], &[]);
        assert_eq!(
            url.path(),
            "/api/v1/projects/goodcases/changes/a%2Fb%3Fc%20d/versions/1%232"
        );
        assert_eq!(url.query(), None);
    }
}
