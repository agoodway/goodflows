//! Bounded, sequential delivery of frozen report snapshots.
//!
//! Diagnostics are structural codes and HTTP status numbers only: response
//! bodies, request records, URLs and key values are never surfaced.

use reqwest::{header, redirect, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

const MAX_ATTEMPTS: usize = 3;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MIB: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Shared by every request one publisher sends.
    pub budget: Duration,
    pub connect: Duration,
    /// Base per-request limit, extended by `request_per_mib` for each started
    /// MiB of body so a large report is not cut off on an ordinary link.
    /// Always clipped to the remaining budget.
    pub request: Duration,
    pub request_per_mib: Duration,
    pub backoffs: [Duration; MAX_ATTEMPTS - 1],
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            budget: Duration::from_secs(30),
            connect: Duration::from_secs(3),
            request: Duration::from_secs(10),
            request_per_mib: Duration::from_secs(1),
            backoffs: [Duration::from_secs(1), Duration::from_secs(2)],
        }
    }
}

impl Limits {
    pub fn with_budget(self, budget: Duration) -> Self {
        Self { budget, ..self }
    }

    /// The request limit for a body of `len` bytes, before budget clipping.
    pub fn request_for(&self, len: usize) -> Duration {
        let mib = u32::try_from(len.div_ceil(MIB)).unwrap_or(u32::MAX);
        self.request
            .saturating_add(self.request_per_mib.saturating_mul(mib))
    }
}

/// Parse `Retry-After` delta-seconds. The HTTP-date form (and anything else)
/// returns `None`, so the normal backoff schedule applies instead.
fn retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Whether a transport error came from TLS (handshake, certificate or protocol).
fn is_tls(error: &(dyn std::error::Error + 'static)) -> bool {
    if error.is::<rustls::Error>() {
        return true;
    }
    // io::Error::source() skips its payload, and hyper nests io errors, so
    // inspect each payload directly.
    if let Some(inner) = error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
    {
        if is_tls(inner) {
            return true;
        }
    }
    error.source().is_some_and(is_tls)
}

pub struct Target {
    endpoint: Url,
    key: String,
    loopback: bool,
}

pub enum Credentials {
    Ready(Target),
    Missing,
    InvalidUrl,
    InvalidProject,
}

/// Validate an origin-only base URL and build the project-scoped endpoint.
pub fn resolve(url: Option<&str>, key: Option<&str>, project: &str) -> Credentials {
    if !crate::config::valid_project_slug(project) {
        return Credentials::InvalidProject;
    }
    let (Some(url), Some(key)) = (url, key) else {
        return Credentials::Missing;
    };
    match endpoint(url, project) {
        Some((endpoint, loopback)) => Credentials::Ready(Target {
            endpoint,
            key: key.to_owned(),
            loopback,
        }),
        None => Credentials::InvalidUrl,
    }
}

fn endpoint(base: &str, project: &str) -> Option<(Url, bool)> {
    // Url::join would normalize `..`, `?` or `#` in the slug, so never trust callers.
    if !crate::config::valid_project_slug(project) {
        return None;
    }
    let url = Url::parse(base).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.cannot_be_a_base()
    {
        return None;
    }
    let host = url.host_str()?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return None,
    }
    // The slug grammar ([a-z0-9][a-z0-9_-]*) needs no escaping.
    let endpoint = url
        .join(&format!("/api/v1/projects/{project}/test-reports"))
        .ok()?;
    Some((endpoint, loopback && url.scheme() == "http"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub id: String,
    pub report_id: String,
    pub status: String,
    pub duplicate: bool,
}

#[derive(Debug)]
pub enum Outcome {
    Uploaded(Ack),
    /// Not delivered; a later replay may succeed.
    Pending {
        reason: &'static str,
        http_status: Option<u16>,
    },
    /// The server permanently refused these exact bytes (400, 409, 413, 415,
    /// 422); resending the same immutable snapshot cannot succeed.
    Rejected {
        reason: &'static str,
        http_status: u16,
    },
}

impl Outcome {
    pub fn pending(reason: &'static str, http_status: Option<u16>) -> Self {
        Self::Pending {
            reason,
            http_status,
        }
    }
}

/// Validate a 200/201 acknowledgement for exactly this report.
pub fn acknowledgement(body: &[u8], report_id: &str) -> Result<Ack, &'static str> {
    let value: Value = serde_json::from_slice(body).map_err(|_| "invalid_acknowledgement")?;
    let data = value.get("data").ok_or("invalid_acknowledgement")?;
    let id = data["id"].as_str().ok_or("invalid_acknowledgement")?;
    uuid::Uuid::parse_str(id).map_err(|_| "invalid_acknowledgement")?;
    let status = data["status"].as_str().ok_or("invalid_acknowledgement")?;
    if !matches!(status, "complete" | "incomplete") {
        return Err("invalid_acknowledgement");
    }
    let duplicate = data["duplicate"]
        .as_bool()
        .ok_or("invalid_acknowledgement")?;
    if data["report_id"].as_str() != Some(report_id) {
        return Err("mismatched_acknowledgement");
    }
    Ok(Ack {
        id: id.to_owned(),
        report_id: report_id.to_owned(),
        status: status.to_owned(),
        duplicate,
    })
}

enum Attempt {
    Done(Outcome),
    Retry {
        reason: &'static str,
        http_status: Option<u16>,
        retry_after: Option<Duration>,
    },
    Cancelled,
}

pub struct Publisher {
    client: reqwest::Client,
    target: Target,
    limits: Limits,
    deadline: Instant,
    cancel: watch::Receiver<Option<i32>>,
}

impl Publisher {
    /// The shared budget starts now and covers every report sent by this publisher.
    pub fn new(
        target: Target,
        limits: Limits,
        cancel: watch::Receiver<Option<i32>>,
    ) -> reqwest::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .connect_timeout(limits.connect)
            // Names the contract version so server logs can spot a stale publisher.
            .user_agent(format!(
                "goodflows/{} test-reports/{}",
                env!("CARGO_PKG_VERSION"),
                super::contract::CONTRACT_VERSION
            ));
        if target.loopback {
            // Never route plaintext loopback traffic (and its key) through a proxy.
            builder = builder.no_proxy();
        }
        Ok(Self {
            client: builder.build()?,
            target,
            limits,
            deadline: Instant::now() + limits.budget,
            cancel,
        })
    }

    fn cancelled(&self) -> bool {
        self.cancel.borrow().is_some()
    }

    /// Whether the shared budget is spent, so no further report should be prepared.
    pub fn exhausted(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub async fn send(&mut self, body: &[u8], report_id: &str) -> Outcome {
        let mut last = ("budget_exhausted", None);
        for attempt in 0..MAX_ATTEMPTS {
            if self.cancelled() {
                return Outcome::pending("cancelled", None);
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Outcome::pending("budget_exhausted", last.1);
            }
            match self.attempt(body, report_id, remaining).await {
                Attempt::Done(outcome) => return outcome,
                Attempt::Cancelled => return Outcome::pending("cancelled", None),
                Attempt::Retry {
                    reason,
                    http_status,
                    retry_after,
                } => {
                    last = (reason, http_status);
                    if attempt + 1 == MAX_ATTEMPTS {
                        break;
                    }
                    let wait = retry_after.unwrap_or(self.limits.backoffs[attempt]);
                    let remaining = self.deadline.saturating_duration_since(Instant::now());
                    if wait >= remaining {
                        // The next attempt could not start inside the budget, so
                        // waiting would only delay the summary; leave it pending now.
                        return Outcome::pending(reason, http_status);
                    }
                    let mut cancel = self.cancel.clone();
                    tokio::select! {
                        biased;
                        _ = cancel.wait_for(Option::is_some) => return Outcome::pending("cancelled", None),
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
            }
        }
        Outcome::pending(last.0, last.1)
    }

    async fn attempt(&self, body: &[u8], report_id: &str, remaining: Duration) -> Attempt {
        let deadline = Instant::now() + self.limits.request_for(body.len()).min(remaining);
        let request = self
            .client
            .post(self.target.endpoint.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .bearer_auth(&self.target.key)
            .body(body.to_vec());
        let exchange = async {
            let mut response = request.send().await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(retry_after);
            let mut bytes = Vec::new();
            let mut oversized = false;
            if matches!(status.as_u16(), 200 | 201) {
                while let Some(chunk) = response.chunk().await? {
                    if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        oversized = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            Ok::<_, reqwest::Error>((status, retry_after, bytes, oversized))
        };
        let mut cancel = self.cancel.clone();
        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(Option::is_some) => return Attempt::Cancelled,
            result = tokio::time::timeout_at(deadline, exchange) => result,
        };
        let (status, retry_after, bytes, oversized) = match result {
            Err(_) => {
                return Attempt::Retry {
                    reason: "timeout",
                    http_status: None,
                    retry_after: None,
                }
            }
            Ok(Err(error)) if is_tls(&error) => {
                // Certificate and handshake failures do not fix themselves on retry.
                return Attempt::Done(Outcome::pending("tls_error", None));
            }
            Ok(Err(error)) => {
                return Attempt::Retry {
                    reason: if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport_error"
                    },
                    http_status: None,
                    retry_after: None,
                }
            }
            Ok(Ok(parts)) => parts,
        };
        let code = Some(status.as_u16());
        match status {
            StatusCode::OK | StatusCode::CREATED => {
                if oversized {
                    return Attempt::Done(Outcome::pending("invalid_acknowledgement", code));
                }
                Attempt::Done(match acknowledgement(&bytes, report_id) {
                    Ok(ack) => Outcome::Uploaded(ack),
                    Err(reason) => Outcome::pending(reason, code),
                })
            }
            StatusCode::TOO_MANY_REQUESTS => Attempt::Retry {
                reason: "rate_limited",
                http_status: code,
                retry_after,
            },
            s if s.is_server_error() => Attempt::Retry {
                reason: if s == StatusCode::SERVICE_UNAVAILABLE {
                    "service_unavailable"
                } else {
                    "server_error"
                },
                http_status: code,
                retry_after,
            },
            s => Attempt::Done(match s.as_u16() {
                status @ (400 | 415) => Outcome::Rejected {
                    reason: "http_error",
                    http_status: status,
                },
                409 => Outcome::Rejected {
                    reason: "conflict",
                    http_status: 409,
                },
                413 => Outcome::Rejected {
                    reason: "request_too_large",
                    http_status: 413,
                },
                422 => Outcome::Rejected {
                    reason: "rejected",
                    http_status: 422,
                },
                status => Outcome::pending(
                    match status {
                        300..=399 => "redirect",
                        401 => "unauthorized",
                        403 => "forbidden",
                        404 => "project_not_found",
                        _ => "http_error",
                    },
                    code,
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn origin_rules() {
        let ok = |url: &str| endpoint(url, "goodcases").map(|(u, _)| u.to_string());
        assert_eq!(
            ok("https://flows.example.com").as_deref(),
            Some("https://flows.example.com/api/v1/projects/goodcases/test-reports")
        );
        assert!(ok("https://flows.example.com/").is_some());
        assert!(ok("https://flows.example.com:8443").is_some());
        for loopback in [
            "http://localhost:4000",
            "http://127.0.0.1:4000",
            "http://127.9.9.9",
            "http://[::1]:4000",
        ] {
            assert!(ok(loopback).is_some(), "{loopback}");
        }
        for invalid in [
            "http://flows.example.com",
            "http://10.0.0.1",
            "https://flows.example.com/api",
            "https://user:pass@flows.example.com",
            "https://user@flows.example.com",
            "https://flows.example.com/?q=1",
            "https://flows.example.com/?",
            "https://flows.example.com/#frag",
            "ftp://flows.example.com",
            "flows.example.com",
            "",
        ] {
            assert!(ok(invalid).is_none(), "{invalid}");
        }
        assert!(matches!(
            resolve(None, Some("k"), "p"),
            Credentials::Missing
        ));
        assert!(matches!(
            resolve(Some("https://x.example"), None, "p"),
            Credentials::Missing
        ));
        assert!(matches!(
            resolve(Some("http://x.example"), Some("k"), "p"),
            Credentials::InvalidUrl
        ));
    }

    #[test]
    fn acknowledgement_rules() {
        let id = "0b9c2b8e-8f1e-4c55-9d43-6a0e1c1f2a3b";
        let ack = |report: &str, status: &str, duplicate: &str| {
            format!(
                r#"{{"data":{{"id":"{id}","report_id":"{report}","status":"{status}","duplicate":{duplicate}}}}}"#
            )
        };
        assert_eq!(
            acknowledgement(ack("r", "complete", "false").as_bytes(), "r").unwrap(),
            Ack {
                id: id.into(),
                report_id: "r".into(),
                status: "complete".into(),
                duplicate: false
            }
        );
        assert!(
            acknowledgement(ack("r", "incomplete", "true").as_bytes(), "r")
                .unwrap()
                .duplicate
        );
        assert_eq!(
            acknowledgement(ack("other", "complete", "false").as_bytes(), "r").unwrap_err(),
            "mismatched_acknowledgement"
        );
        for bad in [
            ack("r", "done", "false"),
            ack("r", "complete", "\"no\""),
            ack("r", "complete", "false").replace(id, "not-a-uuid"),
            "{}".into(),
            "not json".into(),
            r#"{"data":null}"#.into(),
        ] {
            assert_eq!(
                acknowledgement(bad.as_bytes(), "r").unwrap_err(),
                "invalid_acknowledgement",
                "{bad}"
            );
        }
    }

    /// Scripted HTTP/1.1 responses, one per connection.
    #[derive(Clone)]
    enum Reply {
        Status(u16, &'static str, String),
        Hang,
        Close,
    }

    struct Mock {
        url: String,
        requests: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    fn mock(replies: Vec<Reply>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        std::thread::spawn(move || {
            let mut replies = replies.into_iter();
            let mut last = None;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    headers.push_str(&h);
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock()
                    .unwrap()
                    .push((line, headers, String::from_utf8(body).unwrap()));
                let reply = replies.next().or(last.clone()).unwrap_or(Reply::Close);
                last = Some(reply.clone());
                match reply {
                    Reply::Status(code, extra, body) => {
                        let _ = write!(stream, "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
                    }
                    Reply::Hang => {
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(5));
                            drop(stream);
                        });
                    }
                    Reply::Close => {}
                }
            }
        });
        Mock { url, requests }
    }

    fn fast() -> Limits {
        Limits {
            budget: Duration::from_secs(5),
            connect: Duration::from_millis(500),
            request: Duration::from_millis(400),
            request_per_mib: Duration::ZERO,
            backoffs: [Duration::from_millis(20), Duration::from_millis(40)],
        }
    }

    fn ok_body(report_id: &str) -> String {
        format!(
            r#"{{"data":{{"id":"0b9c2b8e-8f1e-4c55-9d43-6a0e1c1f2a3b","report_id":"{report_id}","status":"complete","duplicate":false}}}}"#
        )
    }

    async fn send(mock: &Mock, limits: Limits) -> (Outcome, Duration) {
        let (_tx, rx) = watch::channel(None);
        let Credentials::Ready(target) = resolve(Some(&mock.url), Some("secret-key"), "goodcases")
        else {
            panic!("loopback url must be accepted")
        };
        let mut publisher = Publisher::new(target, limits, rx).unwrap();
        let start = std::time::Instant::now();
        let outcome = publisher.send(br#"{"report":1}"#, "r").await;
        (outcome, start.elapsed())
    }

    fn reason(outcome: &Outcome) -> (&'static str, Option<u16>) {
        match outcome {
            Outcome::Pending {
                reason,
                http_status,
            } => (reason, *http_status),
            Outcome::Rejected {
                reason,
                http_status,
            } => (reason, Some(*http_status)),
            Outcome::Uploaded(_) => ("uploaded", None),
        }
    }

    #[tokio::test]
    async fn request_shape_and_success() {
        let mock = mock(vec![Reply::Status(201, "", ok_body("r"))]);
        let (outcome, _) = send(&mock, fast()).await;
        assert!(matches!(outcome, Outcome::Uploaded(ref ack) if !ack.duplicate));
        let requests = mock.requests.lock().unwrap();
        let (line, headers, body) = &requests[0];
        assert_eq!(
            line.trim(),
            "POST /api/v1/projects/goodcases/test-reports HTTP/1.1"
        );
        assert!(
            headers.contains("authorization: Bearer secret-key")
                || headers.contains("Authorization: Bearer secret-key"),
            "{headers}"
        );
        assert!(headers
            .to_ascii_lowercase()
            .contains("content-type: application/json"));
        assert_eq!(body, r#"{"report":1}"#);
    }

    #[tokio::test]
    async fn retries_transient_failures_with_backoff() {
        let mock = mock(vec![
            Reply::Close,
            Reply::Status(503, "Retry-After: 0\r\n", String::new()),
            Reply::Status(200, "", ok_body("r")),
        ]);
        let (outcome, _) = send(&mock, fast()).await;
        assert!(matches!(outcome, Outcome::Uploaded(_)), "{outcome:?}");
        assert_eq!(mock.requests.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn exhausts_three_attempts() {
        for (reply, expected) in [
            (
                Reply::Status(500, "", String::new()),
                ("server_error", Some(500)),
            ),
            (
                Reply::Status(429, "", String::new()),
                ("rate_limited", Some(429)),
            ),
            (
                Reply::Status(503, "", String::new()),
                ("service_unavailable", Some(503)),
            ),
            (Reply::Close, ("transport_error", None)),
            (Reply::Hang, ("timeout", None)),
        ] {
            let mock = mock(vec![reply]);
            let (outcome, _) = send(&mock, fast()).await;
            assert_eq!(reason(&outcome), expected);
            assert_eq!(mock.requests.lock().unwrap().len(), 3);
        }
    }

    #[tokio::test]
    async fn terminal_responses_are_not_retried() {
        for (code, expected) in [
            (301, "redirect"),
            (307, "redirect"),
            (400, "http_error"),
            (401, "unauthorized"),
            (403, "forbidden"),
            (404, "project_not_found"),
            (406, "http_error"),
            (409, "conflict"),
            (413, "request_too_large"),
            (415, "http_error"),
            (422, "rejected"),
        ] {
            let mock = mock(vec![Reply::Status(
                code,
                "Location: https://elsewhere.example/steal\r\n",
                r#"{"errors":{"detail":"sensitive-body"}}"#.into(),
            )]);
            let (outcome, _) = send(&mock, fast()).await;
            assert_eq!(reason(&outcome), (expected, Some(code)));
            assert_eq!(mock.requests.lock().unwrap().len(), 1, "{code}");
            assert!(!format!("{outcome:?}").contains("sensitive"));
            // Only content rejections are terminal; auth, project and redirects stay replayable.
            assert_eq!(
                matches!(outcome, Outcome::Rejected { .. }),
                matches!(code, 400 | 409 | 413 | 415 | 422),
                "{code}"
            );
        }
    }

    #[tokio::test]
    async fn malformed_or_mismatched_success_stays_pending() {
        for (body, expected) in [
            (ok_body("other"), "mismatched_acknowledgement"),
            ("{\"data\":{}}".to_owned(), "invalid_acknowledgement"),
            (
                "x".repeat(MAX_RESPONSE_BYTES + 1),
                "invalid_acknowledgement",
            ),
        ] {
            let mock = mock(vec![Reply::Status(200, "", body)]);
            let (outcome, _) = send(&mock, fast()).await;
            assert_eq!(reason(&outcome).0, expected);
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn retry_after_beyond_budget_stops_immediately() {
        let mock = mock(vec![Reply::Status(
            503,
            "Retry-After: 120\r\n",
            String::new(),
        )]);
        let (outcome, elapsed) = send(&mock, fast()).await;
        assert_eq!(reason(&outcome), ("service_unavailable", Some(503)));
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn default_limits_match_the_documented_contract() {
        let limits = Limits::default();
        assert_eq!(
            limits,
            Limits {
                budget: Duration::from_secs(30),
                connect: Duration::from_secs(3),
                request: Duration::from_secs(10),
                request_per_mib: Duration::from_secs(1),
                backoffs: [Duration::from_secs(1), Duration::from_secs(2)],
            }
        );
        // The per-request limit grows with the body: 10 s plus 1 s per started MiB.
        assert_eq!(limits.request_for(0), Duration::from_secs(10));
        assert_eq!(limits.request_for(1), Duration::from_secs(11));
        assert_eq!(limits.request_for(MIB), Duration::from_secs(11));
        assert_eq!(limits.request_for(32 * MIB), Duration::from_secs(42));
        assert_eq!(
            limits.with_budget(Duration::from_secs(300)).budget,
            Duration::from_secs(300)
        );
    }

    #[test]
    fn retry_after_accepts_delta_seconds_only() {
        assert_eq!(retry_after("0"), Some(Duration::ZERO));
        assert_eq!(retry_after(" 7 "), Some(Duration::from_secs(7)));
        for other in ["Wed, 21 Oct 2015 07:28:00 GMT", "-1", "1.5", ""] {
            assert_eq!(retry_after(other), None, "{other}");
        }
    }

    #[tokio::test]
    async fn retry_after_on_429_is_honored() {
        let mock = mock(vec![
            Reply::Status(429, "Retry-After: 1\r\n", String::new()),
            Reply::Status(201, "", ok_body("r")),
        ]);
        let (outcome, elapsed) = send(&mock, fast()).await;
        assert!(matches!(outcome, Outcome::Uploaded(_)), "{outcome:?}");
        // The 1 s Retry-After replaced the 20 ms backoff.
        assert!(elapsed >= Duration::from_millis(950), "{elapsed:?}");
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn http_date_retry_after_falls_back_to_backoff() {
        let mock = mock(vec![
            Reply::Status(
                503,
                "Retry-After: Wed, 21 Oct 2099 07:28:00 GMT\r\n",
                String::new(),
            ),
            Reply::Status(201, "", ok_body("r")),
        ]);
        let (outcome, elapsed) = send(&mock, fast()).await;
        assert!(matches!(outcome, Outcome::Uploaded(_)), "{outcome:?}");
        // A far-future date neither stops publishing nor waits: the 20 ms backoff applies.
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn retry_after_on_429_beyond_budget_stops_immediately() {
        let mock = mock(vec![Reply::Status(
            429,
            "Retry-After: 120\r\n",
            String::new(),
        )]);
        let (outcome, elapsed) = send(&mock, fast()).await;
        assert_eq!(reason(&outcome), ("rate_limited", Some(429)));
        assert!(elapsed < Duration::from_secs(2));
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn tls_failures_are_reported_and_not_retried() {
        // A plaintext server behind an https:// URL fails the TLS handshake.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let connections = Arc::new(Mutex::new(0));
        let count = connections.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                *count.lock().unwrap() += 1;
                let mut hello = [0; 512];
                let _ = stream.read(&mut hello);
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let (_tx, rx) = watch::channel(None);
        let Credentials::Ready(target) = resolve(Some(&url), Some("k"), "p") else {
            panic!("https loopback must be accepted")
        };
        let mut publisher = Publisher::new(target, fast(), rx).unwrap();
        let outcome = publisher.send(b"{}", "r").await;
        assert_eq!(reason(&outcome), ("tls_error", None));
        assert_eq!(*connections.lock().unwrap(), 1);
    }

    #[test]
    fn endpoint_validates_the_project_slug_itself() {
        for project in ["../admin", "a?x=1", "a#f", "A", "-a", "_a", "", "a/b"] {
            assert!(
                endpoint("https://flows.example.com", project).is_none(),
                "{project}"
            );
            assert!(matches!(
                resolve(Some("https://flows.example.com"), Some("k"), project),
                Credentials::InvalidProject
            ));
        }
    }

    #[tokio::test]
    async fn shared_budget_is_invocation_wide() {
        let mock = mock(vec![Reply::Hang]);
        let limits = Limits {
            budget: Duration::from_millis(600),
            ..fast()
        };
        let (_tx, rx) = watch::channel(None);
        let Credentials::Ready(target) = resolve(Some(&mock.url), Some("k"), "p") else {
            panic!()
        };
        let mut publisher = Publisher::new(target, limits, rx).unwrap();
        let start = std::time::Instant::now();
        let first = publisher.send(b"{}", "a").await;
        let second = publisher.send(b"{}", "b").await;
        assert!(start.elapsed() < Duration::from_millis(1500));
        assert_eq!(reason(&first).0, "timeout");
        assert_eq!(reason(&second).0, "budget_exhausted");
    }

    #[tokio::test]
    async fn cancellation_interrupts_requests_and_prevents_new_ones() {
        let mock = mock(vec![Reply::Hang]);
        let (tx, rx) = watch::channel(None);
        let Credentials::Ready(target) = resolve(Some(&mock.url), Some("k"), "p") else {
            panic!()
        };
        let limits = Limits {
            request: Duration::from_secs(10),
            ..fast()
        };
        let mut publisher = Publisher::new(target, limits, rx).unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tx.send_replace(Some(15));
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let start = std::time::Instant::now();
        assert_eq!(reason(&publisher.send(b"{}", "a").await).0, "cancelled");
        assert_eq!(reason(&publisher.send(b"{}", "b").await).0, "cancelled");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }
}
