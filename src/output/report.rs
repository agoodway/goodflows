//! The closing text run report, rendered from the `summary` event alone.

use super::{instance, invocation_label, short_id, text, visible};
use anyhow::Result;
use colored::Colorize;
use serde_json::Value;
use std::io::Write;

/// Text-only summary key: the `--config` path the user passed explicitly.
pub(super) const CONFIG: &str = "rerun_config";
/// Text-only summary key: the caller's Sprite identity of a remote run.
pub(super) const SPRITE: &str = "rerun_sprite";
/// Text-only summary key: true when a signal arrived before the summary.
pub(super) const INTERRUPTED: &str = "interrupted";

/// The runner's skip reason for a job whose dependency did not pass.
const DEPENDENCY_SKIP: &str = "dependency did not pass";

/// Result statuses from most to least severe: a row takes the first status any
/// of its instances has. Must list every `runner::Status` (enforced by the
/// `severity_lists_every_runner_status` test), so every row finds a status.
const SEVERITY: [&str; 5] = ["failed", "timed_out", "cancelled", "passed", "skipped"];

/// One configured job in the run report, with partition instances rolled up.
struct Row<'a> {
    name: &'a str,
    status: &'static str,
    /// Longest duration among instances that entered execution.
    duration: Option<u64>,
    note: String,
    /// Skipped because a dependency did not pass, so a rerun of the failed
    /// jobs should include it.
    dependency_skipped: bool,
}

/// Groups consecutive results with the same name. The runner emits results in
/// configured job order, then partition index, so groups are configured jobs.
fn rows(results: &[Value]) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut rest = results;
    while let Some(first) = rest.first() {
        let name = text(first, "name");
        let len = rest.iter().take_while(|r| text(r, "name") == name).count();
        let (group, tail) = rest.split_at(len);
        rest = tail;
        let counts =
            SEVERITY.map(|status| group.iter().filter(|r| text(r, "status") == status).count());
        let status = SEVERITY
            .iter()
            .zip(counts)
            .find(|(_, n)| *n > 0)
            .map(|(status, _)| *status)
            .expect("every runner status is listed in SEVERITY");
        let duration = group
            .iter()
            .filter(|r| r["started"] == true)
            .filter_map(|r| r["duration_ms"].as_u64())
            .max();
        let mut notes = Vec::new();
        // A workflow-call job is one row; its child invocation has its own report.
        if let Some(target) = first["workflow"].as_str() {
            notes.push(match first["child_run_id"].as_str() {
                Some(child) => format!(
                    "workflow {} [{}]",
                    visible(target),
                    visible(short_id(child))
                ),
                None => format!("workflow {}", visible(target)),
            });
        }
        let partitioned = group.iter().any(|r| r["partition"].is_object());
        let all_passed = group.iter().all(|r| text(r, "status") == "passed");
        if partitioned && !all_passed {
            let breakdown: Vec<_> = SEVERITY
                .iter()
                .zip(counts)
                .filter(|(status, n)| **status != "passed" && *n > 0)
                .map(|(status, n)| format!("{n}/{} {}", group.len(), status.replace('_', " ")))
                .collect();
            notes.push(breakdown.join(", "));
        }
        let mut dependency_skipped = false;
        if status == "skipped" {
            let mut reasons: Vec<&str> = Vec::new();
            for error in group.iter().filter_map(|r| r["error"].as_str()) {
                dependency_skipped |= error == DEPENDENCY_SKIP;
                if !error.is_empty() && !reasons.contains(&error) {
                    reasons.push(error);
                }
            }
            if !reasons.is_empty() {
                notes.push(visible(&reasons.join("; ")).into_owned());
            }
        }
        rows.push(Row {
            name,
            status,
            duration,
            note: notes.join("  "),
            dependency_skipped,
        });
    }
    rows
}

/// `ms` below a second, `x.ys` below a minute, `XmYYs` from a minute (truncated).
fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{}.{}s", ms / 1000, ms % 1000 / 100)
    } else {
        format!("{}m{:02}s", ms / 60_000, ms % 60_000 / 1000)
    }
}

fn row_label(status: &str) -> (&'static str, colored::Color) {
    use colored::Color;
    match status {
        "passed" => ("PASS", Color::Green),
        "skipped" => ("SKIP", Color::Yellow),
        "cancelled" => ("CANCEL", Color::Yellow),
        "timed_out" => ("TIMEOUT", Color::Red),
        _ => ("FAIL", Color::Red),
    }
}

/// The closing text report, built only from the `summary` event.
pub(super) fn report(event: &Value, out: &mut impl Write) -> Result<()> {
    let results: &[Value] = event["results"].as_array().map_or(&[], Vec::as_slice);
    let has = |status: &str| results.iter().any(|r| text(r, "status") == status);
    // `outcome` comes from the same signal snapshot as the exit code; the
    // fallback covers summaries without it.
    let (outcome, failed) = if event["outcome"] == "failed" || has("failed") || has("timed_out") {
        ("FAILED".red(), true)
    } else if event["outcome"] == "cancelled" || has("cancelled") || event[INTERRUPTED] == true {
        // A signal after every job finished still makes the run exit 128+n.
        ("CANCELLED".yellow(), false)
    } else {
        ("PASSED".green(), false)
    };
    write!(out, "\n{}  {}", invocation_label(event), outcome.bold())?;
    if let Some(ms) = event["duration_ms"].as_u64() {
        write!(out, "  in {}", duration(ms))?;
    }
    writeln!(out)?;
    // A child invocation's report names the call that started it.
    let child = event["parent_run_id"].as_str();
    if let Some(parent) = child {
        writeln!(
            out,
            "called by {} in run {}",
            visible(text(event, "caller_job")),
            visible(parent)
        )?;
    }

    let rows = rows(results);
    // Alignment counts chars, so it assumes narrow (single-column) characters;
    // wide characters such as CJK in job names shift that row's later columns.
    let name_width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    let label_width = rows
        .iter()
        .map(|r| row_label(r.status).0.len())
        .max()
        .unwrap_or(0);
    let durations: Vec<_> = rows.iter().map(|r| r.duration.map(duration)).collect();
    let duration_width = durations
        .iter()
        .flatten()
        .map(String::len)
        .max()
        .unwrap_or(0);
    for (row, elapsed) in rows.iter().zip(&durations) {
        let (label, color) = row_label(row.status);
        let mut line = format!(
            "  {}{}  {:<name_width$}",
            label.color(color),
            " ".repeat(label_width - label.len()),
            row.name
        );
        if duration_width > 0 {
            line.push_str(&format!(
                "  {:>duration_width$}",
                elapsed.as_deref().unwrap_or("")
            ));
        }
        if !row.note.is_empty() {
            line.push_str("  ");
            line.push_str(&row.note);
        }
        writeln!(out, "{}", line.trim_end())?;
    }
    let counts: Vec<_> = [
        ("passed", "passed"),
        ("failed", "failed"),
        ("timed_out", "timed out"),
        ("skipped", "skipped"),
        ("cancelled", "cancelled"),
    ]
    .iter()
    .filter_map(|(status, word)| {
        let n = rows.iter().filter(|r| r.status == *status).count();
        (n > 0).then(|| format!("{n} {word}"))
    })
    .collect();
    writeln!(
        out,
        "{}",
        if counts.is_empty() {
            "no jobs".to_owned()
        } else {
            counts.join(", ")
        }
    )?;

    if failed {
        writeln!(out, "\nFailures")?;
        let mut first = true;
        for result in results
            .iter()
            .filter(|r| matches!(text(r, "status"), "failed" | "timed_out"))
        {
            if !first {
                writeln!(out)?;
            }
            first = false;
            failure(result, out)?;
        }
    }

    let log_dir = event["log_dir"].as_str();
    let history_file = event["history_file"].as_str();
    let failed_rows = rows
        .iter()
        .any(|r| matches!(r.status, "failed" | "timed_out"));
    // Failed jobs plus dependents skipped because a dependency did not pass;
    // `--jobs` adds each selected job's prerequisites.
    // Only the root report offers a rerun: rerunning a child directly would
    // skip the preparation its caller runs first.
    let rerun_jobs: Vec<&str> = rows
        .iter()
        .filter(|r| {
            child.is_none()
                && failed_rows
                && (matches!(r.status, "failed" | "timed_out") || r.dependency_skipped)
        })
        .map(|r| r.name)
        .collect();
    if log_dir.is_some() || history_file.is_some() || !rerun_jobs.is_empty() {
        writeln!(out)?;
    }
    if let Some(dir) = log_dir {
        writeln!(out, "Logs: {}", visible(dir))?;
    }
    if let Some(file) = history_file {
        writeln!(out, "History: {}", visible(file))?;
    }
    if !rerun_jobs.is_empty() {
        let context = Rerun {
            workflow: text(event, "workflow"),
            environment: event["environment"].as_str(),
            config: event[CONFIG].as_str(),
            sprite: event[SPRITE].as_str(),
        };
        match rerun_jobs.iter().find(|name| name.contains(',')) {
            Some(name) => {
                writeln!(
                    out,
                    "Selective rerun unavailable: job {} contains a comma, which --jobs cannot select.",
                    shell_quote(name)
                )?;
                writeln!(out, "Rerun workflow: {}", context.command(None))?;
            }
            None => writeln!(out, "Rerun failed: {}", context.command(Some(&rerun_jobs)))?,
        }
    }
    Ok(())
}

/// One Failures entry for a failed or timed-out instance. Output tails stay
/// in JSON results only; text mode already streamed the full output live.
fn failure(result: &Value, out: &mut impl Write) -> Result<()> {
    let status = if text(result, "status") == "timed_out" {
        "TIMED OUT"
    } else {
        "FAILED"
    };
    write!(out, "--- {}  {}", instance(result), status.red())?;
    if let Some(code) = result["exit_code"].as_i64() {
        write!(out, "  exit {code}")?;
    }
    writeln!(out)?;
    let step = result["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|step| matches!(text(step, "status"), "failed" | "timed_out"));
    if let Some(step) = step {
        writeln!(out, "  step: {}", text(step, "id"))?;
    }
    // A failed call points to its child, whose report details the failed jobs.
    if let Some(target) = result["workflow"].as_str() {
        writeln!(out, "  workflow: {}", visible(target))?;
        if let Some(child) = result["child_run_id"].as_str() {
            writeln!(out, "  child run: {}", visible(child))?;
        }
    }
    if let Some(command) = result["command"].as_str() {
        writeln!(out, "  command: {}", visible(command))?;
    }
    if let Some(error) = result["error"].as_str() {
        writeln!(out, "  error: {}", visible(error))?;
    }
    Ok(())
}

/// What a rerun hint preserves from the original run.
struct Rerun<'a> {
    workflow: &'a str,
    environment: Option<&'a str>,
    /// Only when the original run passed `--config` explicitly.
    config: Option<&'a str>,
    /// Only for a remote run: its original caller-supplied identity.
    sprite: Option<&'a str>,
}

impl Rerun<'_> {
    /// `goodflows run` with the workflow, any config and environment and
    /// (optionally) jobs. Option values use equals form so a leading hyphen is
    /// never read as a flag; a workflow beginning with `-` goes last, after `--`.
    fn command(&self, jobs: Option<&[&str]>) -> String {
        let mut words = vec!["goodflows".to_owned(), "run".to_owned()];
        let hyphen = self.workflow.starts_with('-');
        if !hyphen {
            words.push(shell_quote(self.workflow));
        }
        if let Some(config) = self.config {
            words.push(format!("--config={}", shell_quote(config)));
        }
        if let Some(environment) = self.environment {
            words.push(format!("--env={}", shell_quote(environment)));
        }
        if let Some(sprite) = self.sprite {
            words.push(format!("--sprite={}", shell_quote(sprite)));
        }
        if let Some(jobs) = jobs {
            words.push(format!("--jobs={}", shell_quote(&jobs.join(","))));
        }
        if hyphen {
            words.push("--".into());
            words.push(shell_quote(self.workflow));
        }
        words.join(" ")
    }
}

/// POSIX shell quoting: plain tokens stay bare, anything else is single-quoted
/// with each embedded apostrophe written as `'"'"'`.
pub(super) fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r#"'"'"'"#))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `rows` relies on every runner status appearing in `SEVERITY`. The
    /// exhaustive match makes adding a `Status` variant a compile error here.
    #[test]
    fn severity_lists_every_runner_status() {
        use crate::runner::Status;
        let all = |status: Status| match status {
            Status::Passed
            | Status::Failed
            | Status::Skipped
            | Status::TimedOut
            | Status::Cancelled => status,
        };
        for status in [
            Status::Passed,
            Status::Failed,
            Status::Skipped,
            Status::TimedOut,
            Status::Cancelled,
        ]
        .map(all)
        {
            let name = serde_json::to_value(status).unwrap();
            assert!(
                SEVERITY.contains(&name.as_str().unwrap()),
                "{name} missing from SEVERITY"
            );
        }
    }

    fn rendered(event: Value) -> String {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        super::super::render(false, event, &mut out, &mut err).unwrap();
        assert!(err.is_empty());
        String::from_utf8(out).unwrap()
    }

    fn result(name: &str, status: &str, started: bool, ms: u64) -> Value {
        json!({"name": name, "status": status, "started": started, "duration_ms": ms,
               "command": null, "exit_code": null, "error": null, "outputs": {}})
    }

    fn part(mut result: Value, index: u64, count: u64) -> Value {
        result["partition"] = json!({"index": index, "count": count});
        result
    }

    fn with(mut result: Value, key: &str, value: Value) -> Value {
        result[key] = value;
        result
    }

    fn summary(results: Vec<Value>) -> Value {
        json!({"type": "summary", "workflow": "ci", "duration_ms": 710, "results": results})
    }

    fn report_of(results: Vec<Value>) -> String {
        colored::control::set_override(false);
        rendered(summary(results))
    }

    /// The table row for `name`, with internal runs of spaces collapsed.
    fn row(report: &str, name: &str) -> String {
        report
            .lines()
            .find(|line| line.starts_with("  ") && line.split_whitespace().nth(1) == Some(name))
            .unwrap_or_else(|| panic!("no row for {name}: {report}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn rows_roll_up_partitions() {
        let report = report_of(vec![
            result("plain", "passed", true, 12),
            part(result("allpass", "passed", true, 5), 1, 3),
            part(result("allpass", "passed", true, 9), 2, 3),
            part(result("allpass", "passed", true, 7), 3, 3),
            part(result("mixed", "failed", true, 30), 1, 3),
            part(result("mixed", "passed", true, 50), 2, 3),
            part(result("mixed", "failed", true, 40), 3, 3),
            part(result("some", "passed", true, 20), 1, 3),
            part(
                with(
                    result("some", "skipped", true, 0),
                    "error",
                    json!("condition false"),
                ),
                2,
                3,
            ),
            part(
                with(
                    result("some", "skipped", true, 3),
                    "error",
                    json!("condition false"),
                ),
                3,
                3,
            ),
            part(
                with(
                    result("blocked", "skipped", false, 0),
                    "error",
                    json!("dependency did not pass"),
                ),
                1,
                2,
            ),
            part(
                with(
                    result("blocked", "skipped", false, 0),
                    "error",
                    json!("dependency did not pass"),
                ),
                2,
                2,
            ),
            with(
                result("cond", "skipped", true, 0),
                "error",
                json!("condition evaluated false"),
            ),
            part(
                with(
                    result("reasons", "skipped", false, 0),
                    "error",
                    json!("dependency did not pass"),
                ),
                1,
                3,
            ),
            part(
                with(
                    result("reasons", "skipped", false, 0),
                    "error",
                    json!("fail_fast: previous job failed"),
                ),
                2,
                3,
            ),
            part(
                with(
                    result("reasons", "skipped", false, 0),
                    "error",
                    json!("dependency did not pass"),
                ),
                3,
                3,
            ),
            result("zero", "passed", true, 0),
            with(
                result("queued", "cancelled", false, 0),
                "error",
                json!("cancelled by signal"),
            ),
            with(
                result("running", "cancelled", true, 80),
                "error",
                json!("cancelled by signal"),
            ),
        ]);
        assert_eq!(row(&report, "plain"), "PASS plain 12ms");
        assert_eq!(row(&report, "allpass"), "PASS allpass 9ms");
        assert_eq!(row(&report, "mixed"), "FAIL mixed 50ms 2/3 failed");
        assert_eq!(row(&report, "some"), "PASS some 20ms 2/3 skipped");
        assert_eq!(
            row(&report, "blocked"),
            "SKIP blocked 2/2 skipped dependency did not pass"
        );
        assert_eq!(
            row(&report, "cond"),
            "SKIP cond 0ms condition evaluated false"
        );
        assert_eq!(
            row(&report, "reasons"),
            "SKIP reasons 3/3 skipped dependency did not pass; fail_fast: previous job failed"
        );
        assert_eq!(row(&report, "zero"), "PASS zero 0ms");
        assert_eq!(row(&report, "queued"), "CANCEL queued");
        assert_eq!(row(&report, "running"), "CANCEL running 80ms");
        // One row per configured job, in result order.
        let names: Vec<_> = report
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| l.split_whitespace().nth(1))
            .take(11)
            .collect();
        assert_eq!(
            names,
            [
                "plain", "allpass", "mixed", "some", "blocked", "cond", "reasons", "zero",
                "queued", "running"
            ]
        );
    }

    #[test]
    fn rollup_orders_breakdown_by_severity() {
        let report = report_of(vec![
            part(result("t", "timed_out", true, 1), 1, 4),
            part(result("t", "cancelled", true, 1), 2, 4),
            part(result("t", "failed", true, 1), 3, 4),
            part(result("t", "skipped", false, 0), 4, 4),
        ]);
        assert_eq!(
            row(&report, "t"),
            "FAIL t 1ms 1/4 failed, 1/4 timed out, 1/4 cancelled, 1/4 skipped"
        );
    }

    #[test]
    fn header_follows_the_summary_outcome() {
        let mut event = summary(vec![result("a", "passed", true, 1)]);
        event["outcome"] = json!("cancelled");
        colored::control::set_override(false);
        assert!(rendered(event).starts_with("\nci  CANCELLED  in "));
    }

    #[test]
    fn durations_use_the_coarsest_readable_unit() {
        for (ms, text) in [
            (0, "0ms"),
            (999, "999ms"),
            (1000, "1.0s"),
            (1234, "1.2s"),
            (59_999, "59.9s"),
            (60_000, "1m00s"),
            (65_400, "1m05s"),
            (3_725_000, "62m05s"),
        ] {
            assert_eq!(duration(ms), text);
        }
    }

    #[test]
    fn header_states_outcome_and_wall_time() {
        let header = |results: Vec<Value>| report_of(results).lines().nth(1).unwrap().to_owned();
        assert_eq!(
            header(vec![
                result("a", "passed", true, 1),
                result("b", "skipped", false, 0)
            ]),
            "ci  PASSED  in 710ms"
        );
        assert_eq!(
            header(vec![
                result("a", "cancelled", true, 1),
                result("b", "passed", true, 1)
            ]),
            "ci  CANCELLED  in 710ms"
        );
        assert_eq!(
            header(vec![
                result("a", "cancelled", true, 1),
                result("b", "timed_out", true, 1)
            ]),
            "ci  FAILED  in 710ms"
        );
        assert_eq!(
            header(vec![
                result("a", "failed", true, 1),
                result("b", "cancelled", true, 1)
            ]),
            "ci  FAILED  in 710ms"
        );
        // A signal that arrives after every job finished still cancels the run.
        let interrupted = |results| {
            colored::control::set_override(false);
            let event = with(summary(results), INTERRUPTED, json!(true));
            rendered(event).lines().nth(1).unwrap().to_owned()
        };
        assert_eq!(
            interrupted(vec![result("a", "passed", true, 1)]),
            "ci  CANCELLED  in 710ms"
        );
        assert_eq!(
            interrupted(vec![result("a", "failed", true, 1)]),
            "ci  FAILED  in 710ms"
        );
    }

    #[test]
    fn table_pads_names_and_counts_configured_jobs() {
        let report = report_of(vec![
            result("fmt", "passed", true, 204),
            result("lint", "passed", true, 110),
            with(
                part(result("test", "failed", true, 3), 1, 2),
                "exit_code",
                json!(1),
            ),
            with(
                part(result("test", "failed", true, 2), 2, 2),
                "exit_code",
                json!(1),
            ),
            with(
                result("deploy", "skipped", false, 0),
                "error",
                json!("dependency did not pass"),
            ),
        ]);
        let expected = "\nci  FAILED  in 710ms\n  PASS  fmt     204ms\n  PASS  lint    110ms\n  FAIL  test      3ms  2/2 failed\n  SKIP  deploy         dependency did not pass\n2 passed, 1 failed, 1 skipped\n";
        assert!(report.starts_with(expected), "{report}");
        let passing = report_of(vec![result("a", "passed", true, 1)]);
        assert_eq!(
            passing,
            "\nci  PASSED  in 710ms\n  PASS  a  1ms\n1 passed\n"
        );
    }

    #[test]
    fn failures_detail_each_failed_instance() {
        let failed = |index, tail: &str| {
            let r = part(result("test", "failed", true, 3), index, 2);
            let r = with(r, "exit_code", json!(1));
            let r = with(r, "command", json!("cargo test"));
            let r = with(r, "error", json!("exited with 1"));
            let r = with(r, "stderr_tail", json!(tail));
            let r = with(r, "stderr_truncated", json!(index == 2));
            let r = with(r, "stdout_tail", json!(""));
            with(r, "stdout_truncated", json!(false))
        };
        let report = report_of(vec![
            failed(1, "boom-1\n"),
            failed(2, "boom-2\nmore\n"),
            with(
                result("other", "cancelled", true, 4),
                "error",
                json!("cancelled by signal"),
            ),
        ]);
        let failures = &report[report.find("\nFailures\n").unwrap()..];
        assert!(
            failures.starts_with("\nFailures\n--- test (1/2)  FAILED  exit 1\n  command: cargo test\n  error: exited with 1\n\n--- test (2/2)  FAILED  exit 1\n  command: cargo test\n  error: exited with 1\n\n"),
            "{report}"
        );
        assert!(!failures.contains("tail"), "{report}");
        assert!(!failures.contains("boom"), "{report}");
        assert!(!failures.contains("other"), "{report}");

        // A failed step names the step; its command is the step's.
        let stepped = with(
            result("test", "failed", true, 3),
            "steps",
            json!([
                {"id": "unit", "command": "true", "status": "passed", "exit_code": 0, "error": null},
                {"id": "integration", "command": "make it", "status": "failed", "exit_code": 2, "error": "exited with 2"},
                {"id": "after", "command": "true", "status": "skipped", "exit_code": null, "error": "previous step did not pass"},
            ]),
        );
        let stepped = with(stepped, "command", json!("make it"));
        let stepped = with(stepped, "exit_code", json!(2));
        let stepped = with(stepped, "error", json!("step 'integration': exited with 2"));
        let report = report_of(vec![stepped]);
        assert!(
            report.contains("--- test  FAILED  exit 2\n  step: integration\n  command: make it\n  error: step 'integration': exited with 2\n"),
            "{report}"
        );

        // Failures before or after every step invent no step, command or code.
        for error in ["failed to open log", "job output 'x': unknown"] {
            let r = with(
                result("prep", "failed", true, 1),
                "steps",
                json!([
                    {"id": "a", "command": "true", "status": if error.starts_with("job") {"passed"} else {"skipped"}, "exit_code": null, "error": null},
                ]),
            );
            let report = report_of(vec![with(r, "error", json!(error))]);
            let failures = &report[report.find("\nFailures\n").unwrap()..];
            assert!(
                failures.starts_with(&format!(
                    "\nFailures\n--- prep  FAILED\n  error: {error}\n\n"
                )),
                "{report}"
            );
        }

        // A timeout without an exit code stays explicit.
        let timeout = with(
            result("slow", "timed_out", true, 1000),
            "command",
            json!("sleep 5"),
        );
        let timeout = with(timeout, "error", json!("timed out after 1s"));
        let timeout = with(timeout, "stdout_tail", json!("before"));
        let report = report_of(vec![timeout]);
        assert!(
            report
                .contains("--- slow  TIMED OUT\n  command: sleep 5\n  error: timed out after 1s\n"),
            "{report}"
        );
        assert!(!report.contains("before"), "{report}");
        assert!(report.contains("  TIMEOUT  slow  1.0s"), "{report}");

        // A timed-out step is the failing step; no exit code is invented.
        let stepped = with(
            result("test", "timed_out", true, 1000),
            "steps",
            json!([
                {"id": "a", "command": "true", "status": "passed", "exit_code": 0, "error": null},
                {"id": "slow", "command": "sleep 5", "status": "timed_out", "exit_code": null, "error": "timed out after 1s"},
            ]),
        );
        let report = report_of(vec![stepped]);
        let entry = &report[report.find("\nFailures\n").unwrap() + "\nFailures\n".len()..];
        assert!(
            entry.starts_with("--- test  TIMED OUT\n  step: slow\n"),
            "{report}"
        );
        assert!(!entry.contains("exit"), "{report}");
    }

    #[test]
    fn report_escapes_control_characters_from_jobs() {
        let r = with(
            result("bad", "failed", true, 1),
            "command",
            json!("printf '\x1b[2J'"),
        );
        let r = with(r, "error", json!("oops\rhidden"));
        let skip = with(
            result("later", "skipped", true, 0),
            "error",
            json!("reason\x1b[31m"),
        );
        let report = report_of(vec![r, skip]);
        assert!(!report.contains(['\x1b', '\r', '\x07']), "{report:?}");
        for expected in [
            "  command: printf '\\u{1b}[2J'\n",
            "  error: oops\\rhidden\n",
            "  SKIP  later  0ms  reason\\u{1b}[31m\n",
        ] {
            assert!(report.contains(expected), "{expected:?}: {report:?}");
        }
    }

    #[test]
    fn logs_and_rerun_hint() {
        colored::control::set_override(false);
        let mut event = summary(vec![
            result("lint", "failed", true, 1),
            part(result("test", "failed", true, 1), 1, 2),
            part(result("test", "timed_out", true, 1), 2, 2),
            result("deploy", "skipped", false, 0),
        ]);
        let report = rendered(event.clone());
        assert!(!report.contains("Logs:"));
        assert!(
            report.ends_with("\nRerun failed: goodflows run ci --jobs='lint,test'\n"),
            "{report}"
        );
        event["log_dir"] = json!("logs/ci");
        event["environment"] = json!("prod");
        let report = rendered(event);
        assert!(
            report.ends_with(
                "\n\nLogs: logs/ci\nRerun failed: goodflows run ci --env=prod --jobs='lint,test'\n"
            ),
            "{report}"
        );

        let success = summary(vec![result("a", "passed", true, 1)]);
        let report = rendered(with(success, "log_dir", json!("logs")));
        assert!(report.ends_with("1 passed\n\nLogs: logs\n"), "{report}");
        assert!(!report.contains("Rerun") && !report.contains("Failures"));

        // The history file follows the log directory, or stands alone.
        let success = summary(vec![result("a", "passed", true, 1)]);
        let logged = with(success.clone(), "log_dir", json!("logs/run"));
        let report = rendered(with(logged, "history_file", json!("/w/h/ci/run.json")));
        assert!(
            report.ends_with("1 passed\n\nLogs: logs/run\nHistory: /w/h/ci/run.json\n"),
            "{report}"
        );
        let report = rendered(with(success, "history_file", json!("/w/h/ci/run.json")));
        assert!(
            report.ends_with("1 passed\n\nHistory: /w/h/ci/run.json\n"),
            "{report}"
        );

        let comma = summary(vec![result("test,unit", "failed", true, 1)]);
        let report = rendered(comma);
        assert!(
            report.ends_with("Selective rerun unavailable: job 'test,unit' contains a comma, which --jobs cannot select.\nRerun workflow: goodflows run ci\n"),
            "{report}"
        );

        // The fallback keeps the environment and a leading-hyphen workflow.
        let mut comma = summary(vec![result("test,unit", "failed", true, 1)]);
        comma["environment"] = json!("prod");
        comma["workflow"] = json!("-wf");
        let report = rendered(comma);
        assert!(
            report.ends_with("\nRerun workflow: goodflows run --env=prod -- -wf\n"),
            "{report}"
        );

        // A timed-out-only run still gets a hint.
        let report = rendered(summary(vec![
            result("ok", "passed", true, 1),
            result("slow", "timed_out", true, 1000),
        ]));
        assert!(
            report.ends_with("\nRerun failed: goodflows run ci --jobs=slow\n"),
            "{report}"
        );

        // Dependents skipped because a dependency did not pass are rerun too, in
        // row order; other skips and cancellations are not.
        let blocked = |name| {
            with(
                result(name, "skipped", false, 0),
                "error",
                json!(DEPENDENCY_SKIP),
            )
        };
        let report = rendered(summary(vec![
            blocked("early"),
            result("build", "failed", true, 1),
            part(blocked("deploy"), 1, 2),
            part(blocked("deploy"), 2, 2),
            with(
                result("late", "skipped", false, 0),
                "error",
                json!("fail_fast: previous job failed"),
            ),
            with(
                result("cond", "skipped", true, 0),
                "error",
                json!("condition evaluated false"),
            ),
            with(
                result("stopped", "cancelled", false, 0),
                "error",
                json!("cancelled by signal"),
            ),
        ]));
        assert!(
            report.ends_with("\nRerun failed: goodflows run ci --jobs='early,build,deploy'\n"),
            "{report}"
        );
        // Without a failure, a dependency skip gets no hint.
        let report = rendered(summary(vec![
            with(
                result("dep", "cancelled", true, 1),
                "error",
                json!("cancelled by signal"),
            ),
            blocked("after"),
        ]));
        assert!(!report.contains("Rerun"), "{report}");
        // A comma in a dependent's name also forces the full-workflow fallback.
        let report = rendered(summary(vec![
            result("build", "failed", true, 1),
            blocked("a,b"),
        ]));
        assert!(
            report.ends_with("job 'a,b' contains a comma, which --jobs cannot select.\nRerun workflow: goodflows run ci\n"),
            "{report}"
        );

        // An explicit --config path is carried into both hint forms.
        let event = with(
            summary(vec![result("t", "failed", true, 1)]),
            CONFIG,
            json!("ci/goodflows.toml"),
        );
        assert!(rendered(event)
            .ends_with("\nRerun failed: goodflows run ci --config=ci/goodflows.toml --jobs=t\n"));
        let event = with(
            summary(vec![result("t,u", "failed", true, 1)]),
            CONFIG,
            json!("my config.toml"),
        );
        assert!(rendered(event)
            .ends_with("\nRerun workflow: goodflows run ci --config='my config.toml'\n"));
    }

    #[test]
    fn call_rows_and_child_reports_link_invocations() {
        colored::control::set_override(false);
        let child = "0123456789abcdef-0000-0000-0000-000000000000";
        let call = |status, ms| {
            let r = with(
                result("integration", status, true, ms),
                "workflow",
                json!("suite"),
            );
            with(r, "child_run_id", json!(child))
        };
        let failed = with(call("failed", 0), "error", json!("workflow 'suite' failed"));
        let skipped = with(
            with(
                result("again", "skipped", false, 0),
                "workflow",
                json!("suite"),
            ),
            "error",
            json!(DEPENDENCY_SKIP),
        );
        let mut event = summary(vec![
            result("prepare", "passed", true, 5),
            failed,
            skipped,
            with(
                result("publish", "skipped", false, 0),
                "error",
                json!(DEPENDENCY_SKIP),
            ),
        ]);
        event["invocation_id"] = json!("feedbeef-0000-0000-0000-000000000000");
        let report = rendered(event.clone());
        assert!(
            report.starts_with("\nci [feedbeef]  FAILED  in 710ms\n"),
            "{report}"
        );
        // A started call shows its integer duration (0ms included) and child.
        assert_eq!(
            row(&report, "integration"),
            "FAIL integration 0ms workflow suite [01234567]"
        );
        // A call skipped before activation has no duration or child.
        assert_eq!(
            row(&report, "again"),
            "SKIP again workflow suite dependency did not pass"
        );
        assert!(
            report.contains("1 passed, 1 failed, 2 skipped\n"),
            "{report}"
        );
        assert!(
            report.contains(&format!(
                "--- integration  FAILED\n  workflow: suite\n  child run: {child}\n  error: workflow 'suite' failed\n"
            )),
            "{report}"
        );
        assert!(
            !report.contains("command:") && !report.contains("exit"),
            "{report}"
        );
        assert!(
            report
                .ends_with("\nRerun failed: goodflows run ci --jobs='integration,again,publish'\n"),
            "{report}"
        );

        // A child report names its caller and parent and offers no rerun.
        let mut child_event = summary(vec![with(
            result("test", "failed", true, 3),
            "error",
            json!("exited with 1"),
        )]);
        child_event["workflow"] = json!("suite");
        child_event["invocation_id"] = json!(child);
        child_event["parent_run_id"] = json!("feedbeef-0000-0000-0000-000000000000");
        child_event["caller_job"] = json!("integ\x1bration");
        let report = rendered(child_event);
        assert!(
            report.starts_with("\nsuite [01234567]  FAILED  in 710ms\ncalled by integ\\u{1b}ration in run feedbeef-0000-0000-0000-000000000000\n"),
            "{report}"
        );
        assert!(report.contains("--- test  FAILED\n"), "{report}");
        assert!(!report.contains("Rerun"), "{report}");
    }

    #[test]
    fn rerun_quotes_for_the_shell_and_cli() {
        assert_eq!(shell_quote("a-b_c.d:e/f"), "a-b_c.d:e/f");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's $x"), r#"'it'"'"'s $x'"#);
        let rerun = |workflow, environment, config, jobs: &[&str]| {
            Rerun {
                workflow,
                environment,
                config,
                sprite: None,
            }
            .command(Some(jobs))
        };
        assert_eq!(
            rerun("my flow", Some("-prod"), None, &["a b", "-x"]),
            "goodflows run 'my flow' --env=-prod --jobs='a b,-x'"
        );
        assert_eq!(
            rerun("-wf", None, None, &["t"]),
            "goodflows run --jobs=t -- -wf"
        );
        assert_eq!(
            rerun("ci", Some("prod"), Some("-my dir/it's.toml"), &["t"]),
            r#"goodflows run ci --config='-my dir/it'"'"'s.toml' --env=prod --jobs=t"#
        );
    }

    #[test]
    fn remote_rerun_hints_keep_the_sprite_identity() {
        // Selective rerun keeps identity, profile and the controller config spelling.
        let event = with(
            with(
                with(
                    summary(vec![result("t", "failed", true, 1)]),
                    CONFIG,
                    json!("custom.toml"),
                ),
                SPRITE,
                json!("T-example"),
            ),
            "environment",
            json!("test"),
        );
        assert!(
            rendered(event).ends_with(
                "\nRerun failed: goodflows run ci --config=custom.toml --env=test --sprite=T-example --jobs=t\n"
            )
        );
        // Comma fallback: identity before the final `-- -wf`, no --jobs.
        let mut event = with(
            with(
                summary(vec![result("test,unit", "failed", true, 1)]),
                SPRITE,
                json!("T-example"),
            ),
            "environment",
            json!("prod"),
        );
        event["workflow"] = json!("-wf");
        assert!(rendered(event)
            .ends_with("\nRerun workflow: goodflows run --env=prod --sprite=T-example -- -wf\n"));
        // Identities are shell-quoted like every other value.
        let event = with(
            summary(vec![result("t", "failed", true, 1)]),
            SPRITE,
            json!("thread 'x'"),
        );
        assert!(rendered(event).ends_with(
            r#" --sprite='thread '"'"'x'"'"'' --jobs=t
"#
        ));
    }
}
