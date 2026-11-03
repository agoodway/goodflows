use crate::tail::{SharedTails, Tails};
use crate::{
    config::{self, Condition, Job, Partition},
    expression::{Expression, Reference},
    output::Output,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// One scheduled unit: a configured job, or one instance of a partitioned job.
#[derive(Clone, Copy, Debug)]
struct Unit {
    job: usize,
    partition: Option<Partition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobResult {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition: Option<Partition>,
    pub command: Option<String>,
    pub status: Status,
    pub duration_ms: u128,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub outputs: Map<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_truncated: Option<bool>,
    /// Whether the job started (as opposed to being skipped or cancelled before launch).
    #[serde(skip)]
    pub executed: bool,
    /// Whether execution entered `run_job`, even if a condition then skipped it.
    /// Projected only into the run summary, never into live events.
    #[serde(skip)]
    pub started: bool,
}

impl JobResult {
    fn new(job: &Job, partition: Option<Partition>, status: Status, error: Option<String>) -> Self {
        Self {
            name: job.name.clone(),
            partition,
            command: job.command.clone(),
            status,
            duration_ms: 0,
            exit_code: None,
            error,
            outputs: Map::new(),
            steps: job
                .steps
                .iter()
                .map(|step| StepResult {
                    id: step.id.clone(),
                    command: step.command.clone(),
                    duration_ms: 0,
                    outcome: CommandResult::new(
                        if status == Status::Cancelled {
                            Status::Cancelled
                        } else {
                            Status::Skipped
                        },
                        Some("job did not run".into()),
                    ),
                })
                .collect(),
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: None,
            stderr_truncated: None,
            executed: false,
            started: false,
        }
    }

    fn finish(&mut self, outcome: CommandResult) {
        self.status = outcome.status;
        self.exit_code = outcome.exit_code;
        self.error = outcome.error;
        self.outputs = outcome.outputs;
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StepResult {
    pub id: String,
    pub command: String,
    pub duration_ms: u128,
    #[serde(flatten)]
    pub outcome: CommandResult,
}

#[derive(Clone, Debug, Serialize)]
pub struct CommandResult {
    pub status: Status,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub outputs: Map<String, Value>,
}

impl CommandResult {
    fn new(status: Status, error: Option<String>) -> Self {
        Self {
            status,
            exit_code: None,
            error,
            outputs: Map::new(),
        }
    }

    fn failed(error: anyhow::Error) -> Self {
        Self::new(Status::Failed, Some(format!("{error:#}")))
    }
}

type DependencyOutputs = BTreeMap<String, Map<String, Value>>;

pub struct Options {
    pub parallelism: usize,
    pub fail_fast: bool,
    pub workdir: PathBuf,
    pub log_dir: Option<PathBuf>,
    pub output: Output,
    pub reporting: Arc<ReportingEnv>,
    /// Selected `--env` profile. `None` when the run did not select one.
    pub environment: Option<String>,
    /// Resolved settings env defaults, layered over the caller's environment
    /// and under job and step env.
    pub settings_env: BTreeMap<String, String>,
}

/// Environment adjustments for reporting. Applied after job/step overrides.
#[derive(Debug, Default)]
pub struct ReportingEnv {
    /// A `[reporting]` block exists. The reserved names are then removed from
    /// every command and expression (even when inherited from the CLI's own
    /// environment) and set only for report-producing jobs with a spool.
    pub active: bool,
    /// The publisher's credential variable, removed from every child and expression.
    pub scrub: Option<String>,
    /// Reserved variables injected into report-producing jobs, keyed by job
    /// name and partition (`None` for an unpartitioned job).
    pub jobs: HashMap<ReportingKey, Vec<(String, String)>>,
}

/// A report-producing unit: job name and, for an instance, its partition.
pub type ReportingKey = (String, Option<Partition>);

impl ReportingEnv {
    fn reserved(&self, job: &str, partition: Option<Partition>) -> &[(String, String)] {
        self.jobs
            .get(&(job.to_owned(), partition))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Remove the token and the reserved names. Conditions see only this view:
    /// they decide whether the job runs, so they are not report producers.
    fn scrub(&self, env: &mut HashMap<String, String>) {
        if let Some(token) = &self.scrub {
            env.remove(token);
        }
        if self.active {
            for key in config::RESERVED_ENV {
                env.remove(key);
            }
        }
    }

    /// The scrubbed view plus the reserved values, for the job's own commands.
    fn inject(
        &self,
        job: &str,
        partition: Option<Partition>,
        env: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut env = env.clone();
        for (key, value) in self.reserved(job, partition) {
            env.insert(key.clone(), value.clone());
        }
        env
    }
}

/// Per-invocation state shared by every job task.
struct Shared {
    workdir: PathBuf,
    log_dir: Option<PathBuf>,
    output: Output,
    reporting: Arc<ReportingEnv>,
    environment: Option<String>,
    settings_env: BTreeMap<String, String>,
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
    // Dependencies are per configured job; scheduling state is per unit.
    let deps: Vec<Vec<usize>> = jobs
        .iter()
        .map(|j| j.depends_on.iter().map(|d| names[d.as_str()]).collect())
        .collect();
    let units: Vec<Unit> = jobs
        .iter()
        .enumerate()
        .flat_map(|(job, config)| {
            config
                .partition_ids()
                .map(move |partition| Unit { job, partition })
        })
        .collect();
    let mut job_units = vec![Vec::new(); jobs.len()];
    for (index, unit) in units.iter().enumerate() {
        job_units[unit.job].push(index);
    }
    let mut results: Vec<Option<JobResult>> = vec![None; units.len()];
    let mut started = vec![false; units.len()];
    let mut running = JoinSet::new();
    let mut failed = false;
    let shared = Arc::new(Shared {
        workdir: options.workdir.clone(),
        log_dir: options.log_dir.clone(),
        output: options.output.clone(),
        reporting: options.reporting.clone(),
        environment: options.environment.clone(),
        settings_env: options.settings_env.clone(),
    });

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
            for (index, unit) in units.iter().enumerate() {
                if started[index] || results[index].is_some() {
                    continue;
                }
                let job = &jobs[unit.job];
                let dependency = |d: usize| dependency_state(&results, &job_units[d]);
                let blocked = deps[unit.job]
                    .iter()
                    .any(|&d| dependency(d) == Dependency::Blocked);
                // A dependency job with a non-passing unit but unfinished siblings
                // blocks only once they finish, so the dependency reason is stable.
                let settling = deps[unit.job]
                    .iter()
                    .any(|&d| dependency(d) == Dependency::Settling);
                let reason = if cancelled {
                    Some((Status::Cancelled, "cancelled by signal"))
                } else if blocked {
                    Some((Status::Skipped, "dependency did not pass"))
                } else if settling {
                    None
                } else if options.fail_fast && failed {
                    Some((Status::Skipped, "fail_fast: previous job failed"))
                } else {
                    None
                };
                if let Some((status, error)) = reason {
                    record(
                        &mut results,
                        index,
                        JobResult::new(job, unit.partition, status, Some(error.into())),
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
        for (index, unit) in units.iter().copied().enumerate() {
            if running.len() >= options.parallelism || cancel.borrow().is_some() {
                break;
            }
            if started[index] || results[index].is_some() {
                continue;
            }
            if !deps[unit.job]
                .iter()
                .all(|&d| dependency_state(&results, &job_units[d]) == Dependency::Passed)
            {
                continue;
            }
            started[index] = true;
            let job = jobs[unit.job].clone();
            let shared = shared.clone();
            let cancel = cancel.clone();
            // Validation forbids output references to partitioned jobs, so only
            // single-unit dependencies publish outputs.
            let needs = deps[unit.job]
                .iter()
                .filter(|&&d| jobs[d].partitions.is_none())
                .map(|&d| {
                    let result = results[job_units[d][0]]
                        .as_ref()
                        .expect("ready dependency has a result");
                    (result.name.clone(), result.outputs.clone())
                })
                .collect();
            running.spawn(async move {
                let result = run_job(unit.job, unit.partition, &job, &shared, cancel, needs).await;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dependency {
    /// Every unit of the dependency job passed.
    Passed,
    /// Some unit has not finished and none has failed yet.
    Pending,
    /// Some unit did not pass, but siblings are still unfinished.
    Settling,
    /// Every unit finished and at least one did not pass.
    Blocked,
}

fn dependency_state(results: &[Option<JobResult>], units: &[usize]) -> Dependency {
    // An empty unit list would vacuously report `Passed`.
    debug_assert!(
        !units.is_empty(),
        "every job expands into at least one unit"
    );
    let mut finished = true;
    let mut passed = true;
    for &unit in units {
        match &results[unit] {
            Some(result) => passed &= result.status == Status::Passed,
            None => finished = false,
        }
    }
    match (finished, passed) {
        (true, true) => Dependency::Passed,
        (false, true) => Dependency::Pending,
        (false, false) => Dependency::Settling,
        (true, false) => Dependency::Blocked,
    }
}

/// The selected profile wins over job, step and inherited values. A run with no
/// profile removes the variable so a parent goodflows cannot leak its own.
fn apply_profile(env: &mut HashMap<String, String>, name: Option<&str>) {
    match name {
        Some(name) => {
            env.insert(config::ENVIRONMENT_ENV.into(), name.to_owned());
        }
        None => {
            env.remove(config::ENVIRONMENT_ENV);
        }
    }
}

async fn run_job(
    index: usize,
    partition: Option<Partition>,
    job: &Job,
    shared: &Shared,
    cancel: watch::Receiver<Option<i32>>,
    needs: DependencyOutputs,
) -> JobResult {
    let (workdir, log_dir, output) = (&shared.workdir, shared.log_dir.as_deref(), &shared.output);
    let reporting = &*shared.reporting;
    let start = Instant::now();
    let tails = Arc::new(Mutex::new(Tails::default()));
    let mut result = JobResult::new(job, partition, Status::Passed, None);
    result.started = true;
    // Set only when the main command or a step command spawns, never by a condition.
    let spawned = AtomicBool::new(false);
    let execution: Result<()> = async {
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
        // `index` is the job's position in the selected job list, so instance logs sort together.
        let stem = match partition {
            Some(partition) => format!("{:03}-{safe_name}.p{:02}", index + 1, partition.index),
            None => format!("{:03}-{safe_name}", index + 1),
        };
        let logs = [
            open_log(log_dir, &format!("{stem}.stdout.log")).await?,
            open_log(log_dir, &format!("{stem}.stderr.log")).await?,
        ];
        let mut execution = JobExecution {
            job,
            workdir,
            output,
            cancel,
            tails: tails.clone(),
            logs,
            needs,
            reporting,
            partition,
            environment: shared.environment.clone(),
            spawned: &spawned,
            deadline: job.timeout_secs.and_then(|secs| {
                tokio::time::Instant::now().checked_add(Duration::from_secs(secs))
            }),
        };
        output
            .emit(Partition::tag(
                partition,
                json!({"type":"job_started", "name":job.name}),
            ))
            .await?;
        // The caller's GOODFLOWS_OUTPUT belongs to an enclosing command; drop it so
        // expressions never observe it. Commands get their own file (or none).
        let mut env: HashMap<String, String> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, _)| key != config::OUTPUT_ENV)
            .chain(shared.settings_env.clone())
            .chain(job.env.clone())
            .collect();
        // Expressions read this map, so the token must be gone before any evaluation.
        reporting.scrub(&mut env);
        // Partition identity is visible to conditions, expressions and every
        // command; unpartitioned jobs never see values inherited by the CLI.
        let [index_key, count_key] = config::PARTITION_ENV;
        match partition {
            Some(partition) => {
                env.insert(index_key.into(), partition.index.to_string());
                env.insert(count_key.into(), partition.count.to_string());
            }
            None => {
                env.remove(index_key);
                env.remove(count_key);
            }
        }
        apply_profile(&mut env, shared.environment.as_deref());
        let condition = execution
            .condition(job.condition.as_ref(), &env, &[], None)
            .await?;
        if condition.status != Status::Passed {
            result.finish(condition);
            return Ok(());
        }
        let job_env = reporting.inject(&job.name, partition, &env);
        if let Some(command) = &job.command {
            let outcome = execution
                .command(
                    command,
                    job.shell,
                    &job_env,
                    None,
                    // Partitioned jobs publish no job outputs.
                    Invocation::Command {
                        outputs: partition.is_none(),
                    },
                )
                .await?;
            result.finish(outcome);
            return Ok(());
        }
        for (index, step) in job.steps.iter().enumerate() {
            let step_start = Instant::now();
            let mut condition_env = env
                .iter()
                .chain(step.env.iter())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            reporting.scrub(&mut condition_env);
            apply_profile(&mut condition_env, execution.environment.as_deref());
            let step_env = reporting.inject(&job.name, partition, &condition_env);
            output
                .emit(Partition::tag(
                    partition,
                    json!({"type":"step_started", "name":job.name, "step":step.id}),
                ))
                .await?;
            let outcome = async {
                let condition = execution
                    .condition(
                        step.condition.as_ref(),
                        &condition_env,
                        &result.steps[..index],
                        Some(&step.id),
                    )
                    .await?;
                if condition.status != Status::Passed {
                    return Ok(condition);
                }
                execution
                    .command(
                        &step.command,
                        step.shell.unwrap_or(job.shell),
                        &step_env,
                        Some(&step.id),
                        Invocation::Command { outputs: true },
                    )
                    .await
            }
            .await
            .unwrap_or_else(CommandResult::failed);
            result.steps[index].duration_ms = step_start.elapsed().as_millis();
            result.steps[index].outcome = outcome.clone();
            output
                .emit(Partition::tag(
                    partition,
                    json!({"type":"step_finished", "name":job.name, "result":result.steps[index]}),
                ))
                .await?;
            if !matches!(outcome.status, Status::Passed | Status::Skipped) {
                result.command = Some(step.command.clone());
                result.finish(outcome);
                result.error = result
                    .error
                    .take()
                    .map(|error| format!("step '{}': {error}", step.id));
                for pending in &mut result.steps[index + 1..] {
                    pending.outcome = CommandResult::new(
                        if result.status == Status::Cancelled {
                            Status::Cancelled
                        } else {
                            Status::Skipped
                        },
                        Some("previous step did not pass".into()),
                    );
                    output
                        .emit(Partition::tag(
                            partition,
                            json!({"type":"step_finished", "name":job.name, "result":pending}),
                        ))
                        .await?;
                }
                return Ok(());
            }
        }
        for (key, source) in &job.outputs {
            let value = execution
                .evaluate(source, &job_env, &result.steps)
                .with_context(|| format!("job output '{key}'"))?;
            if !matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_)) {
                bail!("job output '{key}' must be a boolean, number, or string");
            }
            result.outputs.insert(key.clone(), value);
        }
        Ok(())
    }
    .await;
    if let Err(error) = execution {
        result.finish(CommandResult::failed(error));
    }
    result.executed = spawned.load(Ordering::Relaxed);
    result.duration_ms = start.elapsed().as_millis();
    if matches!(result.status, Status::Failed | Status::TimedOut) {
        let [(stdout, stdout_truncated), (stderr, stderr_truncated)] =
            tails.lock().expect("tail mutex poisoned").snapshot();
        result.stdout_tail = Some(stdout);
        result.stderr_tail = Some(stderr);
        result.stdout_truncated = Some(stdout_truncated);
        result.stderr_truncated = Some(stderr_truncated);
    }
    result
}

struct JobExecution<'a> {
    job: &'a Job,
    workdir: &'a Path,
    output: &'a Output,
    cancel: watch::Receiver<Option<i32>>,
    tails: SharedTails,
    logs: [Option<tokio::fs::File>; 2],
    needs: DependencyOutputs,
    reporting: &'a ReportingEnv,
    partition: Option<Partition>,
    environment: Option<String>,
    spawned: &'a AtomicBool,
    deadline: Option<tokio::time::Instant>,
}

impl JobExecution<'_> {
    fn stopped(&self) -> Option<CommandResult> {
        if self.cancel.borrow().is_some() {
            Some(CommandResult::new(
                Status::Cancelled,
                Some("cancelled by signal".into()),
            ))
        } else if self
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            Some(CommandResult::new(
                Status::TimedOut,
                Some(format!(
                    "timed out after {}s",
                    self.job.timeout_secs.unwrap_or_default()
                )),
            ))
        } else {
            None
        }
    }

    fn evaluate(
        &self,
        source: &str,
        env: &HashMap<String, String>,
        steps: &[StepResult],
    ) -> Result<Value> {
        Expression::parse(source)?.evaluate(&|reference| self.lookup(reference, env, steps))
    }

    fn lookup(
        &self,
        reference: &Reference,
        env: &HashMap<String, String>,
        steps: &[StepResult],
    ) -> Result<Value> {
        match reference {
            Reference::Env(key) => Ok(env
                .get(key)
                .map(|value| Value::String(value.clone()))
                .unwrap_or(Value::Null)),
            Reference::Needs { job, key } => self
                .needs
                .get(job)
                .and_then(|outputs| outputs.get(key))
                .cloned()
                .with_context(|| format!("missing output needs.{job}.outputs.{key}")),
            Reference::Step { step, key } => steps
                .iter()
                .find(|result| &result.id == step)
                .and_then(|result| result.outcome.outputs.get(key))
                .cloned()
                .with_context(|| format!("missing output steps.{step}.outputs.{key}")),
        }
    }

    async fn condition(
        &mut self,
        condition: Option<&Condition>,
        env: &HashMap<String, String>,
        steps: &[StepResult],
        step: Option<&str>,
    ) -> Result<CommandResult> {
        if let Some(result) = self.stopped() {
            return Ok(result);
        }
        match condition {
            Some(Condition::Expression { expression }) => {
                let value = Expression::parse(expression)?
                    .evaluate_bool(&|reference| self.lookup(reference, env, steps))
                    .context("condition expression")?;
                match value {
                    true => Ok(CommandResult::new(Status::Passed, None)),
                    false => Ok(CommandResult::new(
                        Status::Skipped,
                        Some("condition expression was false".into()),
                    )),
                }
            }
            Some(Condition::LegacyShell(shell) | Condition::Shell { shell }) => {
                self.command(shell, true, env, step, Invocation::Condition)
                    .await
            }
            None => Ok(CommandResult::new(Status::Passed, None)),
        }
    }
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

/// What a spawned command is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Invocation {
    /// A shell condition: a nonzero exit skips, and it never counts as spawned work.
    Condition,
    /// The job's command or a step; `outputs` provides `GOODFLOWS_OUTPUT`.
    Command { outputs: bool },
}

impl JobExecution<'_> {
    async fn command(
        &mut self,
        script: &str,
        shell: bool,
        env: &HashMap<String, String>,
        step: Option<&str>,
        invocation: Invocation,
    ) -> Result<CommandResult> {
        if let Some(result) = self.stopped() {
            return Ok(result);
        }
        let is_condition = invocation == Invocation::Condition;
        let job = self.job;
        let output = self.output;
        let tails = &self.tails;
        let output_file = if invocation == (Invocation::Command { outputs: true }) {
            Some(tempfile::NamedTempFile::new().context("create job output file")?)
        } else {
            None
        };
        let deadline = async {
            if let Some(deadline) = self.deadline {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(deadline);
        let mut command = if shell {
            let mut c = Command::new("sh");
            c.arg("-c").arg(script);
            c
        } else {
            let args = shell_words::split(script)?;
            let mut c = Command::new(&args[0]);
            c.args(&args[1..]);
            c
        };
        command
            .current_dir(self.workdir)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        // .envs() only adds to the inherited CLI environment, so the token and any
        // inherited reserved values (from an enclosing goodflows run) must be removed.
        if let Some(token) = &self.reporting.scrub {
            command.env_remove(token);
        }
        if self.reporting.active {
            for key in config::RESERVED_ENV {
                command.env_remove(key);
            }
        }
        if !is_condition {
            for (key, value) in self.reporting.reserved(&job.name, self.partition) {
                command.env(key, value);
            }
        }
        // After the reporting removals, so partition identity always wins.
        let [index_key, count_key] = config::PARTITION_ENV;
        match self.partition {
            Some(partition) => {
                command.env(index_key, partition.index.to_string());
                command.env(count_key, partition.count.to_string());
            }
            None => {
                command.env_remove(index_key);
                command.env_remove(count_key);
            }
        }
        match &self.environment {
            Some(name) => {
                command.env(config::ENVIRONMENT_ENV, name);
            }
            None => {
                command.env_remove(config::ENVIRONMENT_ENV);
            }
        }
        if let Some(file) = &output_file {
            command.env(config::OUTPUT_ENV, file.path());
        } else {
            command.env_remove(config::OUTPUT_ENV);
        }
        if let Some(result) = self.stopped() {
            return Ok(result);
        }
        let cancel = &mut self.cancel;
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn '{}'", job.name))?;
        if !is_condition {
            self.spawned.store(true, Ordering::Relaxed);
        }
        let mut group = ProcessGroup {
            id: Some(child.id().context("child has no PID")?),
        };
        let mut readers = JoinSet::new();
        // The fields every `job_output` event of this command shares.
        let mut source = Partition::tag(
            self.partition,
            json!({"type":"job_output", "name":job.name}),
        );
        if let Some(step) = step {
            source["step"] = json!(step);
        }
        let pipe = child.stdout.take().context("missing stdout pipe")?;
        readers.spawn(drain(
            pipe,
            source.clone(),
            "stdout",
            match &self.logs[0] {
                Some(file) => Some(file.try_clone().await?),
                None => None,
            },
            output.clone(),
            tails.clone(),
        ));
        let pipe = child.stderr.take().context("missing stderr pipe")?;
        readers.spawn(drain(
            pipe,
            source.clone(),
            "stderr",
            match &self.logs[1] {
                Some(file) => Some(file.try_clone().await?),
                None => None,
            },
            output.clone(),
            tails.clone(),
        ));

        let mut result = loop {
            tokio::select! {
                biased;
                _ = async {
                    if cancel.borrow().is_none() { let _ = cancel.changed().await; }
                } => break CommandResult::new(Status::Cancelled, Some("cancelled by signal".into())),
                status = child.wait() => {
                    let status = status.context("failed to wait for child")?;
                    let skipped = is_condition && !status.success() && status.code().is_some();
                    let mut result = CommandResult::new(if status.success() {Status::Passed} else if skipped {Status::Skipped} else {Status::Failed}, None);
                    result.exit_code = status.code();
                    if !status.success() {
                        result.error = Some(status.code().map(|c| format!("exited with {c}")).unwrap_or_else(|| "terminated by signal".into()));
                    }
                    break result;
                }
                _ = &mut deadline => break CommandResult::new(Status::TimedOut, Some(format!("timed out after {}s", job.timeout_secs.unwrap_or_default()))),
                reader = readers.join_next(), if !readers.is_empty() => {
                    let error = match reader {
                        Some(Ok(Ok(()))) => continue,
                        Some(Ok(Err(error))) => format!("{error:#}"),
                        Some(Err(error)) => error.to_string(),
                        None => continue,
                    };
                    break CommandResult::new(Status::Failed, Some(format!("failed to read job output: {error}")));
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
        if is_condition && result.status != Status::Passed {
            result.error = result.error.map(|error| format!("condition: {error}"));
        }
        if result.status == Status::Passed {
            if let Some(file) = &output_file {
                match read_outputs(file.path()).await {
                    Ok(outputs) => result.outputs = outputs,
                    Err(error) => {
                        result.status = Status::Failed;
                        result.error = Some(format!("invalid job outputs: {error:#}"));
                    }
                }
            }
        }
        Ok(result)
    }
}

async fn read_outputs(path: &Path) -> Result<Map<String, Value>> {
    const LIMIT: u64 = 65_536;
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .await?;
    if !file.metadata().await?.is_file() {
        bail!("output must be a regular file");
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > LIMIT {
        bail!("output file exceeds 64 KiB");
    }
    if bytes.is_empty() {
        return Ok(Map::new());
    }
    let outputs: Map<String, Value> =
        serde_json::from_slice(&bytes).context("expected a JSON object")?;
    for (key, value) in &outputs {
        if key.trim().is_empty() || key.chars().any(char::is_control) {
            bail!("output keys must be nonempty and contain no control characters");
        }
        if !matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_)) {
            bail!("output '{key}' must be a boolean, number, or string");
        }
    }
    Ok(outputs)
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
    source: Value,
    stream: &'static str,
    mut log: Option<tokio::fs::File>,
    output: Output,
    tails: SharedTails,
) -> Result<()> {
    let event = |data: &[u8]| {
        let mut event = source.clone();
        event["stream"] = json!(stream);
        event["data"] = json!(String::from_utf8_lossy(data));
        event
    };
    let mut chunk = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        tails
            .lock()
            .expect("tail mutex poisoned")
            .append(stream, &chunk[..count]);
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
            output.emit(event(&pending[..length])).await?;
            pending.drain(..length);
        }
    }
    if !pending.is_empty() {
        output.emit(event(&pending)).await?;
    }
    if let Some(file) = log.as_mut() {
        file.flush().await?;
    }
    Ok(())
}
