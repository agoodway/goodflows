use crate::{config::Job, output::Output};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::JoinSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
    TimedOut,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobResult {
    pub name: String,
    pub status: Status,
    pub duration_ms: u128,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl JobResult {
    fn new(job: &Job, status: Status, error: Option<String>) -> Self {
        Self {
            name: job.name.clone(),
            status,
            duration_ms: 0,
            exit_code: None,
            error,
        }
    }
}

pub struct Options {
    pub parallelism: usize,
    pub fail_fast: bool,
    pub workdir: PathBuf,
    pub log_dir: Option<PathBuf>,
    pub output: Output,
}

async fn record(
    results: &mut [Option<JobResult>],
    index: usize,
    result: JobResult,
    output: &Output,
) -> Result<()> {
    output
        .emit(json!({"type": "job_finished", "result": result}))
        .await?;
    results[index] = Some(result);
    Ok(())
}

pub async fn execute(
    jobs: &[Job],
    options: &Options,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<Vec<JobResult>> {
    if let Some(dir) = &options.log_dir {
        tokio::fs::create_dir_all(dir)
            .await
            .with_context(|| format!("failed to create log directory {}", dir.display()))?;
    }
    let names: HashMap<_, _> = jobs
        .iter()
        .enumerate()
        .map(|(i, j)| (j.name.as_str(), i))
        .collect();
    let deps: Vec<Vec<usize>> = jobs
        .iter()
        .map(|j| j.depends_on.iter().map(|d| names[d.as_str()]).collect())
        .collect();
    let mut results: Vec<Option<JobResult>> = vec![None; jobs.len()];
    let mut started = vec![false; jobs.len()];
    let mut running = JoinSet::new();
    let mut failed = false;

    loop {
        // Consume completed jobs before deciding whether to launch queued work.
        while let Some(completed) = running.try_join_next() {
            let (index, result): (usize, JobResult) = completed?;
            failed |= matches!(result.status, Status::Failed | Status::TimedOut);
            record(&mut results, index, result, &options.output).await?;
        }
        let cancelled = cancel.borrow().is_some();
        // Repeat so transitive skips propagate even when configs are not topologically ordered.
        loop {
            let mut changed = false;
            for (index, job) in jobs.iter().enumerate() {
                if started[index] || results[index].is_some() {
                    continue;
                }
                let blocked = deps[index].iter().any(|&d| {
                    results[d]
                        .as_ref()
                        .is_some_and(|r| r.status != Status::Passed)
                });
                let reason = if cancelled {
                    Some((Status::Cancelled, "cancelled by signal"))
                } else if blocked {
                    Some((Status::Skipped, "dependency did not pass"))
                } else if options.fail_fast && failed {
                    Some((Status::Skipped, "fail_fast: previous job failed"))
                } else {
                    None
                };
                if let Some((status, error)) = reason {
                    record(
                        &mut results,
                        index,
                        JobResult::new(job, status, Some(error.into())),
                        &options.output,
                    )
                    .await?;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (index, job) in jobs.iter().enumerate() {
            if running.len() >= options.parallelism || cancel.borrow().is_some() {
                break;
            }
            if started[index] || results[index].is_some() {
                continue;
            }
            if !deps[index].iter().all(|&d| {
                results[d]
                    .as_ref()
                    .is_some_and(|r| r.status == Status::Passed)
            }) {
                continue;
            }
            started[index] = true;
            let job = job.clone();
            let workdir = options.workdir.clone();
            let log_dir = options.log_dir.clone();
            let output = options.output.clone();
            let cancel = cancel.clone();
            running.spawn(async move {
                let result =
                    run_job(index, &job, &workdir, log_dir.as_deref(), output, cancel).await;
                (index, result)
            });
        }
        if results.iter().all(Option::is_some) {
            break;
        }
        if running.is_empty() {
            // Cancellation can arrive during the launch loop; classify pending work next iteration.
            if cancel.borrow().is_some() {
                continue;
            }
            anyhow::bail!("no runnable jobs remain");
        }
        tokio::select! {
            biased;
            changed = cancel.changed(), if !cancelled => { changed.context("signal monitor stopped")?; }
            completed = running.join_next() => {
                let (index, result) = completed.context("job scheduler stopped")??;
                failed |= matches!(result.status, Status::Failed | Status::TimedOut);
                record(&mut results, index, result, &options.output).await?;
            }
        }
    }
    Ok(results.into_iter().flatten().collect())
}

async fn run_job(
    index: usize,
    job: &Job,
    workdir: &Path,
    log_dir: Option<&Path>,
    output: Output,
    cancel: watch::Receiver<Option<i32>>,
) -> JobResult {
    let start = Instant::now();
    let mut result = match run_process(index, job, workdir, log_dir, output, cancel).await {
        Ok(result) => result,
        Err(error) => JobResult::new(job, Status::Failed, Some(format!("{error:#}"))),
    };
    result.duration_ms = start.elapsed().as_millis();
    result
}

// A guard also cleans up if an I/O error aborts the scheduler or a task panics.
struct ProcessGroup {
    id: Option<u32>,
}

impl ProcessGroup {
    fn kill(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        #[cfg(unix)]
        // SAFETY: process_group(0) creates a private group whose ID is this child PID.
        // Negative PID targets that group, never the CLI's own process group.
        unsafe {
            libc::kill(-(id as i32), libc::SIGKILL);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn run_process(
    index: usize,
    job: &Job,
    workdir: &Path,
    log_dir: Option<&Path>,
    output: Output,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<JobResult> {
    if cancel.borrow().is_some() {
        return Ok(JobResult::new(
            job,
            Status::Cancelled,
            Some("cancelled by signal".into()),
        ));
    }
    let mut command = if job.shell {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&job.command);
        c
    } else {
        let args = shell_words::split(&job.command)?;
        let mut c = Command::new(&args[0]);
        c.args(&args[1..]);
        c
    };
    let safe_name: String = job
        .name
        .chars()
        .take(60)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = format!("{:03}-{safe_name}", index + 1);
    let stdout_log = open_log(log_dir, &format!("{stem}.stdout.log")).await?;
    let stderr_log = open_log(log_dir, &format!("{stem}.stderr.log")).await?;
    command
        .current_dir(workdir)
        .envs(&job.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    output
        .emit(json!({"type":"job_started", "name":job.name}))
        .await?;
    if cancel.borrow().is_some() {
        return Ok(JobResult::new(
            job,
            Status::Cancelled,
            Some("cancelled by signal".into()),
        ));
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn '{}'", job.name))?;
    let mut group = ProcessGroup {
        id: Some(child.id().context("child has no PID")?),
    };
    let mut readers = JoinSet::new();
    let name = job.name.clone();
    let pipe = child.stdout.take().context("missing stdout pipe")?;
    readers.spawn(drain(pipe, name, "stdout", stdout_log, output.clone()));
    let name = job.name.clone();
    let pipe = child.stderr.take().context("missing stderr pipe")?;
    readers.spawn(drain(pipe, name, "stderr", stderr_log, output.clone()));

    let deadline = async {
        if let Some(secs) = job.timeout_secs {
            tokio::time::sleep(Duration::from_secs(secs)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(deadline);
    let mut result = loop {
        tokio::select! {
            biased;
            _ = async {
                if cancel.borrow().is_none() { let _ = cancel.changed().await; }
            } => break JobResult::new(job, Status::Cancelled, Some("cancelled by signal".into())),
            status = child.wait() => {
                let status = status.context("failed to wait for child")?;
                let mut result = JobResult::new(job, if status.success() {Status::Passed} else {Status::Failed}, None);
                result.exit_code = status.code();
                if !status.success() {
                    result.error = Some(status.code().map(|c| format!("exited with {c}")).unwrap_or_else(|| "terminated by signal".into()));
                }
                break result;
            }
            _ = &mut deadline => break JobResult::new(job, Status::TimedOut, Some(format!("timed out after {}s", job.timeout_secs.unwrap_or_default()))),
            reader = readers.join_next(), if !readers.is_empty() => {
                let error = match reader {
                    Some(Ok(Ok(()))) => continue,
                    Some(Ok(Err(error))) => format!("{error:#}"),
                    Some(Err(error)) => error.to_string(),
                    None => continue,
                };
                break JobResult::new(job, Status::Failed, Some(format!("failed to read job output: {error}")));
            }
        }
    };
    // Stop descendants before draining pipes: they may otherwise keep the pipes open forever.
    group.kill();
    if child.try_wait()?.is_none() {
        let _ = child.kill().await;
        child.wait().await.context("failed to reap child")?;
    }
    while let Some(reader) = readers.join_next().await {
        if let Err(error) = reader? {
            result.status = Status::Failed;
            result.error = Some(format!("failed to read job output: {error:#}"));
        }
    }
    Ok(result)
}

async fn open_log(dir: Option<&Path>, filename: &str) -> Result<Option<tokio::fs::File>> {
    match dir {
        Some(dir) => {
            let path = dir.join(filename);
            Ok(Some(tokio::fs::File::create(&path).await.with_context(
                || format!("failed to create log {}", path.display()),
            )?))
        }
        None => Ok(None),
    }
}

async fn drain<R: AsyncRead + Unpin>(
    mut reader: R,
    name: String,
    stream: &'static str,
    mut log: Option<tokio::fs::File>,
    output: Output,
) -> Result<()> {
    let mut chunk = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        if let Some(file) = log.as_mut() {
            file.write_all(&chunk[..count]).await?;
        }
        pending.extend_from_slice(&chunk[..count]);
        loop {
            let length = match pending.iter().position(|&b| b == b'\n') {
                Some(index) => index + 1,
                None if pending.len() >= 8192 => {
                    // Keep a possible incomplete UTF-8 character for the next read.
                    let mut end = pending.len();
                    for start in pending.len().saturating_sub(3)..pending.len() {
                        if let Err(error) = std::str::from_utf8(&pending[start..]) {
                            if error.valid_up_to() == 0 && error.error_len().is_none() {
                                end = start;
                                break;
                            }
                        }
                    }
                    end
                }
                None => break,
            };
            if length == 0 {
                break;
            }
            output.emit(json!({"type":"job_output", "name":name, "stream":stream, "data":String::from_utf8_lossy(&pending[..length])})).await?;
            pending.drain(..length);
        }
    }
    if !pending.is_empty() {
        output.emit(json!({"type":"job_output", "name":name, "stream":stream, "data":String::from_utf8_lossy(&pending)})).await?;
    }
    if let Some(file) = log.as_mut() {
        file.flush().await?;
    }
    Ok(())
}
