//! nextest's JUnit report to `test-timings/1` records.
//!
//! Every test binary (`<testsuite>`) is a group and every attempt of a test
//! (`<testcase>` and its retry elements) a `test_started`/`test_finished`
//! pair. Records are written on one timeline, sorted by when each event
//! happened, so groups that ran in parallel interleave as they would in a
//! live stream. Failure messages and captured output are never read.

use std::collections::{BTreeMap, HashMap};

use roxmltree::Node;
use serde_json::{json, Map, Value};

use crate::time;

const FORMAT: &str = "test-timings/1";
const MAX_TEXT_BYTES: usize = 4096;
const MAX_META_STRING_BYTES: usize = 256;
const MAX_RUNNER_VERSION_BYTES: usize = 128;
const MAX_PATH_ENTRIES: usize = 32;

/// What the report needs besides the JUnit file.
pub struct Run {
    pub suite_id: String,
    pub report_id: String,
    pub partition: Option<String>,
    pub partition_count: u64,
    pub runner_version: Option<String>,
    /// Source file of each test binary, by nextest binary id.
    pub files: HashMap<String, String>,
    /// When the wrapped command started, in Unix microseconds.
    pub started_us: i64,
    /// The command's exit code, or `None` when a signal ended it.
    pub exit_code: Option<i32>,
    pub interrupted: bool,
}

/// The records of a run whose command wrote no JUnit report: no test is
/// known, so the run is only recorded as aborted.
pub fn aborted(run: &Run) -> Vec<Value> {
    let mut records = vec![suite_started(run, run.started_us, None)];
    if run.interrupted {
        records.push(json!({"event": "interrupted"}));
    }
    records.push(suite_finished(run, true, &Counts::new(), None));
    stamp(run, records.into_iter().map(|r| (0, r)).collect())
}

pub fn records(run: &Run, xml: &str) -> Result<Vec<Value>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("invalid JUnit XML: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name("testsuites") {
        return Err(format!(
            "JUnit root is <{}>, not <testsuites>",
            root.tag_name().name()
        ));
    }
    let start = root
        .attribute("timestamp")
        .and_then(time::parse_us)
        .unwrap_or(run.started_us);
    let offset = |at: Option<i64>| at.map(|at| (at - start).max(0) as u64);

    // (offset, rank, record): a stable sort by the first two puts every
    // started event before its finished event at the same instant.
    let mut events: Vec<(u64, u8, Value)> = Vec::new();
    let mut suite_counts = Counts::new();
    let mut next_id = 1u64;
    for suite in children(root, &["testsuite"]) {
        let binary = suite.attribute("name").unwrap_or("");
        let group = text(binary);
        let file = run.files.get(binary).map(|f| text(f));
        let cases: Vec<Case> = children(suite, &["testcase"]).map(Case::read).collect();

        let attempts = || cases.iter().flat_map(|c| &c.attempts);
        let group_start = attempts().filter_map(|a| offset(a.at)).min();
        let group_end = attempts()
            .filter_map(|a| Some(offset(a.at)? + a.duration_us.unwrap_or(0)))
            .max();
        let group_start_us = group_start.unwrap_or(0);
        let group_id = next_id;
        next_id += 1;
        let group_core = json!({
            "group": group, "file": file, "execution_id": group_id, "started_offset_us": group_start_us,
        });
        events.push((
            group_start_us,
            0,
            with(&group_core, json!({"event": "group_started"})),
        ));

        let mut counts = Counts::new();
        for case in &cases {
            let full = case.name.as_str();
            let (path, name) = match full.rsplit_once("::") {
                Some((path, name)) => (
                    path.split("::").take(MAX_PATH_ENTRIES).map(text).collect(),
                    name,
                ),
                None => (Vec::new(), full),
            };
            let last = case.attempts.len();
            for (n, attempt) in case.attempts.iter().enumerate() {
                let started = offset(attempt.at).unwrap_or(group_start_us);
                let core = json!({
                    "group": group, "path": path, "name": text(name), "file": Value::Null,
                    "line": Value::Null, "execution_id": next_id, "started_offset_us": started,
                });
                next_id += 1;
                let mut meta = Map::new();
                if n > 0 {
                    meta.insert("invocations".into(), json!(n + 1));
                }
                if n + 1 == last && case.flaky {
                    meta.insert("flaky".into(), json!(true));
                }
                if let Some(kind) = &attempt.failure_type {
                    meta.insert("failure_type".into(), json!(meta_text(kind)));
                }
                if let Some(reason) = &attempt.skip_reason {
                    meta.insert("skip_reason".into(), json!(meta_text(reason)));
                }
                let mut finished = json!({
                    "event": "test_finished", "duration_us": attempt.duration_us, "outcome": attempt.outcome,
                });
                if !meta.is_empty() {
                    finished["meta"] = Value::Object(meta);
                }
                events.push((started, 1, with(&core, json!({"event": "test_started"}))));
                let ended = started + attempt.duration_us.unwrap_or(0);
                events.push((ended, 2, with(&core, finished)));
            }
            let outcome = case.attempts.last().map_or("skipped", |a| a.outcome);
            *counts.entry(outcome).or_default() += 1;
            *suite_counts.entry(outcome).or_default() += 1;
        }

        let outcome = if counts.contains_key("failed") {
            "failed"
        } else if counts.contains_key("errored") {
            "errored"
        } else if !counts.is_empty() && counts.keys().all(|k| *k == "skipped") {
            "skipped"
        } else {
            "passed"
        };
        let group_end_us = group_end.unwrap_or(group_start_us);
        let duration = group_start.map(|start| group_end_us - start);
        let finished = json!({
            "event": "group_finished", "duration_us": duration, "outcome": outcome, "counts": counts,
        });
        events.push((group_end_us, 3, with(&group_core, finished)));
    }
    events.sort_by_key(|(at, rank, _)| (*at, *rank));

    let duration = root.attribute("time").and_then(time::seconds_us);
    let mut timeline = vec![(0, suite_started(run, start, root.attribute("uuid")))];
    let end = events
        .iter()
        .map(|(at, _, _)| *at)
        .max()
        .unwrap_or(0)
        .max(duration.unwrap_or(0));
    timeline.extend(events.into_iter().map(|(at, _, record)| (at, record)));
    if run.interrupted {
        timeline.push((end, json!({"event": "interrupted"})));
    }
    timeline.push((
        end,
        suite_finished(run, run.interrupted, &suite_counts, duration),
    ));
    Ok(stamp(run, timeline))
}

type Counts = BTreeMap<&'static str, u64>;

struct Attempt {
    /// Unix microseconds; `None` for a skipped test, which never started.
    at: Option<i64>,
    duration_us: Option<u64>,
    outcome: &'static str,
    failure_type: Option<String>,
    skip_reason: Option<String>,
}

/// One `<testcase>`: its attempts, oldest first. Its own `timestamp`/`time`
/// is the first attempt of a test that failed every time (later attempts are
/// `rerun*` elements) but the last attempt of a flaky test (earlier attempts
/// are `flaky*` elements).
struct Case {
    name: String,
    attempts: Vec<Attempt>,
    flaky: bool,
}

impl Case {
    fn read(case: Node) -> Case {
        let name = case.attribute("name").unwrap_or("").to_owned();
        let attempt = |node: Node, outcome: &'static str, timed: Node| Attempt {
            at: timed.attribute("timestamp").and_then(time::parse_us),
            duration_us: timed.attribute("time").and_then(time::seconds_us),
            outcome,
            failure_type: node.attribute("type").map(str::to_owned),
            skip_reason: None,
        };
        let outcome = |node: Node| {
            if node.tag_name().name().ends_with("rror") {
                "errored"
            } else {
                "failed"
            }
        };
        if let Some(skipped) = children(case, &["skipped"]).next() {
            let attempt = Attempt {
                at: None,
                duration_us: None,
                outcome: "skipped",
                failure_type: None,
                skip_reason: skipped.attribute("message").map(str::to_owned),
            };
            return Case {
                name,
                attempts: vec![attempt],
                flaky: false,
            };
        }
        let own = match children(case, &["failure", "error"]).next() {
            Some(result) => attempt(result, outcome(result), case),
            None => Attempt {
                failure_type: None,
                ..attempt(case, "passed", case)
            },
        };
        let earlier: Vec<Attempt> = children(case, &["flakyFailure", "flakyError"])
            .map(|n| attempt(n, outcome(n), n))
            .collect();
        if !earlier.is_empty() {
            let mut attempts = earlier;
            attempts.push(own);
            return Case {
                name,
                attempts,
                flaky: true,
            };
        }
        let mut attempts = vec![own];
        attempts.extend(
            children(case, &["rerunFailure", "rerunError"]).map(|n| attempt(n, outcome(n), n)),
        );
        Case {
            name,
            attempts,
            flaky: false,
        }
    }
}

fn children<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    tags: &'a [&'a str],
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |n| tags.iter().any(|t| n.has_tag_name(*t)))
}

fn suite_started(run: &Run, start_us: i64, uuid: Option<&str>) -> Value {
    let mut meta = json!({
        "reporter": env!("CARGO_PKG_NAME"),
        "reporter_version": env!("CARGO_PKG_VERSION"),
        "duration_resolution_us": 1000,
    });
    if let Some(uuid) = uuid {
        meta["run_uuid"] = json!(meta_text(uuid));
    }
    json!({
        "event": "suite_started",
        "format": FORMAT,
        "started_at": time::format_us(start_us),
        "runner": {
            "name": "cargo-nextest",
            "version": run.runner_version.as_deref().map(|v| truncate(v, MAX_RUNNER_VERSION_BYTES)),
        },
        "partition": run.partition,
        "partition_count": run.partition_count,
        "seed": Value::Null,
        "filters": Value::Null,
        "meta": meta,
    })
}

fn suite_finished(run: &Run, aborted: bool, counts: &Counts, duration_us: Option<u64>) -> Value {
    json!({
        "event": "suite_finished",
        "aborted": aborted,
        "unfinished_tests": 0,
        "unfinished_groups": 0,
        "counts": counts,
        "duration_us": duration_us,
        "meta": {"exit_code": run.exit_code},
    })
}

/// Add the identity keys and each record's offset as `received_offset_us`.
fn stamp(run: &Run, timeline: Vec<(u64, Value)>) -> Vec<Value> {
    timeline
        .into_iter()
        .map(|(at, mut record)| {
            record["suite_id"] = json!(run.suite_id);
            record["report_id"] = json!(run.report_id);
            record["received_offset_us"] = json!(at);
            record
        })
        .collect()
}

fn with(core: &Value, extra: Value) -> Value {
    let mut record = core.clone();
    if let (Value::Object(record), Value::Object(extra)) = (&mut record, extra) {
        record.extend(extra);
    }
    record
}

/// Core text: 1 to 4,096 bytes, without U+0000.
fn text(value: &str) -> String {
    let cut = truncate(value, MAX_TEXT_BYTES);
    if cut.is_empty() {
        "-".to_owned()
    } else {
        cut
    }
}

fn meta_text(value: &str) -> String {
    truncate(value, MAX_META_STRING_BYTES)
}

/// At most `limit` UTF-8 bytes, cut on a character boundary.
fn truncate(value: &str, limit: usize) -> String {
    let value = value.replace('\0', "\u{FFFD}");
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Run {
        Run {
            suite_id: "suite".into(),
            report_id: "report".into(),
            partition: None,
            partition_count: 1,
            runner_version: Some("0.9.152".into()),
            files: HashMap::from([("app::api".to_owned(), "tests/api.rs".to_owned())]),
            started_us: 0,
            exit_code: Some(100),
            interrupted: false,
        }
    }

    // One binary covering each way nextest records a test's attempts. Times
    // are offsets from the run's timestamp, which is in UTC+02:00.
    const JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="5" uuid="45c50042-482e-477e-88a2-60cfcc3eaf95" timestamp="2026-01-01T12:00:00.000+02:00" time="0.500">
  <testsuite name="app::api" tests="5">
    <testcase name="ignored" classname="app::api" time="0.000"><skipped message="Skipped: ignored"/></testcase>
    <testcase name="always_fails" classname="app::api" timestamp="2026-01-01T12:00:00.010+02:00" time="0.020">
      <failure type="test failure with exit code 101">secret</failure>
      <rerunFailure timestamp="2026-01-01T12:00:00.100+02:00" time="0.030" type="test timeout">secret</rerunFailure>
    </testcase>
    <testcase name="routes::flaky" classname="app::api" timestamp="2026-01-01T12:00:00.300+02:00" time="0.040">
      <flakyFailure timestamp="2026-01-01T12:00:00.050+02:00" time="0.005" type="test abort">secret</flakyFailure>
      <flakyError timestamp="2026-01-01T12:00:00.200+02:00" time="0.006" type="test failure">secret</flakyError>
    </testcase>
    <testcase name="flaky_fail" classname="app::api" timestamp="2026-01-01T12:00:00.400+02:00" time="0.001">
      <failure type="test failure"/>
      <flakyFailure timestamp="2026-01-01T12:00:00.020+02:00" time="0.002" type="test failure"/>
    </testcase>
    <testcase name="passes" classname="app::api" timestamp="2026-01-01T12:00:00.005+02:00" time="0.001"/>
  </testsuite>
</testsuites>"#;

    fn finished_tests(records: &[Value]) -> Vec<(String, u64, Option<u64>, String, Value)> {
        records
            .iter()
            .filter(|r| r["event"] == "test_finished")
            .map(|r| {
                (
                    r["name"].as_str().unwrap().to_owned(),
                    r["started_offset_us"].as_u64().unwrap(),
                    r["duration_us"].as_u64(),
                    r["outcome"].as_str().unwrap().to_owned(),
                    r.get("meta").cloned().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    #[test]
    fn every_attempt_is_a_pair_in_time_order() {
        let records = records(&run(), JUNIT).unwrap();
        assert_eq!(records[0]["started_at"], "2026-01-01T10:00:00.000Z");
        assert_eq!(
            records[0]["meta"]["run_uuid"],
            "45c50042-482e-477e-88a2-60cfcc3eaf95"
        );
        let rows = finished_tests(&records);
        let row = |name: &str, at, duration, outcome: &str, meta| {
            (name.to_owned(), at, duration, outcome.to_owned(), meta)
        };
        assert_eq!(
            rows,
            vec![
                // The skipped test never started: it sits at the group's start.
                row(
                    "ignored",
                    5_000,
                    None,
                    "skipped",
                    json!({"skip_reason": "Skipped: ignored"})
                ),
                row("passes", 5_000, Some(1_000), "passed", Value::Null),
                row(
                    "flaky_fail",
                    20_000,
                    Some(2_000),
                    "failed",
                    json!({"failure_type": "test failure"})
                ),
                row(
                    "always_fails",
                    10_000,
                    Some(20_000),
                    "failed",
                    json!({"failure_type": "test failure with exit code 101"})
                ),
                row(
                    "flaky",
                    50_000,
                    Some(5_000),
                    "failed",
                    json!({"failure_type": "test abort"})
                ),
                row(
                    "always_fails",
                    100_000,
                    Some(30_000),
                    "failed",
                    json!({"invocations": 2, "failure_type": "test timeout"})
                ),
                row(
                    "flaky",
                    200_000,
                    Some(6_000),
                    "errored",
                    json!({"invocations": 2, "failure_type": "test failure"})
                ),
                row(
                    "flaky",
                    300_000,
                    Some(40_000),
                    "passed",
                    json!({"invocations": 3, "flaky": true})
                ),
                // nextest's flaky-fail-status = "failure": the last attempt passed,
                // but the test counts as failed, as nextest itself counts it.
                row(
                    "flaky_fail",
                    400_000,
                    Some(1_000),
                    "failed",
                    json!({"invocations": 2, "flaky": true, "failure_type": "test failure"})
                ),
            ]
        );
        let flaky = records.iter().find(|r| r["name"] == "flaky").unwrap();
        assert_eq!(flaky["path"], json!(["routes"]));

        let group = records
            .iter()
            .find(|r| r["event"] == "group_finished")
            .unwrap();
        assert_eq!(group["file"], "tests/api.rs");
        assert_eq!(group["started_offset_us"], 5_000);
        assert_eq!(group["duration_us"], 396_000);
        assert_eq!(group["outcome"], "failed");
        assert_eq!(
            group["counts"],
            json!({"passed": 2, "failed": 2, "skipped": 1})
        );

        let last = records.last().unwrap();
        assert_eq!(last["event"], "suite_finished");
        assert_eq!(
            last["counts"],
            json!({"passed": 2, "failed": 2, "skipped": 1})
        );
        assert_eq!(last["duration_us"], 500_000);
        assert_eq!(last["aborted"], false);
        assert!(!serde_json::to_string(&records).unwrap().contains("secret"));
    }

    #[test]
    fn started_events_precede_their_finished_events() {
        let records = records(&run(), JUNIT).unwrap();
        let mut open = std::collections::HashSet::new();
        for record in &records {
            let id = record["execution_id"].as_u64();
            match record["event"].as_str().unwrap() {
                "group_started" | "test_started" => assert!(open.insert(id.unwrap())),
                "group_finished" | "test_finished" => assert!(open.remove(&id.unwrap())),
                _ => {}
            }
        }
        assert!(open.is_empty());
        let offsets: Vec<u64> = records
            .iter()
            .map(|r| r["received_offset_us"].as_u64().unwrap())
            .collect();
        assert!(offsets.windows(2).all(|w| w[0] <= w[1]), "{offsets:?}");
    }
}
