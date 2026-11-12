//! Detached runs: `goodflows run --detach` starts the run in a background
//! process and prints its run id; `goodflows status` and `goodflows wait`
//! read it back. Each detached run owns `.goodflows/runs/<run_id>/` beside
//! the config file:
//!
//! - `run.json`: what the parent started (workflow, environment, pid).
//! - `lock`: held by the background run for its whole life, so a run that
//!   died without a result is told apart from one still running.
//! - `pid`: written once the background run holds the lock; the parent
//!   prints the run id only after it exists.
//! - `events.jsonl`: the run's JSON Lines events, for progress.
//! - `stderr.log`: the run's stderr.
//! - `last.json`: the run's own last-run record, which `wait` replays.
//! - `exit_code`: the run's exit code, written last.
//!
//! The background run is the same command line with `--detached-run <dir>`
//! added and `--json` for every event, in a new session, so it outlives the
//! shell call and the process group that started it.

use crate::config::ConfigError;
use crate::output::{self, Output};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `wait`'s exit code when the timeout passed and the run is still going:
/// EX_TEMPFAIL from sysexits.h, "try again later". 0, 1, 2, 130 and 143
/// keep their meaning, and 3 is a Sprite run's unresolved remote execution.
pub const STILL_RUNNING: u8 = 75;

/// How long `run --detach` waits for the background run to start.
const START_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);

/// `.goodflows/runs/<run_id>` beside the config file.
pub fn dir(config_file: &Path, run_id: &str) -> PathBuf {
    config_file
        .parent()
        .unwrap_or(Path::new("."))
        .join(".goodflows/runs")
        .join(run_id)
}

/// The record a detached run's [`crate::last::Recorder`] also writes.
pub fn last_record(dir: &Path) -> PathBuf {
    dir.join("last.json")
}

/// The run id a background run's directory names.
pub fn run_id(dir: &Path) -> Result<String> {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| crate::fsutil::canonical_uuid(name))
        .map(str::to_owned)
        .with_context(|| format!("{} does not name a detached run", dir.display()))
}

#[derive(Serialize, Deserialize)]
struct Meta {
    run_id: String,
    workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    pid: u32,
    started_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// A new file private to the invoking user: events and output are not
/// redacted.
fn create(path: &Path) -> Result<std::fs::File> {
    std::fs::File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn flock(file: &std::fs::File, operation: i32) -> std::io::Result<()> {
    // SAFETY: flock on a descriptor `file` owns for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `run --detach`: start the already validated run in the background and
/// report its id. `workflow` and `environment` are the resolved selection.
pub async fn start(
    config_file: &Path,
    workflow: &str,
    environment: Option<&str>,
    config: Option<&Path>,
    output: &Output,
) -> Result<u8> {
    let run_id = uuid::Uuid::new_v4().hyphenated().to_string();
    let dir = dir(config_file, &run_id);
    crate::fsutil::create_state_dir(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let file = |name: &str| create(&dir.join(name));
    // Every event, for `status`, whatever `--json` the caller asked for.
    // `--json` takes its value only after `=`, so no other argument goes.
    let mut args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != "--json" && !arg.to_string_lossy().starts_with("--json="))
        .collect();
    args.extend(["--json".into(), "--detached-run".into(), dir.clone().into()]);
    let mut command = Command::new(std::env::current_exe().context("locate goodflows")?);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(file("events.jsonl")?)
        .stderr(file("stderr.log")?);
    // SAFETY: setsid is async-signal-safe. A new session keeps the run alive
    // when the caller's process group, session or terminal goes away.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start the detached run")?;
    let pid_file = dir.join("pid");
    let deadline = Instant::now() + START_WAIT;
    while !pid_file.exists() {
        if let Some(status) = child.try_wait()? {
            let stderr = std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default();
            anyhow::bail!(
                "the detached run exited before it started ({status}): {}",
                stderr.trim()
            );
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            anyhow::bail!("the detached run did not start within {START_WAIT:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let meta = Meta {
        run_id: run_id.clone(),
        workflow: workflow.into(),
        environment: environment.map(Into::into),
        pid: child.id(),
        started_at_ms: now_ms(),
    };
    let mut bytes = serde_json::to_vec(&meta)?;
    bytes.push(b'\n');
    crate::fsutil::write_atomic(&dir.join("run.json"), &bytes, true)?;
    if output.json {
        output
            .emit(json!({"type":"run_detached", "run_id":run_id, "pid":meta.pid, "run_dir":dir.to_string_lossy()}))
            .await?;
    } else {
        let config = config
            .map(|path| format!(" --config {}", output::shell_word(&path.to_string_lossy())))
            .unwrap_or_default();
        println!("{run_id}");
        eprintln!(
            "goodflows: started detached run {run_id}; check it with `goodflows status {run_id}{config}`, or wait for its report with `goodflows wait {run_id}{config} --timeout 90`"
        );
    }
    Ok(0)
}

/// The background side of a detached run, from `--detached-run <dir>`: holds
/// the run's lock until the process exits.
pub struct Background {
    dir: PathBuf,
    _lock: std::fs::File,
}

impl Background {
    /// Take the run's lock, then write `pid`, which tells the parent the
    /// run started.
    pub fn start(dir: &Path) -> Result<Self> {
        let path = dir.join("lock");
        let lock = create(&path)?;
        flock(&lock, libc::LOCK_EX | libc::LOCK_NB)
            .with_context(|| format!("lock {}", path.display()))?;
        crate::fsutil::write_atomic(
            &dir.join("pid"),
            format!("{}\n", std::process::id()).as_bytes(),
            true,
        )?;
        Ok(Self {
            dir: dir.into(),
            _lock: lock,
        })
    }

    /// Record the exit code, the run's last effect.
    pub fn finish(self, code: u8) {
        let path = self.dir.join("exit_code");
        if let Err(error) = crate::fsutil::write_atomic(&path, format!("{code}\n").as_bytes(), true)
        {
            eprintln!(
                "goodflows: detached run: write {}: {error:#}",
                path.display()
            );
        }
    }
}

/// Where a detached run is.
enum State {
    Running,
    Finished(u8),
    /// The background process is gone and left no exit code.
    Lost,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Running => "running",
            State::Finished(_) => "finished",
            State::Lost => "lost",
        }
    }
}

/// The directory of an existing detached run of `config_file`.
fn existing(config_file: &Path, run_id: &str) -> Result<PathBuf> {
    let dir = dir(config_file, run_id);
    if dir.join("pid").is_file() {
        return Ok(dir);
    }
    let mut error = ConfigError::new(
        "unknown_run",
        format!(
            "no detached run {run_id} of {} (looked in {})",
            config_file.display(),
            dir.display()
        ),
    );
    error.hint = Some(
        "start one with `goodflows run --detach`; status and wait need the same config".into(),
    );
    Err(error.into())
}

fn exit_code(dir: &Path) -> Result<Option<u8>> {
    let path = dir.join("exit_code");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(
            text.trim()
                .parse()
                .with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn state(dir: &Path) -> Result<State> {
    if let Some(code) = exit_code(dir)? {
        return Ok(State::Finished(code));
    }
    let path = dir.join("lock");
    let lock = std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
    match flock(&lock, libc::LOCK_SH | libc::LOCK_NB) {
        Err(error) if error.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(State::Running),
        Err(error) => Err(error).with_context(|| format!("lock {}", path.display())),
        // The lock is free: the run exited, and wrote its exit code first
        // unless it died.
        Ok(()) => Ok(match exit_code(dir)? {
            Some(code) => State::Finished(code),
            None => State::Lost,
        }),
    }
}

/// The root's job instance count, and each status with its job instances.
type Progress = (Option<u64>, BTreeMap<String, Vec<String>>);

/// Every job instance's latest status from the run's events: `running`
/// after `job_started`, then the `job_finished` status.
fn progress(dir: &Path, run_id: &str) -> Result<Progress> {
    let path = dir.join("events.jsonl");
    let text = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut total = None;
    // Instances in the order they started, by invocation id and label, with
    // their latest status.
    let mut jobs: Vec<(String, String, String)> = Vec::new();
    // A partly written last line does not parse and is left out.
    for event in text
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
    {
        let root = event["invocation_id"]
            .as_str()
            .is_none_or(|id| id == run_id);
        let (label, status) = match event["type"].as_str() {
            Some("run_started") if root => {
                total = event["jobs"].as_u64();
                continue;
            }
            Some("job_started") => (label(&event, &event), "running"),
            Some("job_finished") => (
                label(&event, &event["result"]),
                event["result"]["status"].as_str().unwrap_or("failed"),
            ),
            _ => continue,
        };
        let invocation = event["invocation_id"].as_str().unwrap_or_default();
        match jobs
            .iter_mut()
            .find(|(id, name, _)| id == invocation && *name == label)
        {
            Some(job) => job.2 = status.into(),
            None => jobs.push((invocation.into(), label, status.into())),
        }
    }
    let mut by_status: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for status in [
        "running",
        "passed",
        "failed",
        "timed_out",
        "skipped",
        "cancelled",
    ] {
        by_status.insert(status.into(), Vec::new());
    }
    for (_, label, status) in jobs {
        by_status.entry(status).or_default().push(label);
    }
    Ok((total, by_status))
}

/// `name`, or `name (i/N)` for a partition, prefixed with its workflow and
/// short invocation id for a job of a called workflow.
fn label(event: &Value, value: &Value) -> String {
    let mut name = value["name"].as_str().unwrap_or_default().to_owned();
    if let (Some(index), Some(count)) = (
        value["partition"]["index"].as_u64(),
        value["partition"]["count"].as_u64(),
    ) {
        name = format!("{name} ({index}/{count})");
    }
    // A called workflow's jobs, like the text output's live lines: two calls
    // of one workflow differ by their invocation.
    match (
        event["parent_run_id"].as_str(),
        event["invocation_id"].as_str(),
    ) {
        (Some(_), Some(id)) => format!(
            "{} [{}] {name}",
            event["workflow"].as_str().unwrap_or_default(),
            id.get(..8).unwrap_or(id)
        ),
        _ => name,
    }
}

/// Report a run's state and progress.
async fn report(dir: &Path, run_id: &str, state: &State, output: &Output) -> Result<()> {
    let meta: Option<Meta> = std::fs::read(dir.join("run.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let (total, jobs) = progress(dir, run_id)?;
    // A finished run's time ends when it wrote its exit code.
    let end_ms = std::fs::metadata(dir.join("exit_code"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(now_ms, |d| d.as_millis() as u64);
    let elapsed_ms = meta
        .as_ref()
        .map(|meta| end_ms.saturating_sub(meta.started_at_ms));
    if output.json {
        let mut event = json!({"type":"run_status", "run_id":run_id, "state":state.name(), "jobs":total, "elapsed_ms":elapsed_ms});
        if let Some(meta) = &meta {
            event["workflow"] = json!(meta.workflow);
            if let Some(environment) = &meta.environment {
                event["environment"] = json!(environment);
            }
        }
        if let State::Finished(code) = state {
            event["exit_code"] = json!(code);
        }
        for (status, names) in &jobs {
            event[status] = json!(names);
        }
        return output.emit(event).await;
    }
    let mut text = format!("Run {run_id}: {}", state.name());
    if let State::Finished(code) = state {
        text += &format!(" (exit {code})");
    }
    if let Some(meta) = &meta {
        text += &format!("\nworkflow: {}", meta.workflow);
        if let Some(environment) = &meta.environment {
            text += &format!("  environment: {environment}");
        }
    }
    if let Some(ms) = elapsed_ms {
        text += &format!("\nelapsed: {}s", ms / 1000);
    }
    let count = |status: &str| jobs.get(status).map_or(0, Vec::len);
    let done: usize = jobs
        .iter()
        .filter(|(status, _)| *status != "running")
        .map(|(_, names)| names.len())
        .sum();
    text += &format!(
        "\njobs: {done} finished of {}  passed={} failed={} timed_out={} skipped={} cancelled={}",
        total.map_or("?".into(), |n| n.to_string()),
        count("passed"),
        count("failed"),
        count("timed_out"),
        count("skipped"),
        count("cancelled"),
    );
    for (status, heading) in [
        ("running", "Running"),
        ("failed", "Failed"),
        ("timed_out", "Timed out"),
    ] {
        if let Some(names) = jobs.get(status).filter(|names| !names.is_empty()) {
            text += &format!("\n{heading}: {}", names.join(", "));
        }
    }
    if let State::Lost = state {
        text += &format!(
            "\nThe run stopped without a result; see {}",
            dir.join("stderr.log").display()
        );
    }
    println!("{text}");
    Ok(())
}

/// `goodflows status <run_id>`.
pub async fn status(config_file: &Path, run_id: &str, output: &Output) -> Result<u8> {
    let dir = existing(config_file, run_id)?;
    let state = state(&dir)?;
    report(&dir, run_id, &state, output).await?;
    Ok(0)
}

/// `goodflows wait <run_id> [--timeout <secs>]`: once the run finished,
/// emit its summaries as the run did and exit with its exit code; when the
/// timeout passes first, report its progress and exit [`STILL_RUNNING`].
pub async fn wait(
    config_file: &Path,
    run_id: &str,
    timeout: Option<u64>,
    config: Option<&Path>,
    output: &Output,
) -> Result<u8> {
    let dir = existing(config_file, run_id)?;
    let deadline = timeout.map(|secs| Instant::now() + Duration::from_secs(secs));
    let code = loop {
        match state(&dir)? {
            State::Finished(code) => break code,
            State::Lost => {
                anyhow::bail!(
                    "detached run {run_id} stopped without a result; see {}",
                    dir.join("stderr.log").display()
                )
            }
            State::Running if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                report(&dir, run_id, &State::Running, output).await?;
                if !output.json {
                    let config = config
                        .map(|path| {
                            format!(" --config {}", output::shell_word(&path.to_string_lossy()))
                        })
                        .unwrap_or_default();
                    println!(
                        "Still running; wait again with `goodflows wait {run_id}{config} --timeout {}`",
                        timeout.unwrap_or_default()
                    );
                }
                return Ok(STILL_RUNNING);
            }
            State::Running => tokio::time::sleep(POLL).await,
        }
    };
    let record = last_record(&dir);
    if record.is_file() {
        crate::last::replay(&record, output).await?;
    } else {
        // The run stopped before its summary: show its errors instead.
        let path = dir.join("events.jsonl");
        let text = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        for event in text
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .filter(|event| event["type"] == "error")
        {
            output.emit(event).await?;
        }
    }
    Ok(code)
}
