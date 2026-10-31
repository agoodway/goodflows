use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Value};
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

#[derive(Clone)]
pub struct Output {
    pub json: bool,
    workflow: Option<String>,
    environment: Option<String>,
    sender: mpsc::Sender<Request>,
    cancel: watch::Receiver<Option<i32>>,
    suppressed: Arc<AtomicBool>,
}

impl Output {
    pub fn new(json: bool, cancel: watch::Receiver<Option<i32>>) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Request>(32);
        let mut out = duplicate_stdio(1)?;
        let mut err = duplicate_stdio(2)?;
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

    pub fn with_environment(&self, name: &str) -> Self {
        let mut output = self.clone();
        output.environment = Some(name.into());
        output
    }

    pub async fn emit(&self, mut event: Value) -> Result<()> {
        if let Some(workflow) = &self.workflow {
            event["workflow"] = json!(workflow);
        }
        if let Some(environment) = &self.environment {
            event["environment"] = json!(environment);
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
                "goodflows  parallelism={}  fail_fast={}  jobs={}\nworkflow: {}\nworkdir: {}",
                event["parallelism"],
                event["fail_fast"],
                event["jobs"],
                text(&event, "workflow"),
                text(&event, "workdir")
            )?,
            "job_started" => writeln!(out, "→ {}", text(&event, "name").cyan())?,
            "job_output" => {
                let data = text(&event, "data");
                write!(
                    out,
                    "[{} {}] {}",
                    text(&event, "name"),
                    text(&event, "stream"),
                    data
                )?;
                if !data.ends_with('\n') {
                    writeln!(out)?;
                }
            }
            "job_finished" => {
                let result = &event["result"];
                let label = match text(result, "status") {
                    "passed" => "PASSED".green(),
                    "skipped" => "SKIPPED".yellow(),
                    "cancelled" => "CANCELLED".yellow(),
                    "timed_out" => "TIMED OUT".red(),
                    _ => "FAILED".red(),
                };
                write!(
                    out,
                    "{} {}  {}ms",
                    text(result, "name"),
                    label,
                    result["duration_ms"]
                )?;
                if let Some(code) = result["exit_code"].as_i64() {
                    write!(out, " exit={code}")?;
                }
                writeln!(out)?;
                if let Some(error) = result["error"].as_str() {
                    writeln!(out, "  {error}")?;
                }
                if matches!(text(result, "status"), "failed" | "timed_out") {
                    writeln!(out, "  command: {}", text(result, "command"))?;
                    for stream in ["stdout", "stderr"] {
                        let tail = text(result, &format!("{stream}_tail"));
                        if !tail.is_empty() {
                            let truncated = if result[format!("{stream}_truncated")] == true {
                                " (truncated)"
                            } else {
                                ""
                            };
                            writeln!(out, "  {stream} tail{truncated}:")?;
                            for line in tail.lines() {
                                writeln!(out, "    {line}")?;
                            }
                        }
                    }
                }
            }
            "summary" => {
                writeln!(
                    out,
                    "\nSummary: {} passed, {} failed, {} timed out, {} skipped, {} cancelled",
                    event["passed"],
                    event["failed"],
                    event["timed_out"],
                    event["skipped"],
                    event["cancelled"]
                )?;
                if event["success"] == true {
                    writeln!(out, "All jobs passed")?;
                }
            }
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
            }
            "jobs" => {
                writeln!(out, "Workflow: {}", text(&event, "workflow"))?;
                let settings = &event["settings"];
                writeln!(
                    out,
                    "Settings: fail_fast={} parallelism={}\nworkdir: {}",
                    settings["fail_fast"],
                    settings["parallelism"],
                    text(settings, "workdir")
                )?;
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
                        text(job, "command")
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
                        "  {}: {} jobs  parallelism={} fail_fast={}\n    workdir: {}",
                        text(workflow, "name"),
                        workflow["jobs"],
                        workflow["settings"]["parallelism"],
                        workflow["settings"]["fail_fast"],
                        text(&workflow["settings"], "workdir")
                    )?;
                }
            }
            "error" => {
                writeln!(err, "Error: {}", text(&event, "message"))?;
            }
            _ => {}
        }
    }
    out.flush()?;
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

fn text<'a>(event: &'a Value, key: &str) -> &'a str {
    event[key].as_str().unwrap_or_default()
}
