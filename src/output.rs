mod history;
mod report;

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

struct Request {
    event: Value,
    done: oneshot::Sender<std::result::Result<(), String>>,
}

/// The owning invocation of a run that calls workflows, and for a child
/// invocation its parent run and caller job.
#[derive(Clone)]
struct Invocation {
    id: String,
    parent: Option<(String, String)>,
}

#[derive(Clone)]
pub struct Output {
    pub json: bool,
    workflow: Option<String>,
    environment: Option<String>,
    invocation: Option<Invocation>,
    sender: mpsc::Sender<Request>,
    cancel: watch::Receiver<Option<i32>>,
    suppressed: Arc<AtomicBool>,
}

impl Output {
    pub fn new(json: bool, cancel: watch::Receiver<Option<i32>>) -> Result<Self> {
        Self::writing(json, cancel, duplicate_stdio(1)?, duplicate_stdio(2)?)
    }

    /// Output that renders every event and discards it, for unit tests that
    /// drive real runs.
    #[cfg(test)]
    pub fn discard(json: bool, cancel: watch::Receiver<Option<i32>>) -> Result<Self> {
        let null = || File::options().write(true).open("/dev/null");
        Self::writing(json, cancel, null()?, null()?)
    }

    fn writing(
        json: bool,
        cancel: watch::Receiver<Option<i32>>,
        mut out: File,
        mut err: File,
    ) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Request>(32);
        // Blocking terminal/pipe writes must never occupy Tokio workers. This thread
        // is detached so a stopped downstream reader cannot prevent signal shutdown.
        std::thread::Builder::new()
            .name("goodflows-output".into())
            .spawn(move || {
                while let Some(request) = receiver.blocking_recv() {
                    let result =
                        render(json, request.event, &mut out, &mut err).map_err(|e| e.to_string());
                    let failed = result.is_err();
                    let _ = request.done.send(result);
                    if failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            json,
            workflow: None,
            environment: None,
            invocation: None,
            sender,
            cancel,
            suppressed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_workflow(&self, name: &str) -> Self {
        let mut output = self.clone();
        output.workflow = Some(name.into());
        output
    }

    /// Tag every event with its owning invocation. Used only when the run
    /// calls workflows, so runs without calls keep their event shapes.
    pub fn with_invocation(&self, id: &str, parent: Option<(&str, &str)>) -> Self {
        let mut output = self.clone();
        output.invocation = Some(Invocation {
            id: id.into(),
            parent: parent.map(|(run_id, job)| (run_id.into(), job.into())),
        });
        output
    }

    /// A copy that adds no workflow, environment or invocation context, for
    /// events that already carry their own (a remote run's native events,
    /// including those of called workflows).
    pub fn relay(&self) -> Self {
        let mut output = self.clone();
        output.workflow = None;
        output.environment = None;
        output.invocation = None;
        output
    }

    pub fn with_environment(&self, name: &str) -> Self {
        let mut output = self.clone();
        output.environment = Some(name.into());
        output
    }

    /// Emits the run `summary`. Text mode alone also receives the explicit
    /// `--config` path and any Sprite identity for the rerun hint and whether
    /// a signal arrived; none of them is part of the JSON event.
    pub async fn emit_summary(
        &self,
        mut event: Value,
        config: Option<&str>,
        sprite: Option<&str>,
        interrupted: bool,
    ) -> Result<()> {
        if !self.json {
            if let Some(config) = config {
                event[report::CONFIG] = json!(config);
            }
            if let Some(sprite) = sprite {
                event[report::SPRITE] = json!(sprite);
            }
            event[report::INTERRUPTED] = json!(interrupted);
        }
        self.emit(event).await
    }

    pub async fn emit(&self, mut event: Value) -> Result<()> {
        if let Some(workflow) = &self.workflow {
            event["workflow"] = json!(workflow);
        }
        if let Some(environment) = &self.environment {
            event["environment"] = json!(environment);
        }
        if let Some(invocation) = &self.invocation {
            event["invocation_id"] = json!(invocation.id);
            if let Some((run_id, job)) = &invocation.parent {
                event["parent_run_id"] = json!(run_id);
                event["caller_job"] = json!(job);
            }
        }
        if self.suppressed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (done, ack) = oneshot::channel();
        let write = async {
            self.sender
                .send(Request { event, done })
                .await
                .context("output writer stopped")?;
            ack.await
                .context("output writer stopped")?
                .map_err(anyhow::Error::msg)
        };
        tokio::pin!(write);
        let mut cancel = self.cancel.clone();
        if cancel.borrow().is_none() {
            tokio::select! {
                biased;
                result = &mut write => return result,
                _ = cancel.changed() => {}
            }
        }
        // Still deliver the final summary when the consumer is responsive. A full
        // pipe must not block shutdown forever after an interrupt.
        match tokio::time::timeout(Duration::from_millis(250), &mut write).await {
            Ok(result) => result,
            Err(_) => {
                self.suppressed.store(true, Ordering::Relaxed);
                Ok(())
            }
        }
    }
}

/// POSIX shell quoting as used by rerun hints.
pub(crate) fn shell_word(value: &str) -> String {
    report::shell_quote(value)
}

#[cfg(unix)]
fn duplicate_stdio(fd: i32) -> io::Result<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: fcntl validates the supplied descriptor; the returned descriptor is
    // owned here and CLOEXEC prevents jobs inheriting the CLI's output descriptors.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(duplicated) })
}

#[cfg(not(unix))]
compile_error!("goodflows currently supports Linux and macOS (Unix process groups required)");

fn render(json: bool, mut event: Value, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
    event["schema_version"] = json!(1);
    if json {
        serde_json::to_writer(&mut *out, &event)?;
        writeln!(out)?;
    } else {
        if matches!(
            event["type"].as_str(),
            Some("run_started" | "jobs" | "workflows" | "config_valid")
        ) {
            if let Some(environment) = event["environment"].as_str() {
                writeln!(out, "Environment: {environment}")?;
            }
        }
        match event["type"].as_str().unwrap_or_default() {
            "run_started" => writeln!(
                out,
                "goodflows  parallelism={}  fail_fast={}  jobs={}\nworkflow: {}{}\nworkdir: {}",
                event["parallelism"],
                event["fail_fast"],
                event["jobs"],
                invocation_label(&event),
                caller(&event),
                text(&event, "workdir")
            )?,
            "job_started" => writeln!(out, "→ {}", label(&event, &event).cyan())?,
            "step_started" => writeln!(
                out,
                "  → {}/{}",
                label(&event, &event),
                text(&event, "step")
            )?,
            "step_finished" => {
                let result = &event["result"];
                writeln!(
                    out,
                    "  {}/{} {}  {}ms",
                    label(&event, &event),
                    text(result, "id"),
                    text(result, "status").to_uppercase(),
                    result["duration_ms"]
                )?;
                if let Some(error) = result["error"].as_str() {
                    writeln!(out, "    {}", visible(error))?;
                }
            }
            "job_output" => {
                let data = text(&event, "data");
                let name = match event["step"].as_str() {
                    Some(step) => format!("{}/{step}", label(&event, &event)),
                    None => label(&event, &event),
                };
                write!(out, "[{} {}] {}", name, text(&event, "stream"), data)?;
                if !data.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            "job_finished" => {
                let result = &event["result"];
                let status = match text(result, "status") {
                    "passed" => "PASSED".green(),
                    "skipped" => "SKIPPED".yellow(),
                    "cancelled" => "CANCELLED".yellow(),
                    "timed_out" => "TIMED OUT".red(),
                    _ => "FAILED".red(),
                };
                write!(
                    out,
                    "{} {}  {}ms",
                    label(&event, result),
                    status,
                    result["duration_ms"]
                )?;
                if let Some(code) = result["exit_code"].as_i64() {
                    write!(out, " exit={code}")?;
                }
                writeln!(out)?;
                if let Some(error) = result["error"].as_str() {
                    writeln!(out, "  {}", visible(error))?;
                }
                // The command is in the run report's Failures section; output
                // tails are JSON-only, since the output already streamed live.
            }
            "summary" => report::report(&event, out)?,
            "history_runs" => history::runs(&event, out)?,
            "history_run" => history::run(&event, out)?,
            "config_valid" => {
                if let Some(workflows) = event["workflows"].as_array() {
                    writeln!(
                        out,
                        "Config valid ({} workflows / {} defined jobs)",
                        workflows.len(),
                        event["total_defined_jobs"]
                    )?;
                    for workflow in workflows {
                        writeln!(
                            out,
                            "  {}: {} jobs",
                            text(workflow, "name"),
                            workflow["jobs"]
                        )?;
                    }
                    out.flush()?;
                    return Ok(());
                }
                writeln!(
                    out,
                    "Config valid ({} selected / {} defined jobs)",
                    event["jobs"], event["total_defined_jobs"]
                )?;
                let requested = names(&event["requested_jobs"]);
                writeln!(
                    out,
                    "  requested: {}",
                    if requested.is_empty() {
                        "(all)"
                    } else {
                        &requested
                    }
                )?;
                writeln!(out, "  selected: {}", names(&event["selected_jobs"]))?;
                for call in event["calls"].as_array().into_iter().flatten() {
                    writeln!(
                        out,
                        "  call: {} -> workflow {}",
                        visible(text(call, "job")),
                        visible(text(call, "workflow"))
                    )?;
                }
            }
            "jobs" => {
                writeln!(out, "Workflow: {}", text(&event, "workflow"))?;
                let settings = &event["settings"];
                writeln!(
                    out,
                    "Settings: fail_fast={} parallelism={} env={}\nworkdir: {}",
                    settings["fail_fast"],
                    settings["parallelism"],
                    settings["env"],
                    text(settings, "workdir")
                )?;
                history_settings(settings, "", out)?;
                if let Some(reporting) = event.get("reporting") {
                    writeln!(
                        out,
                        "Reporting: project={} url_env={} token_env={}\nspool_dir: {}",
                        text(reporting, "project"),
                        text(reporting, "url_env"),
                        text(reporting, "token_env"),
                        text(reporting, "spool_dir")
                    )?;
                }
                writeln!(out, "Jobs")?;
                for job in event["jobs"].as_array().into_iter().flatten() {
                    let deps: Vec<_> = job["depends_on"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    writeln!(
                        out,
                        "  {}  depends_on: {}\n    {}",
                        text(job, "name"),
                        if deps.is_empty() {
                            "none".to_owned()
                        } else {
                            deps.join(", ")
                        },
                        match job["workflow"].as_str() {
                            Some(target) => format!("workflow: {}", visible(target)),
                            None => text(job, "command").to_owned(),
                        }
                    )?;
                    let timeout = job["timeout_secs"]
                        .as_u64()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "none".into());
                    writeln!(
                        out,
                        "    timeout_secs={} shell={} env={}",
                        timeout, job["shell"], job["env"]
                    )?;
                    if let Some(condition) = job["condition"].as_str() {
                        writeln!(out, "    condition: {condition}")?;
                    } else if !job["condition"].is_null() {
                        writeln!(out, "    condition: {}", job["condition"])?;
                    }
                    if let Some(outputs) = job.get("outputs") {
                        writeln!(out, "    outputs: {outputs}")?;
                    }
                    if let Some(partitions) = job.get("partitions") {
                        writeln!(
                            out,
                            "    partitions: {} (strategy={})",
                            partitions["count"],
                            text(partitions, "strategy")
                        )?;
                    }
                    if job.get("reports").is_some() {
                        writeln!(out, "    reports: {}", names(&job["reports"]))?;
                    }
                    for step in job["steps"].as_array().into_iter().flatten() {
                        let shell = step["shell"]
                            .as_bool()
                            .unwrap_or(job["shell"].as_bool().unwrap_or(true));
                        writeln!(
                            out,
                            "    step {}: {}\n      shell={} env={} condition={}",
                            text(step, "id"),
                            text(step, "command"),
                            shell,
                            step["env"],
                            step["condition"]
                        )?;
                    }
                }
            }
            "config_written" => writeln!(
                out,
                "Wrote {} ({} template)",
                text(&event, "path"),
                text(&event, "template")
            )?,
            "init_cancelled" => writeln!(out, "Kept existing {}", text(&event, "path"))?,
            "workflows" => {
                writeln!(
                    out,
                    "Workflows (default: {})",
                    event["default_workflow"].as_str().unwrap_or("none")
                )?;
                for workflow in event["workflows"].as_array().into_iter().flatten() {
                    writeln!(
                        out,
                        "  {}: {} jobs  parallelism={} fail_fast={} env={}\n    workdir: {}",
                        text(workflow, "name"),
                        workflow["jobs"],
                        workflow["settings"]["parallelism"],
                        workflow["settings"]["fail_fast"],
                        workflow["settings"]["env"],
                        text(&workflow["settings"], "workdir")
                    )?;
                    history_settings(&workflow["settings"], "    ", out)?;
                }
            }
            "reports_collected" => {
                write!(
                    out,
                    "reports {}: {} collected ({})",
                    instance_of(&event, "job"),
                    event["reports"],
                    text(&event, "outcome").replace('_', " ")
                )?;
                if let Some(rejected) = event["rejected"].as_object().filter(|r| !r.is_empty()) {
                    let codes: Vec<_> = rejected
                        .iter()
                        .map(|(code, n)| format!("{code}={n}"))
                        .collect();
                    write!(out, "  rejected: {}", codes.join(", "))?;
                }
                writeln!(out)?;
                if event["reports"].as_u64().is_some_and(|n| n > 0) {
                    writeln!(out, "  saved in {}", text(&event, "run_dir"))?;
                }
            }
            "report_uploaded" => writeln!(
                out,
                "report {}/{} uploaded{}",
                instance_of(&event, "job"),
                text(&event, "report_id"),
                if event["duplicate"] == true {
                    " (already stored)"
                } else {
                    ""
                }
            )?,
            "report_upload_pending" => {
                write!(
                    out,
                    "report {}/{} pending: {}",
                    instance_of(&event, "job"),
                    text(&event, "report_id"),
                    text(&event, "reason").replace('_', " ")
                )?;
                if let Some(status) = event["http_status"].as_u64() {
                    write!(out, " (HTTP {status})")?;
                }
                if event["terminal"] == true {
                    write!(out, ", not retried by reports upload")?;
                }
                writeln!(out)?;
            }
            "reporting_skipped" => {
                write!(
                    out,
                    "reporting skipped: {}",
                    text(&event, "reason").replace('_', " ")
                )?;
                if let Some(pending) = event["pending"].as_u64() {
                    write!(out, " ({pending} reports kept locally)")?;
                }
                writeln!(out)?;
            }
            "run_uploaded" => writeln!(
                out,
                "{} uploaded{}",
                run_history(&event),
                if event["duplicate"] == true {
                    " (already stored)"
                } else {
                    ""
                }
            )?,
            "run_upload_pending" => {
                write!(
                    out,
                    "{} upload pending: {}",
                    run_history(&event),
                    text(&event, "reason").replace('_', " ")
                )?;
                if let Some(status) = event["http_status"].as_u64() {
                    write!(out, " (HTTP {status})")?;
                }
                if event["terminal"] == true {
                    write!(out, ", not retried by reports upload")?;
                }
                writeln!(out)?;
            }
            "run_upload_skipped" => {
                write!(
                    out,
                    "{} upload skipped: {}",
                    run_history(&event),
                    text(&event, "reason").replace('_', " ")
                )?;
                if let Some(path) = event["path"].as_str() {
                    write!(out, " at {}", visible(path))?;
                }
                writeln!(out)?;
            }
            "reports_upload_summary" => {
                write!(
                    out,
                    "\nReports: {} acknowledged, {} pending, {} rejected, {} invalid",
                    event["acknowledged"], event["pending"], event["rejected"], event["invalid"]
                )?;
                if let Some(state) = event["run_record"].as_str() {
                    write!(out, "\nRun history: {state}")?;
                }
                writeln!(out)?;
            }
            "error" => {
                writeln!(err, "Error: {}", text(&event, "message"))?;
                if let Some(hint) = event["hint"].as_str() {
                    writeln!(err, "Hint: {hint}")?;
                }
            }
            "sprite_hook_output" => {
                let data = text(&event, "data");
                write!(
                    out,
                    "[{} {}] {}",
                    text(&event, "hook"),
                    text(&event, "stream"),
                    data
                )?;
                if !data.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            kind if kind.starts_with("sprite_") => {
                if let Some(message) = event["message"].as_str() {
                    writeln!(out, "{}", visible(message))?;
                }
            }
            _ => {}
        }
    }
    out.flush()?;
    Ok(())
}

/// The effective history and log retention settings, one line each.
fn history_settings(settings: &Value, indent: &str, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "{indent}History: history={} history_runs={} history_bytes={} log_runs={}\n{indent}history_dir: {}",
        settings["history"],
        settings["history_runs"],
        settings["history_bytes"],
        settings["log_runs"],
        text(settings, "history_dir")
    )?;
    Ok(())
}

fn names(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A short distinguisher for an invocation id: its first 8 hex digits.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The event's workflow, with a short invocation distinguisher when the run
/// calls workflows.
fn invocation_label(event: &Value) -> String {
    let workflow = visible(text(event, "workflow")).into_owned();
    match event["invocation_id"].as_str() {
        Some(id) => format!("{workflow} [{}]", visible(short_id(id))),
        None => workflow,
    }
}

/// `run history for <workflow>` for a run record event, naming the
/// invocation when the run calls workflows.
fn run_history(event: &Value) -> String {
    match event["workflow"].as_str() {
        Some(_) => format!("run history for {}", invocation_label(event)),
        None => "run history".into(),
    }
}

/// `  called by <job> in run <id>` for a child invocation's event.
fn caller(event: &Value) -> String {
    match (
        event["caller_job"].as_str(),
        event["parent_run_id"].as_str(),
    ) {
        (Some(job), Some(run)) => format!(
            "  called by {} in run {}",
            visible(job),
            visible(short_id(run))
        ),
        _ => String::new(),
    }
}

/// A live line's job label: the [`instance`] of `value`, prefixed with the
/// owning workflow and invocation when the run calls workflows, so the same
/// job in two invocations stays distinguishable.
fn label(event: &Value, value: &Value) -> String {
    match event["invocation_id"].as_str() {
        Some(_) => format!("{} {}", invocation_label(event), instance(value)),
        None => instance(value),
    }
}

/// A job's display name: `name`, or `name (i/N)` for a partition instance.
fn instance(value: &Value) -> String {
    instance_of(value, "name")
}

/// [`instance`] for events that name the job under `key`.
fn instance_of(value: &Value, key: &str) -> String {
    let name = text(value, key);
    match (
        value["partition"]["index"].as_u64(),
        value["partition"]["count"].as_u64(),
    ) {
        (Some(index), Some(count)) => format!("{name} ({index}/{count})"),
        _ => name.to_owned(),
    }
}

fn text<'a>(event: &'a Value, key: &str) -> &'a str {
    event[key].as_str().unwrap_or_default()
}

/// Escapes control characters other than tab (for example `\u{1b}` or `\r`)
/// so job-provided text cannot move the cursor or restyle the terminal.
fn visible(text: &str) -> Cow<'_, str> {
    let control = |c: char| c.is_control() && c != '\t';
    if !text.chars().any(control) {
        return Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if control(c) {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(event: Value) -> String {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        render(false, event, &mut out, &mut err).unwrap();
        assert!(err.is_empty());
        String::from_utf8(out).unwrap()
    }

    /// Text for reporting events with and without a partition instance.
    #[test]
    fn reporting_events_name_the_instance_in_text() {
        let partition = json!({"index": 2, "count": 3});
        for (tag, job) in [(None, "test"), (Some(partition), "test (2/3)")] {
            let with = |mut event: Value| {
                if let Some(partition) = &tag {
                    event["partition"] = partition.clone();
                }
                rendered(event)
            };
            assert_eq!(
                with(json!({"type":"reports_collected", "job":"test", "outcome":"collected", "reports":1, "run_dir":"/spool/run", "rejected":{"duplicate_report_id":1}})),
                format!("reports {job}: 1 collected (collected)  rejected: duplicate_report_id=1\n  saved in /spool/run\n")
            );
            assert_eq!(
                with(
                    json!({"type":"report_uploaded", "job":"test", "report_id":"r1", "duplicate":true})
                ),
                format!("report {job}/r1 uploaded (already stored)\n")
            );
            assert_eq!(
                with(
                    json!({"type":"report_upload_pending", "job":"test", "report_id":"r1", "reason":"state_write_failed", "http_status":null})
                ),
                format!("report {job}/r1 pending: state write failed\n")
            );
        }
    }

    #[test]
    fn visible_escapes_control_characters_except_tab() {
        assert!(matches!(
            visible("plain\ttext é"),
            Cow::Borrowed("plain\ttext é")
        ));
        assert_eq!(
            visible("a\x1b[2Jb\rc\nd\0\u{9b}"),
            "a\\u{1b}[2Jb\\rc\\nd\\u{0}\\u{9b}"
        );
    }

    #[test]
    fn live_failure_errors_are_escaped() {
        colored::control::set_override(false);
        let text = rendered(
            json!({"type": "job_finished", "result": {"name": "a", "status": "failed",
            "duration_ms": 1, "exit_code": 1, "error": "bad\x1b[2J\rworse"}}),
        );
        assert_eq!(text, "a FAILED  1ms exit=1\n  bad\\u{1b}[2J\\rworse\n");
        let text = rendered(
            json!({"type": "step_finished", "name": "a", "result": {"id": "s",
            "status": "failed", "duration_ms": 1, "error": "x\x07"}}),
        );
        assert_eq!(text, "  a/s FAILED  1ms\n    x\\u{7}\n");
    }
}
