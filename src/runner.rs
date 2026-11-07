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
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
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
    /// A workflow-call job's target workflow, even when the call was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The child invocation's run id, once the call activated one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
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
    /// From the start of this invocation's `execute` until every dependency
    /// passed, or until an earlier skip or cancel. Summary and history only.
    #[serde(skip)]
    pub blocked_ms: u64,
    /// After `blocked_ms`, the wait for a slot until launch, skip or cancel.
    #[serde(skip)]
    pub queued_ms: u64,
    /// One entry per spawned condition, command or step. Summary and history only.
    #[serde(skip)]
    pub commands: Vec<CommandCost>,
}

/// What a spawned process was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Condition,
    Command,
}

/// The measured cost of one spawned process. Unavailable measurements are omitted.
#[derive(Clone, Debug, Serialize)]
pub struct CommandCost {
    pub kind: CommandKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
}

/// The fields each `summary.results` entry and each history instance add
/// to a result: never part of live `job_finished` events.
#[derive(Serialize)]
pub struct Scheduling<'a> {
    pub started: bool,
    pub blocked_ms: u64,
    pub queued_ms: u64,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub commands: &'a [CommandCost],
}

/// One `summary.results` entry: the live result plus its scheduling fields.
#[derive(Serialize)]
pub struct SummaryResult<'a> {
    #[serde(flatten)]
    pub result: &'a JobResult,
    #[serde(flatten)]
    pub scheduling: Scheduling<'a>,
}

impl JobResult {
    pub fn scheduling(&self) -> Scheduling<'_> {
        Scheduling {
            started: self.started,
            blocked_ms: self.blocked_ms,
            queued_ms: self.queued_ms,
            commands: &self.commands,
        }
    }

    pub fn summary(&self) -> SummaryResult<'_> {
        SummaryResult {
            result: self,
            scheduling: self.scheduling(),
        }
    }

    fn new(job: &Job, partition: Option<Partition>, status: Status, error: Option<String>) -> Self {
        Self {
            name: job.name.clone(),
            partition,
            command: job.command.clone(),
            workflow: job.workflow.clone(),
            child_run_id: None,
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
            blocked_ms: 0,
            queued_ms: 0,
            commands: Vec::new(),
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
    /// This invocation's capacity and fail-fast scope in the run tree.
    pub scope: Arc<Scope>,
    /// Starts a call job's child invocation; `None` when no job calls a workflow.
    pub calls: Option<Activate>,
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

/// A call job's request to run its target workflow as a child invocation.
pub struct Activation {
    pub target: String,
    pub caller_job: String,
    /// The caller's scope, under which the child's scope nests.
    pub scope: Arc<Scope>,
}

/// How a child invocation ended, for its call job's result.
pub struct ChildEnd {
    /// Set once the child's identity was allocated.
    pub run_id: Option<String>,
    pub status: Status,
    pub error: Option<String>,
}

/// Runs a child invocation to completion, including its reporting and summary.
pub type Activate =
    Arc<dyn Fn(Activation) -> Pin<Box<dyn Future<Output = ChildEnd> + Send>> + Send + Sync>;

/// State shared by every invocation of one run: the lock that makes a launch
/// check every containing limit at once, a counter bumped whenever capacity
/// frees, and the one RSS sampler.
pub struct Tree {
    lock: Mutex<()>,
    released: watch::Sender<u64>,
    rss: Arc<RssMonitor>,
    _rss_task: RssTask,
}

/// One invocation's place in the run tree: its parallelism limit over the
/// command/step instances (and call-condition shells) executing in its
/// subtree, and its fail-fast policy.
pub struct Scope {
    tree: Arc<Tree>,
    parent: Option<Arc<Scope>>,
    limit: usize,
    fail_fast: bool,
    /// Admitted units holding a slot in this subtree. A slot frees only
    /// after the scheduler records the unit's result.
    now: AtomicUsize,
    /// Units executing in this subtree (inside `run_job`, or a call's shell
    /// condition while it runs) and their high water mark, `max_in_flight`.
    inside: AtomicUsize,
    max: AtomicUsize,
    /// A unit in this subtree failed or timed out.
    failed: AtomicBool,
}

impl Scope {
    /// The root invocation's scope, which starts the run's RSS sampler.
    pub fn root(limit: usize, fail_fast: bool) -> Arc<Self> {
        let (rss, rss_task) = RssMonitor::start();
        let tree = Arc::new(Tree {
            lock: Mutex::new(()),
            released: watch::channel(0).0,
            rss,
            _rss_task: rss_task,
        });
        Arc::new(Self::new(tree, None, limit, fail_fast))
    }

    /// A child invocation's scope, nested in its caller's.
    pub fn child(parent: &Arc<Self>, limit: usize, fail_fast: bool) -> Arc<Self> {
        Arc::new(Self::new(
            parent.tree.clone(),
            Some(parent.clone()),
            limit,
            fail_fast,
        ))
    }

    fn new(tree: Arc<Tree>, parent: Option<Arc<Self>>, limit: usize, fail_fast: bool) -> Self {
        Self {
            tree,
            parent,
            limit,
            fail_fast,
            now: AtomicUsize::new(0),
            inside: AtomicUsize::new(0),
            max: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
        }
    }

    /// This scope and every containing scope up to the root.
    fn chain(&self) -> impl Iterator<Item = &Self> {
        std::iter::successors(Some(self), |scope| scope.parent.as_deref())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.tree.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Admit one executable unit if this scope and every containing scope
    /// have a free slot. The slot is held until the returned guard drops.
    fn try_acquire(self: &Arc<Self>) -> Option<Slot> {
        let _lock = self.lock();
        if self
            .chain()
            .any(|scope| scope.now.load(Ordering::SeqCst) >= scope.limit)
        {
            return None;
        }
        for scope in self.chain() {
            scope.now.fetch_add(1, Ordering::SeqCst);
        }
        Some(Slot(self.clone()))
    }

    /// Count one executing unit in this scope and every containing scope
    /// until the guard drops, even if the unit panics. Separate from the
    /// slot, which outlives execution until the result is recorded.
    fn enter(self: &Arc<Self>) -> Inside {
        for scope in self.chain() {
            let inside = scope.inside.fetch_add(1, Ordering::SeqCst) + 1;
            scope.max.fetch_max(inside, Ordering::SeqCst);
        }
        Inside(self.clone())
    }

    /// Record a failure in this invocation and every containing one.
    fn fail(&self) {
        for scope in self.chain() {
            scope.failed.store(true, Ordering::SeqCst);
        }
    }

    /// Whether an enabled fail-fast policy of this or a containing
    /// invocation stops new work here.
    pub fn stopped(&self) -> bool {
        self.chain()
            .any(|scope| scope.fail_fast && scope.failed.load(Ordering::SeqCst))
    }
}

/// One admitted executable unit. Dropping it frees the slot in every
/// containing scope and wakes invocations waiting for capacity.
struct Slot(Arc<Scope>);

impl Drop for Slot {
    fn drop(&mut self) {
        let lock = self.0.lock();
        for scope in self.0.chain() {
            scope.now.fetch_sub(1, Ordering::SeqCst);
        }
        drop(lock);
        self.0.tree.released.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// One executing unit, counted for `max_in_flight` until dropped.
struct Inside(Arc<Scope>);

impl Drop for Inside {
    fn drop(&mut self) {
        for scope in self.0.chain() {
            scope.inside.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn failed(result: &JobResult) -> bool {
    matches!(result.status, Status::Failed | Status::TimedOut)
}

/// Per-invocation state shared by every job task.
struct Shared {
    workdir: PathBuf,
    log_dir: Option<PathBuf>,
    output: Output,
    reporting: Arc<ReportingEnv>,
    environment: Option<String>,
    settings_env: BTreeMap<String, String>,
    rss: Arc<RssMonitor>,
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

/// Record a started unit's result. A failure stops this and every
/// containing invocation whose fail-fast policy is enabled before the
/// unit's slot frees, so an invocation waiting for that capacity sees the
/// failure before it can launch. An invocation with free slots re-checks
/// `Scope::stopped` before each launch, so only a launch racing that check
/// can still start. Marking the failure here rather than in the unit's task
/// keeps dependents of the failed unit on the dependency reason.
async fn finish(
    scope: &Scope,
    results: &mut [Option<JobResult>],
    index: usize,
    result: JobResult,
    slot: Option<Slot>,
    output: &Output,
) -> Result<()> {
    if failed(&result) {
        scope.fail();
    }
    record(results, index, result, output).await?;
    drop(slot);
    Ok(())
}

/// The scheduler's results plus run-level measurements.
pub struct Execution {
    pub results: Vec<JobResult>,
    /// The most executable units (command/step instances and call-condition
    /// shells) at once in this invocation's subtree; waiting calls never count.
    pub max_in_flight: usize,
}

/// Blocked and queued time in milliseconds for a unit leaving the queue at
/// `now`, given when its dependencies were satisfied (if they were).
fn waits(start: Instant, ready: Option<Instant>, now: Instant) -> (u64, u64) {
    let ms = |d: Duration| d.as_millis() as u64;
    match ready {
        Some(ready) => (ms(ready - start), ms(now - ready)),
        None => (ms(now - start), 0),
    }
}

pub async fn execute(
    jobs: &[Job],
    options: &Options,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<Execution> {
    let run_start = Instant::now();
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
    // Jobs whose dependencies reach a workflow call, directly or through
    // other jobs. Only these take a dependency skip before fail-fast
    // regardless of listing order; other jobs keep the configuration-order
    // behavior of configs without calls.
    let mut reaches_call = vec![false; jobs.len()];
    loop {
        let mut changed = false;
        for job in 0..jobs.len() {
            if !reaches_call[job]
                && deps[job]
                    .iter()
                    .any(|&d| jobs[d].workflow.is_some() || reaches_call[d])
            {
                reaches_call[job] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut results: Vec<Option<JobResult>> = vec![None; units.len()];
    let mut started = vec![false; units.len()];
    // When each unit's dependencies were all satisfied, and its waits at launch.
    let mut ready_at: Vec<Option<Instant>> = vec![None; units.len()];
    let mut launch_waits = vec![(0, 0); units.len()];
    let mut running = JoinSet::new();
    let scope = &options.scope;
    // Bumped whenever any invocation of the run frees a slot.
    let mut released = scope.tree.released.subscribe();
    let shared = Arc::new(Shared {
        workdir: options.workdir.clone(),
        log_dir: options.log_dir.clone(),
        output: options.output.clone(),
        reporting: options.reporting.clone(),
        environment: options.environment.clone(),
        settings_env: options.settings_env.clone(),
        // One sampler serves the whole run tree.
        rss: scope.tree.rss.clone(),
    });

    loop {
        // Consume completed jobs before deciding whether to launch queued work.
        while let Some(completed) = running.try_join_next() {
            let (index, mut result, slot): (usize, JobResult, Option<Slot>) = completed?;
            (result.blocked_ms, result.queued_ms) = launch_waits[index];
            finish(scope, &mut results, index, result, slot, &options.output).await?;
        }
        // Whether every dependency of each configured job passed. Only the
        // skips recorded below change results before the launch loop, and a
        // skip never makes a dependency pass, so one computation per pass
        // serves the ready scan and the launch loop.
        let deps_passed: Vec<bool> = deps
            .iter()
            .map(|deps| {
                deps.iter()
                    .all(|&d| dependency_state(&results, &job_units[d]) == Dependency::Passed)
            })
            .collect();
        // Dependency satisfaction is observed right after the last dependency's result is recorded.
        let now = Instant::now();
        for (index, unit) in units.iter().enumerate() {
            if ready_at[index].is_none()
                && !started[index]
                && results[index].is_none()
                && deps_passed[unit.job]
            {
                ready_at[index] = Some(now);
            }
        }
        // Jobs that wait, directly or through unfinished jobs, on a started
        // call without a result. A child failure stops this invocation before
        // the call fails, so these keep the dependency reason once it does.
        // Skips below never record a call, so one computation per pass serves.
        let mut calling: Vec<bool> = vec![false; jobs.len()];
        loop {
            let mut changed = false;
            for job in 0..jobs.len() {
                if calling[job] {
                    continue;
                }
                calling[job] = deps[job].iter().any(|&d| {
                    let unfinished = job_units[d].iter().any(|&u| results[u].is_none());
                    let running_call = jobs[d].workflow.is_some() && started[job_units[d][0]];
                    unfinished && (running_call || calling[d])
                });
                changed |= calling[job];
            }
            if !changed {
                break;
            }
        }
        let cancelled = cancel.borrow().is_some();
        // A failure anywhere in a containing invocation with fail-fast enabled.
        let stopped = scope.stopped();
        // Repeat so transitive skips propagate even when configs are not
        // topologically ordered. The first phase holds back the fail-fast skip
        // of jobs that reach a call, so a failed call's dependency skips reach
        // them first; the second phase applies it to what remains.
        for defer_calls in [true, false] {
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
                    } else if settling || calling[unit.job] {
                        None
                    // Only call-reaching jobs are deferred. Other jobs keep the
                    // configuration-order fail-fast skip on purpose, so configs
                    // without calls behave exactly as before calls existed.
                    } else if stopped && !(defer_calls && reaches_call[unit.job]) {
                        Some((Status::Skipped, "fail_fast: previous job failed"))
                    } else {
                        None
                    };
                    if let Some((status, error)) = reason {
                        let mut result =
                            JobResult::new(job, unit.partition, status, Some(error.into()));
                        (result.blocked_ms, result.queued_ms) =
                            waits(run_start, ready_at[index], Instant::now());
                        record(&mut results, index, result, &options.output).await?;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
        }
        // Capacity freed after this point wakes the wait below.
        released.borrow_and_update();
        // Set once a ready unit found no free slot: later executable units
        // wait behind it, keeping configuration and partition launch order.
        let mut waiting = false;
        for (index, unit) in units.iter().copied().enumerate() {
            // Another invocation can stop this one after the snapshot above;
            // the next pass skips what remains.
            if cancel.borrow().is_some() || scope.stopped() {
                break;
            }
            if started[index] || results[index].is_some() {
                continue;
            }
            if !deps_passed[unit.job] {
                continue;
            }
            let job = &jobs[unit.job];
            // A waiting call holds no slot; only its own shell condition does,
            // while that shell runs.
            let needs_slot = job.workflow.is_none()
                || matches!(
                    job.condition,
                    Some(Condition::LegacyShell(_) | Condition::Shell { .. })
                );
            let slot = if needs_slot {
                if waiting {
                    continue;
                }
                match scope.try_acquire() {
                    Some(slot) => Some(slot),
                    None => {
                        waiting = true;
                        continue;
                    }
                }
            } else {
                None
            };
            started[index] = true;
            launch_waits[index] = waits(run_start, ready_at[index], Instant::now());
            let job = job.clone();
            let shared = shared.clone();
            let cancel = cancel.clone();
            // Validation forbids output references to partitioned and call
            // jobs, so only single-unit command and step dependencies publish outputs.
            let needs = deps[unit.job]
                .iter()
                .filter(|&&d| jobs[d].partitions.is_none() && jobs[d].workflow.is_none())
                .map(|&d| {
                    let result = results[job_units[d][0]]
                        .as_ref()
                        .expect("ready dependency has a result");
                    (result.name.clone(), result.outputs.clone())
                })
                .collect();
            let scope = scope.clone();
            let calls = options.calls.clone();
            running.spawn(async move {
                // The slot returns with the result, so it frees only after
                // the scheduler records the result and any failure.
                let (result, slot) = if job.workflow.is_some() {
                    run_call(unit.job, &job, &shared, cancel, needs, slot, calls, &scope).await
                } else {
                    let inside = scope.enter();
                    let result =
                        run_job(unit.job, unit.partition, &job, &shared, cancel, needs).await;
                    drop(inside);
                    (result, slot)
                };
                (index, result, slot)
            });
        }
        if results.iter().all(Option::is_some) {
            break;
        }
        if running.is_empty() && !waiting {
            // Cancellation or another invocation's failure can arrive during
            // the launch loop; classify pending work next iteration.
            if cancel.borrow().is_some() || scope.stopped() {
                continue;
            }
            anyhow::bail!("no runnable jobs remain");
        }
        tokio::select! {
            biased;
            changed = cancel.changed(), if !cancelled => { changed.context("signal monitor stopped")?; }
            completed = running.join_next(), if !running.is_empty() => {
                let (index, mut result, slot) = completed.context("job scheduler stopped")??;
                (result.blocked_ms, result.queued_ms) = launch_waits[index];
                finish(scope, &mut results, index, result, slot, &options.output).await?;
            }
            // Another invocation freed a slot (the sender lives as long as `scope`).
            _ = released.changed(), if waiting => {}
        }
    }
    Ok(Execution {
        results: results.into_iter().flatten().collect(),
        max_in_flight: scope.max.load(Ordering::SeqCst),
    })
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

/// The stdout and stderr log files of one unit, when logs are enabled.
async fn open_logs(
    log_dir: Option<&Path>,
    index: usize,
    job: &Job,
    partition: Option<Partition>,
) -> Result<[Option<tokio::fs::File>; 2]> {
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
    Ok([
        open_log(log_dir, &format!("{stem}.stdout.log")).await?,
        open_log(log_dir, &format!("{stem}.stderr.log")).await?,
    ])
}

/// The environment a job's conditions and expressions see, before step env
/// and reserved reporting values: the caller's environment, this
/// invocation's settings env and job env, scrubbed, with partition and
/// profile identity.
fn job_env(shared: &Shared, job: &Job, partition: Option<Partition>) -> HashMap<String, String> {
    // The caller's GOODFLOWS_OUTPUT belongs to an enclosing command; drop it so
    // expressions never observe it. Commands get their own file (or none).
    let mut env: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| key != config::OUTPUT_ENV)
        .chain(shared.settings_env.clone())
        .chain(job.env.clone())
        .collect();
    // Expressions read this map, so the token must be gone before any evaluation.
    shared.reporting.scrub(&mut env);
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
    env
}

/// A workflow-call job: its own condition (a shell condition holds `slot`
/// only while it runs), then the child invocation, whose outcome becomes the
/// call's status. `duration_ms` covers both, never the dependency wait. The
/// slot is returned when the call ends without activating a child.
#[allow(clippy::too_many_arguments)]
async fn run_call(
    index: usize,
    job: &Job,
    shared: &Shared,
    cancel: watch::Receiver<Option<i32>>,
    needs: DependencyOutputs,
    mut slot: Option<Slot>,
    calls: Option<Activate>,
    scope: &Arc<Scope>,
) -> (JobResult, Option<Slot>) {
    let start = Instant::now();
    let target = job.workflow.clone().expect("a call job names a workflow");
    let mut result = JobResult::new(job, None, Status::Passed, None);
    result.started = true;
    let spawned = AtomicBool::new(false);
    let commands = Mutex::new(Vec::new());
    // Only a shell condition, which holds a slot, executes a unit.
    let inside = slot.as_ref().map(|_| scope.enter());
    let condition: Result<CommandResult> = async {
        shared
            .output
            .emit(json!({"type":"job_started", "name":job.name}))
            .await?;
        // Only a shell condition spawns a process, so only it writes logs.
        let logs = match slot {
            Some(_) => open_logs(shared.log_dir.as_deref(), index, job, None).await?,
            None => [None, None],
        };
        let env = job_env(shared, job, None);
        let mut execution = JobExecution {
            job,
            workdir: &shared.workdir,
            output: &shared.output,
            cancel: cancel.clone(),
            tails: Arc::new(Mutex::new(Tails::default())),
            logs,
            needs,
            reporting: &shared.reporting,
            partition: None,
            environment: shared.environment.clone(),
            spawned: &spawned,
            commands: &commands,
            deadline: None,
            rss: shared.rss.clone(),
        };
        execution
            .condition(job.condition.as_ref(), &env, &[], None)
            .await
    }
    .await;
    drop(inside);
    result.commands = commands.into_inner().expect("command cost mutex poisoned");
    match condition {
        Err(error) => result.finish(CommandResult::failed(error)),
        Ok(condition) if condition.status != Status::Passed => result.finish(condition),
        Ok(_) if cancel.borrow().is_some() => result.finish(CommandResult::new(
            Status::Cancelled,
            Some("cancelled by signal".into()),
        )),
        // A failure elsewhere in an enclosing fail-fast invocation during
        // the condition stops new work, so no child starts.
        Ok(_) if scope.stopped() => result.finish(CommandResult::new(
            Status::Skipped,
            Some("fail_fast: previous job failed".into()),
        )),
        Ok(_) => {
            let activate = calls.expect("call jobs run only when the root selection has calls");
            // The condition shell's slot frees before the child starts.
            drop(slot.take());
            let end = activate(Activation {
                target,
                caller_job: job.name.clone(),
                scope: scope.clone(),
            })
            .await;
            result.child_run_id = end.run_id;
            result.status = end.status;
            result.error = end.error;
        }
    }
    result.duration_ms = start.elapsed().as_millis();
    (result, slot)
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
    let commands = Mutex::new(Vec::new());
    let execution: Result<()> = async {
        let logs = open_logs(log_dir, index, job, partition).await?;
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
            commands: &commands,
            deadline: job.timeout_secs.and_then(|secs| {
                tokio::time::Instant::now().checked_add(Duration::from_secs(secs))
            }),
            rss: shared.rss.clone(),
        };
        output
            .emit(Partition::tag(
                partition,
                json!({"type":"job_started", "name":job.name}),
            ))
            .await?;
        let env = job_env(shared, job, partition);
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
    result.commands = commands.into_inner().expect("command cost mutex poisoned");
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
    commands: &'a Mutex<Vec<CommandCost>>,
    deadline: Option<tokio::time::Instant>,
    rss: Arc<RssMonitor>,
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

/// Kills a command's process group, and the child itself if it left the
/// group and is not reaped yet. A guard, so an early return, a dropped
/// future, an I/O error that aborts the scheduler or a panicking task still
/// cleans up.
struct ProcessGroup {
    id: Option<u32>,
    /// Set by the reaper, under this lock, once `wait4` reaped the child.
    reaped: Arc<Mutex<bool>>,
}

impl ProcessGroup {
    fn kill(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        // SAFETY: process_group(0) creates a private group whose ID is this child PID.
        // Negative PID targets that group, never the CLI's own process group.
        unsafe {
            libc::kill(-(id as i32), libc::SIGKILL);
        }
        // The group kill misses a child that left its own group. The reaper
        // sets the flag under this lock only after `wait4`, and until then the
        // child is unreaped (at worst a zombie), so its pid cannot have been
        // reused while the flag is still false.
        let reaped = self.reaped.lock().unwrap_or_else(|e| e.into_inner());
        if !*reaped {
            // SAFETY: kill takes only integer arguments.
            unsafe {
                libc::kill(id as i32, libc::SIGKILL);
            }
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
        // A std command: goodflows reaps the child itself with `wait4` (for its
        // rusage), so tokio must never wait on, try_wait or kill this pid.
        let mut command = if shell {
            let mut c = std::process::Command::new("sh");
            c.arg("-c").arg(script);
            c
        } else {
            let args = shell_words::split(script)?;
            let mut c = std::process::Command::new(&args[0]);
            c.args(&args[1..]);
            c
        };
        command
            .current_dir(self.workdir)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
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
        let spawn_start = Instant::now();
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn '{}'", job.name))?;
        let pid = child.id();
        // The guard and the reaper exist before anything else can fail, so an
        // early return still kills the group (and a child that left it) and
        // reaps the child.
        let reaped_flag = Arc::new(Mutex::new(false));
        let mut group = ProcessGroup {
            id: Some(pid),
            reaped: reaped_flag.clone(),
        };
        let mut reaper = spawn_reaper(pid, reaped_flag);
        if !is_condition {
            self.spawned.store(true, Ordering::Relaxed);
        }
        let rss = self.rss.register(pid as i32);
        let mut readers = JoinSet::new();
        // The fields every `job_output` event of this command shares.
        let mut source = Partition::tag(
            self.partition,
            json!({"type":"job_output", "name":job.name}),
        );
        if let Some(step) = step {
            source["step"] = json!(step);
        }
        let pipe = receiver(child.stdout.take().context("missing stdout pipe")?.into())?;
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
            self.rss.clone(),
        ));
        let pipe = receiver(child.stderr.take().context("missing stderr pipe")?.into())?;
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
            self.rss.clone(),
        ));
        let mut reaped: Option<Rusage> = None;

        let mut result = loop {
            tokio::select! {
                biased;
                _ = async {
                    if cancel.borrow().is_none() { let _ = cancel.changed().await; }
                } => break CommandResult::new(Status::Cancelled, Some("cancelled by signal".into())),
                waited = &mut reaper => {
                    let (status, usage) = waited
                        .context("child reaper stopped")?
                        .context("failed to wait for child")?;
                    reaped = Some(usage);
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
        // Stop descendants before draining pipes: they may otherwise keep the
        // pipes open forever. This also kills a child that left its group.
        group.kill();
        let usage = match reaped {
            Some(usage) => usage,
            None => {
                reaper
                    .await
                    .context("child reaper stopped")?
                    .context("failed to reap child")?
                    .1
            }
        };
        let wall_ms = spawn_start.elapsed().as_millis() as u64;
        let peak_rss_bytes = rss.finish();
        self.commands
            .lock()
            .expect("command cost mutex poisoned")
            .push(CommandCost {
                kind: if is_condition {
                    CommandKind::Condition
                } else {
                    CommandKind::Command
                },
                step: step.map(str::to_owned),
                wall_ms,
                cpu_user_ms: Some(usage.user_ms),
                cpu_system_ms: Some(usage.system_ms),
                peak_rss_bytes,
            });
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

/// User and system CPU of a reaped child, including descendants it waited for.
#[derive(Clone, Copy, Debug)]
struct Rusage {
    user_ms: u64,
    system_ms: u64,
}

/// Block until `pid` exits and reap it with `wait4`, keeping its rusage.
/// `waitid(WNOWAIT)` first waits without reaping, so the exited child stays a
/// zombie; the reap and the `reaped` flag then happen together under the lock
/// a late `kill(pid)` takes, so that kill can never reach a reused pid.
fn reap(pid: u32, reaped: &Mutex<bool>) -> std::io::Result<(std::process::ExitStatus, Rusage)> {
    use std::os::unix::process::ExitStatusExt;
    loop {
        // SAFETY: siginfo_t is plain old data; waitid fills it on success.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out-pointer for this call only; the pid is our unreaped child.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    let mut reaped = reaped.lock().unwrap_or_else(|e| e.into_inner());
    let mut status = 0;
    // SAFETY: rusage is plain old data; wait4 fills it on success.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: valid out-pointers for this call only; the pid is our unreaped child.
        let result = unsafe { libc::wait4(pid as libc::pid_t, &mut status, 0, &mut usage) };
        if result == pid as libc::pid_t {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    *reaped = true;
    drop(reaped);
    let ms = |time: libc::timeval| time.tv_sec as u64 * 1000 + time.tv_usec as u64 / 1000;
    Ok((
        std::process::ExitStatus::from_raw(status),
        Rusage {
            user_ms: ms(usage.ru_utime),
            system_ms: ms(usage.ru_stime),
        },
    ))
}

type Reaped = std::io::Result<(std::process::ExitStatus, Rusage)>;

/// The reaper's stack: `reap` needs only a few hundred bytes.
const REAPER_STACK: usize = 64 * 1024;

/// Reap `pid` on a dedicated thread, so a blocked `waitid` never holds one
/// of tokio's blocking-pool threads, which log writes and RSS scans share.
/// Without a thread to spare, fall back to the blocking pool.
fn spawn_reaper(pid: u32, reaped: Arc<Mutex<bool>>) -> tokio::sync::oneshot::Receiver<Reaped> {
    type Task = Arc<Mutex<Option<(tokio::sync::oneshot::Sender<Reaped>, Arc<Mutex<bool>>)>>>;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task: Task = Arc::new(Mutex::new(Some((sender, reaped))));
    let run = |task: Task| {
        move || {
            let taken = task.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some((sender, reaped)) = taken {
                let _ = sender.send(reap(pid, &reaped));
            }
        }
    };
    let thread = std::thread::Builder::new()
        .name("goodflows-reaper".into())
        .stack_size(REAPER_STACK)
        .spawn(run(task.clone()));
    if thread.is_err() {
        tokio::task::spawn_blocking(run(task));
    }
    receiver
}

fn receiver(fd: std::os::fd::OwnedFd) -> Result<tokio::net::unix::pipe::Receiver> {
    tokio::net::unix::pipe::Receiver::from_owned_fd(fd).context("failed to read child output pipe")
}

/// The slow poll while commands are quiet, and the minimum gap between
/// scans requested by output reads (a `/proc` scan per 4 KiB read is too costly).
const RSS_POLL: Duration = Duration::from_millis(200);
const RSS_ON_OUTPUT: Duration = Duration::from_millis(50);

/// Peak sampled resident set sums of the process groups of running commands.
/// One sampler serves the whole run: each scan reads `/proc` once, on the
/// blocking pool rather than a runtime worker, and updates every registered
/// group. Linux only; elsewhere no scan is ever taken and peaks are omitted.
struct RssMonitor {
    /// Registered groups and their peak so far (0 until a live sample).
    peaks: Mutex<HashMap<i32, u64>>,
    wake: tokio::sync::Notify,
}

/// Aborts the run's sampler task when dropped.
struct RssTask(Option<tokio::task::JoinHandle<()>>);

impl Drop for RssTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl RssMonitor {
    fn start() -> (Arc<Self>, RssTask) {
        let monitor = Arc::new(Self {
            peaks: Mutex::new(HashMap::new()),
            wake: tokio::sync::Notify::new(),
        });
        let task = cfg!(target_os = "linux").then(|| tokio::spawn(monitor.clone().sample()));
        (monitor, RssTask(task))
    }

    /// Track `group` until the returned guard finishes or drops. A new group
    /// is sampled as soon as the minimum gap allows.
    fn register(self: &Arc<Self>, group: i32) -> RssGroup {
        self.lock().insert(group, 0);
        self.wake.notify_one();
        RssGroup {
            monitor: self.clone(),
            group,
        }
    }

    /// Ask for a scan soon; output reads call this.
    fn request(&self) {
        self.wake.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i32, u64>> {
        self.peaks.lock().expect("rss mutex poisoned")
    }

    /// Scan on request (at most every `RSS_ON_OUTPUT`) and otherwise every
    /// `RSS_POLL` while any group is registered.
    async fn sample(self: Arc<Self>) {
        let mut last: Option<Instant> = None;
        loop {
            if self.lock().is_empty() {
                self.wake.notified().await;
            } else {
                tokio::select! {
                    _ = self.wake.notified() => {}
                    _ = tokio::time::sleep(RSS_POLL) => {}
                }
            }
            if let Some(gap) = last.and_then(|last| RSS_ON_OUTPUT.checked_sub(last.elapsed())) {
                tokio::time::sleep(gap).await;
            }
            last = Some(Instant::now());
            let wanted: Vec<i32> = self.lock().keys().copied().collect();
            if wanted.is_empty() {
                continue;
            }
            let Ok(sums) = tokio::task::spawn_blocking(move || group_rss(&wanted)).await else {
                continue;
            };
            let mut peaks = self.lock();
            for (group, sum) in sums {
                // A group that finished during the scan is no longer registered.
                if let Some(peak) = peaks.get_mut(&group) {
                    *peak = (*peak).max(sum);
                }
            }
        }
    }
}

/// One command's registration with the run's `RssMonitor`.
struct RssGroup {
    monitor: Arc<RssMonitor>,
    group: i32,
}

impl RssGroup {
    /// Stop tracking and return the peak, `None` when no sample saw a live,
    /// resident process. A scan still running may miss this command.
    fn finish(self) -> Option<u64> {
        self.monitor
            .lock()
            .remove(&self.group)
            .filter(|&peak| peak > 0)
    }
}

impl Drop for RssGroup {
    fn drop(&mut self) {
        self.monitor.lock().remove(&self.group);
    }
}

/// The process group and resident bytes of one `/proc/<pid>/stat` line, or
/// `None` for a zombie or dead process or one with nothing resident (caught
/// starting or exiting), so such a sample never records a peak of 0.
#[cfg(any(target_os = "linux", test))]
fn stat_rss(stat: &str, page: u64) -> Option<(i32, u64)> {
    // Fields after the parenthesized command name, which may contain spaces:
    // state(3) ppid(4) pgrp(5) ... rss(24, in pages).
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    if fields.len() < 22 || matches!(fields[0], "Z" | "X" | "x") {
        return None;
    }
    let group = fields[2].parse().ok()?;
    let pages = fields[21].parse::<u64>().ok().filter(|&pages| pages > 0)?;
    Some((group, pages * page))
}

/// Summed resident bytes per wanted group from one `/proc` scan. A group
/// with no live, resident process is absent.
#[cfg(target_os = "linux")]
fn group_rss(wanted: &[i32]) -> HashMap<i32, u64> {
    // SAFETY: sysconf takes only an integer argument.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    let mut sums = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return sums;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some((group, bytes)) = stat_rss(&stat, page) {
            if wanted.contains(&group) {
                *sums.entry(group).or_insert(0) += bytes;
            }
        }
    }
    sums
}

#[cfg(not(target_os = "linux"))]
fn group_rss(_wanted: &[i32]) -> HashMap<i32, u64> {
    HashMap::new()
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

/// Create a new log file, private to the invoking user, inside this run's
/// directory. An existing entry (never one of this run's own) or a symlink
/// is refused rather than followed or truncated.
async fn open_log(dir: Option<&Path>, filename: &str) -> Result<Option<tokio::fs::File>> {
    match dir {
        Some(dir) => {
            let path = dir.join(filename);
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .await
                .with_context(|| format!("failed to create log {}", path.display()))?;
            Ok(Some(file))
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
    rss: Arc<RssMonitor>,
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
        rss.request();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slots_count_only_admitted_units() {
        let root = Scope::root(2, true);
        let first = root.try_acquire().unwrap();
        let second = root.try_acquire().unwrap();
        assert!(root.try_acquire().is_none());
        drop(first);
        // A dropped slot no longer counts. The scheduler drops a unit's slot
        // only after recording its result, so until then the unit counts.
        let third = root.try_acquire().unwrap();
        assert_eq!(root.now.load(Ordering::SeqCst), 2);
        // Holding a slot is not executing: `max_in_flight` is unaffected.
        assert_eq!(root.max.load(Ordering::SeqCst), 0);
        drop((second, third));
        assert_eq!(root.now.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn in_flight_counts_only_units_inside_run_job() {
        let root = Scope::root(4, true);
        let child = Scope::child(&root, 4, true);
        let first = child.enter();
        let second = root.enter();
        drop(first);
        // A unit that left `run_job` no longer counts, even before its
        // result is recorded and its slot frees.
        let third = child.enter();
        assert_eq!(root.max.load(Ordering::SeqCst), 2);
        assert_eq!(child.max.load(Ordering::SeqCst), 1);
        drop((second, third));
        assert_eq!(root.inside.load(Ordering::SeqCst), 0);
        assert_eq!(child.inside.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn child_slots_count_against_every_containing_scope() {
        let root = Scope::root(3, false);
        let child = Scope::child(&root, 1, true);
        let grandchild = Scope::child(&child, 4, false);
        let a = grandchild.try_acquire().unwrap();
        // The child's limit of one caps its whole subtree.
        assert!(child.try_acquire().is_none());
        assert!(grandchild.try_acquire().is_none());
        // Siblings still share the root's remaining slots.
        let b = root.try_acquire().unwrap();
        let c = root.try_acquire().unwrap();
        assert!(root.try_acquire().is_none());
        assert_eq!(
            [&root, &child, &grandchild].map(|s| s.now.load(Ordering::SeqCst)),
            [3, 1, 1]
        );
        drop((a, b, c));
        assert_eq!(root.now.load(Ordering::SeqCst), 0);
        // Failures reach every containing scope; only enabled policies stop work.
        grandchild.fail();
        assert!(root.failed.load(Ordering::SeqCst));
        assert!(grandchild.stopped() && child.stopped());
        assert!(!root.stopped());
        let sibling = Scope::child(&root, 1, true);
        assert!(!sibling.stopped());
    }

    #[test]
    fn stat_rss_skips_dead_and_empty_processes() {
        let line = |state: &str, rss: &str| {
            format!("42 (a (b) c) {state} 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 100 4096 {rss} 0")
        };
        assert_eq!(stat_rss(&line("S", "3"), 4096), Some((7, 3 * 4096)));
        assert_eq!(stat_rss(&line("R", "1"), 100), Some((7, 100)));
        for state in ["Z", "X", "x"] {
            assert_eq!(stat_rss(&line(state, "3"), 4096), None, "{state}");
        }
        assert_eq!(stat_rss(&line("S", "0"), 4096), None);
        assert_eq!(stat_rss("42 (truncated", 4096), None);
        assert_eq!(stat_rss("42 (a) S 1 7", 4096), None);
    }

    #[tokio::test]
    async fn rss_monitor_samples_registered_groups_only() {
        let (monitor, task) = RssMonitor::start();
        // SAFETY: getpgrp has no preconditions.
        let own = monitor.register(unsafe { libc::getpgrp() });
        // No process lives in group i32::MAX.
        let empty = monitor.register(i32::MAX);
        let deadline = Instant::now() + Duration::from_secs(5);
        while cfg!(target_os = "linux")
            && monitor.lock().values().all(|&peak| peak == 0)
            && Instant::now() < deadline
        {
            monitor.request();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let peak = own.finish();
        if cfg!(target_os = "linux") {
            assert!(peak.is_some_and(|bytes| bytes > 0), "{peak:?}");
        } else {
            assert!(peak.is_none());
        }
        assert_eq!(empty.finish(), None);
        // Finished and dropped registrations are gone.
        let dropped = monitor.register(1);
        drop(dropped);
        assert!(monitor.lock().is_empty());
        drop(task);
    }
}
