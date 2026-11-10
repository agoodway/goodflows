//! Text rendering of `history list` and `history show` (design D9).
//!
//! Every value comes from the server and is untrusted: each string passes
//! through [`visible`], and a missing or null value prints as `-`.

use super::report::duration;
use super::{instance, visible};
use anyhow::Result;
use serde_json::Value;
use std::io::Write;

const ABSENT: &str = "-";

/// A server string escaped for the terminal, or `-` when absent or not a string.
fn field(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| ABSENT.to_owned(), |s| visible(s).into_owned())
}

/// A number, an escaped string, or `-`.
fn scalar(value: &Value) -> String {
    match value {
        Value::Number(n) => n.to_string(),
        Value::String(s) => visible(s).into_owned(),
        Value::Bool(b) => b.to_string(),
        _ => ABSENT.to_owned(),
    }
}

fn millis(value: &Value) -> String {
    value.as_u64().map_or_else(|| ABSENT.to_owned(), duration)
}

fn bytes(n: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if n < MIB {
        format!("{} KiB", n.div_ceil(1024))
    } else if n < 1024 * MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else {
        format!("{:.1} GiB", n as f64 / (1024 * MIB) as f64)
    }
}

/// The first 12 characters of a commit, with `*` when the tree was dirty.
fn commit(source: &Value) -> String {
    let Some(commit) = source["commit"].as_str() else {
        return ABSENT.to_owned();
    };
    let short: String = visible(commit).chars().take(12).collect();
    if source["dirty"] == true {
        format!("{short}*")
    } else {
        short
    }
}

/// Left-aligned columns separated by two spaces, without trailing blanks.
fn table(out: &mut impl Write, indent: &str, head: &[&str], rows: &[Vec<String>]) -> Result<()> {
    let mut widths: Vec<usize> = head.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let padded: Vec<String> = cells
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        format!("{indent}{}", padded.join("  ").trim_end())
    };
    writeln!(out, "{}", line(&mut head.iter().copied()))?;
    for row in rows {
        writeln!(out, "{}", line(&mut row.iter().map(String::as_str)))?;
    }
    Ok(())
}

/// `history list`: one row per run, then the next page's cursor if any.
pub(super) fn runs(event: &Value, out: &mut impl Write) -> Result<()> {
    let runs: &[Value] = event["runs"].as_array().map_or(&[], Vec::as_slice);
    if runs.is_empty() {
        writeln!(out, "No stored runs.")?;
    } else {
        let rows: Vec<Vec<String>> = runs
            .iter()
            .map(|run| {
                vec![
                    field(&run["started_at"]),
                    field(&run["outcome"]),
                    field(&run["workflow"]),
                    field(&run["environment"]),
                    commit(&run["source"]),
                    field(&run["source"]["branch"]),
                    field(&run["ci"]["event"]),
                    millis(&run["duration_ms"]),
                    field(&run["run_id"]),
                ]
            })
            .collect();
        table(
            out,
            "",
            &[
                "STARTED", "OUTCOME", "WORKFLOW", "ENV", "COMMIT", "BRANCH", "EVENT", "DURATION",
                "RUN ID",
            ],
            &rows,
        )?;
    }
    if let Some(cursor) = event["next_cursor"].as_str() {
        writeln!(out, "next: --cursor {}", visible(cursor))?;
    }
    Ok(())
}

/// `history show`: the run's fields, one row per result, the command and
/// graph text (absent for a public key), then the linked test reports.
pub(super) fn run(event: &Value, out: &mut impl Write) -> Result<()> {
    let run = &event["run"];
    let doc = &run["document"];
    writeln!(out, "Run {}", field(&run["run_id"]))?;
    let mut header = vec![
        ("workflow", field(&doc["workflow"])),
        ("environment", field(&doc["environment"])),
        ("outcome", field(&doc["outcome"])),
        ("started", field(&doc["started_at"])),
        ("duration", millis(&doc["duration_ms"])),
        (
            "parallelism",
            format!(
                "{} (max in flight {})",
                scalar(&doc["parallelism"]),
                scalar(&doc["max_in_flight"])
            ),
        ),
        ("graph", field(&doc["graph_sha256"])),
    ];
    if let Some(jobs) = doc["requested_jobs"].as_array() {
        let names: Vec<String> = jobs.iter().map(field).collect();
        header.push(("requested jobs", names.join(", ")));
    }
    if doc["parent_run_id"].is_string() {
        header.push((
            "called by",
            format!(
                "{} in run {}",
                field(&doc["caller_job"]),
                field(&doc["parent_run_id"])
            ),
        ));
    }
    let source = &doc["source"];
    header.push((
        "source",
        if source.is_object() {
            let mut line = field(&source["commit"]);
            match source["dirty"].as_bool() {
                Some(true) => line.push_str(" (dirty)"),
                Some(false) => line.push_str(" (clean)"),
                None => {}
            }
            if let Some(branch) = source["branch"].as_str() {
                line.push_str(&format!(" on {}", visible(branch)));
            }
            line
        } else {
            ABSENT.to_owned()
        },
    ));
    let ci = &doc["ci"];
    if ci.is_object() {
        let mut parts = vec![field(&ci["provider"])];
        for (key, label) in [
            ("event", "event"),
            ("run_id", "run"),
            ("attempt", "attempt"),
            ("pull_request", "pull request"),
            ("runner", "runner"),
        ] {
            if !ci[key].is_null() {
                parts.push(format!("{label} {}", scalar(&ci[key])));
            }
        }
        header.push(("ci", parts.join(", ")));
        if ci["url"].is_string() {
            header.push(("ci url", field(&ci["url"])));
        }
    }
    let host = &doc["host"];
    let mut host_line = format!(
        "{} {}, {} CPUs",
        field(&host["os"]),
        field(&host["arch"]),
        scalar(&host["logical_cpus"])
    );
    if let (Some(total), Some(available)) = (
        host["mem_total_bytes"].as_u64(),
        host["mem_available_bytes"].as_u64(),
    ) {
        host_line.push_str(&format!(
            ", {} memory ({} available)",
            bytes(total),
            bytes(available)
        ));
    }
    header.push(("host", host_line));
    for (label, value) in header {
        writeln!(out, "  {label}: {value}")?;
    }

    let results: &[Value] = doc["results"].as_array().map_or(&[], Vec::as_slice);
    writeln!(out, "\nResults")?;
    if results.is_empty() {
        writeln!(out, "  none")?;
    } else {
        let rows: Vec<Vec<String>> = results.iter().map(result_row).collect();
        table(
            out,
            "  ",
            &[
                "JOB", "STATUS", "DURATION", "BLOCKED", "QUEUED", "EXIT", "CPU", "PEAK RSS", "CALL",
            ],
            &rows,
        )?;
        writeln!(out, "\nCommands")?;
        for result in results {
            writeln!(
                out,
                "  {}: {}",
                visible(&instance(result)),
                field(&result["command"])
            )?;
        }
        let errors: Vec<_> = results.iter().filter(|r| r["error"].is_string()).collect();
        if !errors.is_empty() {
            writeln!(out, "\nErrors")?;
            for result in errors {
                writeln!(
                    out,
                    "  {}: {}",
                    visible(&instance(result)),
                    field(&result["error"])
                )?;
            }
        }
    }
    if let Some(jobs) = doc["graph"]["jobs"].as_array() {
        writeln!(out, "\nGraph")?;
        for job in jobs {
            graph_line(out, "  ", &field(&job["name"]), job)?;
            for step in job["steps"].as_array().into_iter().flatten() {
                graph_line(out, "    ", &field(&step["id"]), step)?;
            }
        }
    }

    let reports: &[Value] = run["test_reports"].as_array().map_or(&[], Vec::as_slice);
    writeln!(out, "\nTest reports")?;
    if reports.is_empty() {
        writeln!(out, "  none")?;
    } else {
        let rows: Vec<Vec<String>> = reports
            .iter()
            .map(|report| {
                let partition = match (&report["partition"], &report["partition_count"]) {
                    (Value::Null, _) => ABSENT.to_owned(),
                    (index, Value::Null) => scalar(index),
                    (index, count) => format!("{}/{}", scalar(index), scalar(count)),
                };
                vec![
                    field(&report["job"]),
                    partition,
                    field(&report["job_status"]),
                    field(&report["suite_id"]),
                    field(&report["report_id"]),
                    field(&report["status"]),
                ]
            })
            .collect();
        table(
            out,
            "  ",
            &[
                "JOB",
                "PARTITION",
                "JOB STATUS",
                "SUITE",
                "REPORT",
                "STATUS",
            ],
            &rows,
        )?;
    }
    Ok(())
}

/// One result row: timings, exit code, summed CPU and the peak resident set
/// of its commands, and the called workflow and child run of a call.
fn result_row(result: &Value) -> Vec<String> {
    let commands: &[Value] = result["commands"].as_array().map_or(&[], Vec::as_slice);
    let cpu: Vec<u64> = commands
        .iter()
        .flat_map(|c| [&c["cpu_user_ms"], &c["cpu_system_ms"]])
        .filter_map(Value::as_u64)
        .collect();
    let cpu = if cpu.is_empty() {
        ABSENT.to_owned()
    } else {
        duration(cpu.iter().fold(0u64, |sum, ms| sum.saturating_add(*ms)))
    };
    let rss = commands
        .iter()
        .filter_map(|c| c["peak_rss_bytes"].as_u64())
        .max()
        .map_or_else(|| ABSENT.to_owned(), bytes);
    let call = match result["workflow"].as_str() {
        Some(workflow) => match result["child_run_id"].as_str() {
            Some(child) => format!("{} run {}", visible(workflow), visible(child)),
            None => format!("{} (not started)", visible(workflow)),
        },
        None => String::new(),
    };
    vec![
        visible(&instance(result)).into_owned(),
        field(&result["status"]),
        millis(&result["duration_ms"]),
        millis(&result["blocked_ms"]),
        millis(&result["queued_ms"]),
        scalar(&result["exit_code"]),
        cpu,
        rss,
        call,
    ]
}

/// A graph job or step: its command (or `-`), the condition it runs under,
/// and for a call job its target.
fn graph_line(out: &mut impl Write, indent: &str, name: &str, item: &Value) -> Result<()> {
    write!(out, "{indent}{name}: ")?;
    match item["call"]["workflow"].as_str() {
        Some(workflow) => write!(out, "calls {}", visible(workflow))?,
        None => write!(out, "{}", field(&item["command"]))?,
    }
    let condition = &item["condition"];
    if condition.is_object() {
        write!(
            out,
            "  if {} {}",
            field(&condition["kind"]),
            field(&condition["text"])
        )?;
    }
    writeln!(out)?;
    Ok(())
}
