//! `goodflows history list` and `history show`: read run records back from
//! the reporting server.
//!
//! Every filter is checked with the server's rule before any request. The
//! single GET is not retried. Responses are untrusted: they are bounded,
//! checked for the documented `data` shape, and a failure is reported by HTTP
//! status only, never by body.

use super::publish::{self, Credentials, DocumentKind};
use super::run_contract::{valid_label, valid_name};
use crate::config::Reporting;
use crate::output::Output;
use anyhow::{bail, Result};
use reqwest::{header, StatusCode, Url};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::watch;

/// Largest response body read; a longer one fails the command.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// The whole exchange, including reading the body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CURSOR_BYTES: usize = 512;

/// `history list` filters, each checked with the server's rule while parsing.
#[derive(clap::Args, Debug, Default)]
pub struct Filters {
    /// Only runs of this workflow
    #[arg(long, value_name = "NAME", value_parser = name)]
    pub workflow: Option<String>,
    /// Only runs that selected this environment profile
    #[arg(long, value_name = "NAME", value_parser = name)]
    pub environment: Option<String>,
    /// Only runs with this outcome
    #[arg(long, value_parser = ["passed", "failed", "cancelled"])]
    pub outcome: Option<String>,
    /// Only runs of this commit (40 or 64 lowercase hex characters)
    #[arg(long, value_name = "SHA", value_parser = commit)]
    pub commit: Option<String>,
    /// Only runs on this branch
    #[arg(long, value_name = "NAME", value_parser = branch)]
    pub branch: Option<String>,
    /// Only runs started by this CI event, such as push
    #[arg(long, value_name = "NAME", value_parser = name)]
    pub event: Option<String>,
    /// Only runs of this job graph (64 lowercase hex characters)
    #[arg(long, value_name = "HASH", value_parser = graph_sha256)]
    pub graph_sha256: Option<String>,
    /// Runs per page, 1-200 (server default 50)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u16).range(1..=200))]
    pub limit: Option<u16>,
    /// The next page, as printed after the previous one
    #[arg(long, value_parser = cursor)]
    pub cursor: Option<String>,
}

impl Filters {
    /// Query parameters in a fixed order, only for the filters that are set.
    fn query(&self) -> Vec<(&'static str, String)> {
        [
            ("workflow", self.workflow.clone()),
            ("environment", self.environment.clone()),
            ("outcome", self.outcome.clone()),
            ("commit", self.commit.clone()),
            ("branch", self.branch.clone()),
            ("event", self.event.clone()),
            ("graph_sha256", self.graph_sha256.clone()),
            ("limit", self.limit.map(|n| n.to_string())),
            ("cursor", self.cursor.clone()),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|v| (key, v)))
        .collect()
    }
}

fn checked(value: &str, ok: bool, rule: &str) -> Result<String, String> {
    if ok {
        Ok(value.to_owned())
    } else {
        Err(rule.to_owned())
    }
}

fn lowercase_hex(value: &str, lengths: &[usize]) -> bool {
    lengths.contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The server's name rule, for `--workflow`, `--environment` and `--event`.
pub fn name(value: &str) -> Result<String, String> {
    checked(
        value,
        valid_name(value),
        "must be 1-64 bytes, contain no control characters and not be only whitespace",
    )
}

pub fn commit(value: &str) -> Result<String, String> {
    checked(
        value,
        lowercase_hex(value, &[40, 64]),
        "must be a full commit hash: 40 or 64 lowercase hex characters",
    )
}

pub fn branch(value: &str) -> Result<String, String> {
    checked(
        value,
        valid_label(value, 256),
        "must be 1-256 bytes without control characters",
    )
}

pub fn graph_sha256(value: &str) -> Result<String, String> {
    checked(
        value,
        lowercase_hex(value, &[64]),
        "must be 64 lowercase hex characters",
    )
}

/// The server's `next_cursor` grammar: unpadded base64url, at most 512 bytes.
pub fn cursor(value: &str) -> Result<String, String> {
    let ok = !value.is_empty()
        && value.len() <= MAX_CURSOR_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    checked(
        value,
        ok,
        "must be the next_cursor printed after the previous page",
    )
}

/// A lowercase canonical UUID; the server does not look up other spellings.
pub fn run_id(value: &str) -> Result<String, String> {
    checked(
        value,
        crate::fsutil::canonical_uuid(value),
        "must be a lowercase canonical UUID",
    )
}

/// `history list`: one `history_runs` event for one page of run summaries.
pub async fn list(
    reporting: Option<&Reporting>,
    filters: &Filters,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let target = target(reporting)?;
    let mut url = target.endpoint(DocumentKind::WorkflowRun);
    let query = filters.query();
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let body = get(&target, url, "list", cancel).await?;
    let Some((runs, next_cursor)) = page(&body) else {
        bail!("history list failed: the server's response is not a list of runs");
    };
    output
        .emit(json!({"type":"history_runs", "runs":runs, "next_cursor":next_cursor}))
        .await?;
    Ok(0)
}

/// `history show`: one `history_run` event with the server's `data` unchanged.
pub async fn show(
    reporting: Option<&Reporting>,
    run_id: &str,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let target = target(reporting)?;
    let mut url = target.endpoint(DocumentKind::WorkflowRun);
    url.path_segments_mut()
        .expect("an https or loopback http URL has path segments")
        .push(run_id);
    let mut body = get(&target, url, "show", cancel).await?;
    if !stored_run(&body, run_id) {
        bail!("history show failed: the server's response is not a stored run");
    }
    output
        .emit(json!({"type":"history_run", "run":body["data"].take()}))
        .await?;
    Ok(0)
}

/// The publisher's target, resolved from the same `[reporting]` variables.
fn target(reporting: Option<&Reporting>) -> Result<publish::Target> {
    let Some(reporting) = reporting else {
        bail!("history commands require a top-level [reporting] block");
    };
    let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let key = read(&reporting.token_env);
    if key.is_some() {
        super::protect_process();
    }
    match publish::resolve(
        read(&reporting.url_env).as_deref(),
        key.as_deref(),
        &reporting.project,
    ) {
        Credentials::Ready(target) => Ok(target),
        Credentials::Missing => bail!(
            "history commands require {} and {} to be set (reporting url_env and token_env)",
            reporting.url_env,
            reporting.token_env
        ),
        Credentials::InvalidUrl => bail!(
            "{} must be an origin-only https URL (http only for loopback)",
            reporting.url_env
        ),
        Credentials::InvalidProject => bail!("reporting project is not a valid slug"),
    }
}

/// One GET without retries. Any failure names the HTTP status or a
/// structural cause only: never the body, the URL or the key.
async fn get(
    target: &publish::Target,
    url: Url,
    command: &str,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<Value> {
    let Ok(client) = target.client(CONNECT_TIMEOUT) else {
        bail!("history {command} failed: could not create an HTTP client");
    };
    let exchange = async {
        let mut response = client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .bearer_auth(target.key())
            .send()
            .await?;
        let status = response.status();
        let mut bytes = Vec::new();
        if status == StatusCode::OK {
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    return Ok((status, None));
                }
                bytes.extend_from_slice(&chunk);
            }
        }
        Ok::<_, reqwest::Error>((status, Some(bytes)))
    };
    let result = tokio::select! {
        biased;
        _ = cancel.wait_for(Option::is_some) => bail!("history {command} interrupted"),
        result = tokio::time::timeout(REQUEST_TIMEOUT, exchange) => result,
    };
    let (status, bytes) = match result {
        Err(_) => bail!("history {command} failed: timed out"),
        Ok(Err(error)) if error.is_timeout() => bail!("history {command} failed: timed out"),
        Ok(Err(_)) => bail!("history {command} failed: could not reach the server"),
        Ok(Ok(parts)) => parts,
    };
    if status != StatusCode::OK {
        let code = status.as_u16();
        let reason = match code {
            404 if command == "show" => "run not found",
            404 => "project not found",
            300..=399 => "redirect not followed",
            401 => "unauthorized",
            403 => "forbidden",
            422 => "request rejected",
            500..=599 => "server error",
            _ => "unexpected response",
        };
        bail!("history {command} failed: {reason} (HTTP {code})");
    }
    let Some(bytes) = bytes else {
        bail!("history {command} failed: the response exceeds 16 MiB");
    };
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("history {command} failed: the response is not JSON"))
}

fn is_text(value: &Value, key: &str) -> bool {
    value[key].is_string()
}

/// Absent, null, or a string.
fn optional_text(value: &Value, key: &str) -> bool {
    value[key].is_null() || value[key].is_string()
}

/// Absent, null, or an object with a string `commit` and an optional boolean
/// `dirty`.
fn source(value: &Value) -> bool {
    value.is_null()
        || (value.is_object()
            && is_text(value, "commit")
            && (value["dirty"].is_null() || value["dirty"].is_boolean()))
}

/// The `data` and `next_cursor` of a list response, when it has the
/// documented shape.
fn page(body: &Value) -> Option<(&Value, &Value)> {
    let runs = &body["data"];
    let next_cursor = &body["next_cursor"];
    let summary = |run: &Value| {
        run.is_object()
            && ["run_id", "workflow", "outcome", "started_at"]
                .iter()
                .all(|key| is_text(run, key))
            && run["duration_ms"].is_u64()
            && optional_text(run, "environment")
            && source(&run["source"])
    };
    (runs.as_array()?.iter().all(summary) && optional_text(body, "next_cursor"))
        .then_some((runs, next_cursor))
}

/// Whether a show response has the documented shape for this run.
fn stored_run(body: &Value, run_id: &str) -> bool {
    let data = &body["data"];
    let document = &data["document"];
    let result = |r: &Value| r.is_object() && is_text(r, "name") && is_text(r, "status");
    let report = |r: &Value| r.is_object() && is_text(r, "job") && is_text(r, "report_id");
    data["run_id"] == run_id
        && document.is_object()
        && ["workflow", "outcome", "started_at"]
            .iter()
            .all(|key| is_text(document, key))
        && document["duration_ms"].is_u64()
        && source(&document["source"])
        && document["results"]
            .as_array()
            .is_some_and(|r| r.iter().all(result))
        && data["test_reports"]
            .as_array()
            .is_some_and(|r| r.iter().all(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_rules_match_the_server() {
        assert!(name("ci").is_ok());
        for bad in ["", " ", "a\u{1b}b", &"n".repeat(65)] {
            assert!(name(bad).is_err(), "{bad:?}");
        }
        assert!(name(&"é".repeat(32)).is_ok());
        assert!(commit(&"a".repeat(40)).is_ok());
        assert!(commit(&"0".repeat(64)).is_ok());
        for bad in ["HEAD", &"A".repeat(40), &"a".repeat(41), &"g".repeat(40)] {
            assert!(commit(bad).is_err(), "{bad}");
        }
        assert!(graph_sha256(&"f".repeat(64)).is_ok());
        assert!(graph_sha256(&"f".repeat(40)).is_err());
        assert!(branch(&"b".repeat(256)).is_ok());
        assert!(branch(&"b".repeat(257)).is_err());
        assert!(branch("a\nb").is_err());
        assert!(cursor("MjAyNi0xMS0wOVQxMzoyNTozMVp8ZmI3ZA").is_ok());
        assert!(cursor(&"A".repeat(512)).is_ok());
        for bad in ["", "a=", "a+b", "a/b", &"A".repeat(513)] {
            assert!(cursor(bad).is_err(), "{bad}");
        }
        assert!(run_id("fb7d2711-6b67-4f5a-9104-492032a9e1e5").is_ok());
        for bad in [
            "FB7D2711-6B67-4F5A-9104-492032A9E1E5",
            "fb7d27116b674f5a9104492032a9e1e5",
            "../x",
        ] {
            assert!(run_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn query_keeps_the_documented_order_and_skips_unset_filters() {
        let filters = Filters {
            workflow: Some("ci".into()),
            branch: Some("main".into()),
            event: Some("push".into()),
            limit: Some(2),
            ..Filters::default()
        };
        let query: Vec<_> = filters.query();
        assert_eq!(
            query,
            [
                ("workflow", "ci".to_owned()),
                ("branch", "main".to_owned()),
                ("event", "push".to_owned()),
                ("limit", "2".to_owned()),
            ]
        );
        assert!(Filters::default().query().is_empty());
    }

    #[test]
    fn response_shapes_are_checked() {
        let run = json!({"run_id":"r", "workflow":"ci", "outcome":"passed",
            "started_at":"2026-11-09T13:25:31Z", "duration_ms":1, "environment":null,
            "source":{"commit":"c", "dirty":true}});
        assert!(page(&json!({"data":[run], "next_cursor":null})).is_some());
        assert!(page(&json!({"data":[], "next_cursor":"abc"})).is_some());
        for bad in [
            json!([]),
            json!({"data":{}, "next_cursor":null}),
            json!({"data":[], "next_cursor":1}),
            json!({"data":[1], "next_cursor":null}),
            json!({"data":[{"run_id":"r"}], "next_cursor":null}),
        ] {
            assert!(page(&bad).is_none(), "{bad}");
        }
        let mut bad = run.clone();
        bad["source"] = json!({"commit":"c", "dirty":"yes"});
        assert!(page(&json!({"data":[bad], "next_cursor":null})).is_none());

        let id = "fb7d2711-6b67-4f5a-9104-492032a9e1e5";
        let shown = json!({"data":{"id":"x", "run_id":id, "inserted_at":"t", "test_reports":[],
            "document":{"workflow":"ci", "outcome":"passed", "started_at":"t", "duration_ms":1,
                "results":[{"name":"a", "status":"passed"}]}}});
        assert!(stored_run(&shown, id));
        assert!(!stored_run(&shown, "0e54fb55-a115-4ac7-80fe-8fde26e76afb"));
        for (pointer, value) in [
            ("/data/document/results/0/name", json!(1)),
            ("/data/test_reports", json!({})),
            ("/data/document/duration_ms", json!("1")),
            ("/data/document", json!(null)),
        ] {
            let mut bad = shown.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(!stored_run(&bad, id), "{pointer}");
        }
    }
}
