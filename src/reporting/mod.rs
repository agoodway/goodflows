//! Opt-in collection and publishing of per-job test timing reports.
//!
//! Capture and delivery are separate: each reporting invocation gets a
//! persistent spool directory, report-producing jobs write JSONL into their own
//! isolated directory, and only after every job has finished are those files
//! frozen into immutable request snapshots and published. See README
//! "Test timing reports" for the on-disk layout.

pub mod collect;
pub mod contract;
pub mod publish;
pub mod replay;
mod source;

use crate::config::{self, Job, Partition, Reporting};
use crate::fsutil::{dir_builder, real_dir, write_atomic};
use crate::output::Output;
use crate::runner::{JobResult, ReportingEnv, ReportingKey, Status};
use anyhow::Result;
use collect::{Budget, Collection, Head, JobContext, Sink};
use publish::{Credentials, Limits};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub use source::{capture as capture_source, Source};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;

pub const MANIFEST: &str = "manifest.json";
/// Version of manifests without partition entries, readable by every goodflows
/// that supports replay.
pub const MANIFEST_VERSION: u32 = 1;
/// Version of manifests with at least one partition entry. Goodflows releases
/// that predate partitions refuse it with a clear version error rather than a
/// schema error.
pub const PARTITIONED_MANIFEST_VERSION: u32 = 2;
/// Acknowledgement and rejection markers are tiny; anything larger is not ours.
const MAX_MARKER_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    /// Jobs are running or collection has not finished. Replay refuses this state.
    Running,
    /// Every snapshot listed in the manifest was written before this state.
    Finalized,
    /// The scheduler failed; nothing was collected.
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub state: ManifestState,
    pub workflow_run_id: String,
    pub project: String,
    pub workflow: String,
    pub environment: Option<String>,
    pub url_env: String,
    pub token_env: String,
    pub source: Source,
    pub jobs: Vec<ManifestJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestJob {
    pub name: String,
    /// Reversibly encoded directory name under the run directory; always
    /// `config::path_component(name)`.
    pub dir: String,
    /// The instance of a partitioned job. Its state lives in `<dir>/p<index>/`,
    /// derived from this value and never read as a path. Absent for
    /// unpartitioned jobs, so their manifests keep the original layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<ManifestPartition>,
    pub status: Option<String>,
    pub executed: bool,
    pub collection: Option<ManifestCollection>,
}

impl ManifestJob {
    /// The instance this entry describes, for paths, events and expectations.
    pub fn instance(&self) -> Option<Partition> {
        self.partition.map(Partition::from)
    }
}

/// The on-disk form of an instance's identity. Kept apart from
/// [`config::Partition`] so the saved schema (strict, versioned) does not
/// change when the in-memory type does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestPartition {
    pub index: u32,
    pub count: u32,
}

impl From<Partition> for ManifestPartition {
    fn from(partition: Partition) -> Self {
        Self {
            index: partition.index,
            count: partition.count,
        }
    }
}

impl From<ManifestPartition> for Partition {
    fn from(partition: ManifestPartition) -> Self {
        Self {
            index: partition.index,
            count: partition.count,
        }
    }
}

/// The version a manifest with these entries is written as (and must carry).
pub fn manifest_version(jobs: &[ManifestJob]) -> u32 {
    if jobs.iter().any(|job| job.partition.is_some()) {
        PARTITIONED_MANIFEST_VERSION
    } else {
        MANIFEST_VERSION
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCollection {
    pub outcome: String,
    pub rejected: BTreeMap<String, usize>,
    pub reports: Vec<ManifestReport>,
}

/// Snapshot `<base>/pending/NNN.json`, acknowledgement `<base>/acks/NNN.json`
/// and rejection marker `<base>/acks/NNN.rejected.json` are derived from the
/// job directory (`<base>` is `<job>` or, for an instance, `<job>/p<index>`)
/// and this index only; the manifest never names a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestReport {
    pub report_id: String,
    pub index: u32,
}

/// A terminal server refusal (400, 409, 413, 415, 422) of one frozen snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rejection {
    pub report_id: String,
    pub reason: String,
    pub http_status: u16,
}

/// Map a runner result to the server's job_status vocabulary.
pub fn job_status(status: Option<Status>) -> &'static str {
    match status {
        Some(Status::Passed) => "passed",
        Some(Status::Failed) => "failed",
        Some(Status::TimedOut) => "timed_out",
        Some(Status::Cancelled) => "cancelled",
        Some(Status::Skipped) => "skipped",
        None => "unknown",
    }
}

/// On Linux, stop other processes of the same user from reading this
/// process's memory or `/proc/<pid>/environ` (which still holds the key the
/// CLI inherited). The flag is reset by `execve`, so jobs are unaffected.
pub fn protect_process() {
    #[cfg(target_os = "linux")]
    // SAFETY: PR_SET_DUMPABLE takes only integer arguments and touches no memory.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

pub fn write_manifest(run_dir: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    write_atomic(&run_dir.join(MANIFEST), &bytes, true)
}

/// Create a private directory, or confirm an existing one is a real directory.
fn private_dir(path: &Path) -> std::io::Result<()> {
    match dir_builder(false).create(path) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(error),
        _ => real_dir(path),
    }
}

/// One job's (or one instance's) state directories inside a run directory.
#[derive(Debug, Clone)]
pub struct JobDirs {
    job: PathBuf,
    /// `job` itself, or `job/p<index>` for an instance of a partitioned job.
    base: PathBuf,
}

impl JobDirs {
    pub fn new(run_dir: &Path, encoded: &str, partition: Option<Partition>) -> Self {
        let job = run_dir.join(encoded);
        let base = match partition {
            Some(partition) => job.join(instance_dir(partition)),
            None => job.clone(),
        };
        Self { job, base }
    }

    fn reports(&self) -> PathBuf {
        self.base.join("reports")
    }

    fn pending(&self) -> PathBuf {
        self.base.join("pending")
    }

    fn acks(&self) -> PathBuf {
        self.base.join("acks")
    }

    /// Neither the job directory nor an instance directory may be a symlink.
    fn check_base(&self) -> std::io::Result<()> {
        real_dir(&self.job)?;
        if self.base != self.job {
            real_dir(&self.base)?;
        }
        Ok(())
    }

    pub fn snapshot(&self, index: u32) -> PathBuf {
        self.pending().join(format!("{index:03}.json"))
    }

    pub fn ack(&self, index: u32) -> PathBuf {
        self.acks().join(format!("{index:03}.json"))
    }

    pub fn rejection(&self, index: u32) -> PathBuf {
        self.acks().join(format!("{index:03}.rejected.json"))
    }

    /// Neither the job/instance directories nor `pending/` may have been replaced by a symlink.
    fn check_pending(&self) -> std::io::Result<()> {
        self.check_base()?;
        real_dir(&self.pending())
    }

    /// The job/instance directories must be real; `acks/` is (re)created privately if missing.
    fn ensure_acks(&self) -> std::io::Result<()> {
        self.check_base()?;
        private_dir(&self.acks())
    }

    /// Read a small marker file (never through a symlink).
    fn read_marker<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Option<T> {
        self.check_base().ok()?;
        real_dir(&self.acks()).ok()?;
        let bytes = collect::read_bounded(path, MAX_MARKER_BYTES).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn read_snapshot(&self, index: u32) -> Result<Vec<u8>, &'static str> {
        self.check_pending().map_err(|_| "invalid_snapshot")?;
        collect::read_bounded(&self.snapshot(index), collect::MAX_REQUEST_BYTES as u64)
            .map_err(|_| "invalid_snapshot")
    }

    fn record<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        self.ensure_acks()?;
        write_atomic(path, &serde_json::to_vec(value)?, true)
    }
}

/// Freezes request bodies into a job's `pending/` directory as they are built.
struct SpoolSink {
    dirs: JobDirs,
    next: u32,
}

impl Sink for SpoolSink {
    fn freeze(&mut self, body: &[u8]) -> Result<u32, ()> {
        let index = self.next;
        self.next += 1;
        self.dirs.check_pending().map_err(drop)?;
        write_atomic(&self.dirs.snapshot(index), body, false).map_err(drop)?;
        Ok(index)
    }

    fn discard(&mut self, index: u32) -> Result<(), ()> {
        self.dirs.check_pending().map_err(drop)?;
        match std::fs::remove_file(self.dirs.snapshot(index)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(()),
            _ => Ok(()),
        }
    }
}

/// The instance directory name under a partitioned job's directory.
fn instance_dir(partition: Partition) -> String {
    format!("p{}", partition.index)
}

struct Run {
    dir: PathBuf,
    manifest: Manifest,
    env: HashMap<ReportingKey, Vec<(String, String)>>,
}

/// Reporting state for one `run` invocation.
pub struct Session {
    token_env: String,
    url: Option<String>,
    key: Option<String>,
    run: Option<Run>,
    skipped: Option<&'static str>,
    /// The source captured for the spool manifest, when one was created.
    source: Option<Source>,
}

/// Reporting events are best-effort: a failed write (for example a closed
/// stdout) must never change the check result or leave the run unfinalized.
async fn emit(output: &Output, event: Value) {
    let _ = output.emit(event).await;
}

impl Session {
    /// `run_id` is the invocation's run id, used as the `workflow_run_id`.
    pub async fn start(
        reporting: &Reporting,
        run_id: &str,
        workflow: &str,
        environment: Option<&str>,
        jobs: &[Job],
        workdir: &Path,
    ) -> Self {
        // Capture credentials once; the publisher keeps them in this process only.
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let mut session = Self {
            token_env: reporting.token_env.clone(),
            url: read(&reporting.url_env),
            key: read(&reporting.token_env),
            run: None,
            skipped: None,
            source: None,
        };
        if session.key.is_some() {
            protect_process();
        }
        let reporting_jobs: Vec<_> = jobs.iter().filter(|j| !j.reports.is_empty()).collect();
        if reporting_jobs.is_empty() {
            return session;
        }
        let source = source::capture(workdir, Some(&reporting.token_env)).await;
        session.source = Some(source.clone());
        match create_run(
            reporting,
            run_id,
            workflow,
            environment,
            &reporting_jobs,
            source,
        ) {
            Ok(run) => session.run = Some(run),
            Err(_) => session.skipped = Some("spool_unavailable"),
        }
        session
    }

    /// The source context captured at start, if report-producing jobs made
    /// the session capture one.
    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    pub fn runner_env(&self) -> Arc<ReportingEnv> {
        Arc::new(ReportingEnv {
            active: true,
            scrub: Some(self.token_env.clone()),
            jobs: self
                .run
                .as_ref()
                .map(|run| run.env.clone())
                .unwrap_or_default(),
        })
    }

    pub async fn announce(&self, output: &Output) {
        if let Some(reason) = self.skipped {
            emit(
                output,
                json!({"type":"reporting_skipped", "run_id":null, "reason":reason}),
            )
            .await;
        }
    }

    /// The scheduler failed without results: record that and collect nothing.
    pub fn abort(&mut self) {
        if let Some(run) = &mut self.run {
            run.manifest.state = ManifestState::Aborted;
            let _ = write_manifest(&run.dir, &run.manifest);
        }
    }

    /// Collect, freeze and (unless cancelled) publish reports. Never changes check results.
    pub async fn finish(
        &mut self,
        results: &[JobResult],
        cancel: watch::Receiver<Option<i32>>,
        output: &Output,
    ) {
        self.finish_with(results, cancel, output, Limits::default())
            .await
    }

    async fn finish_with(
        &mut self,
        results: &[JobResult],
        cancel: watch::Receiver<Option<i32>>,
        output: &Output,
        limits: Limits,
    ) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        let find = |name: &str, partition: Option<Partition>| {
            results
                .iter()
                .find(|r| r.name == name && r.partition == partition)
        };
        for job in &mut run.manifest.jobs {
            let result = find(&job.name, job.instance());
            job.status = Some(job_status(result.map(|r| r.status)).into());
            job.executed = result.is_some_and(|r| r.executed);
        }
        // Persist terminal results before reading any producer output.
        let _ = write_manifest(&run.dir, &run.manifest);
        // A signal that ended the jobs still lets collection freeze what exists;
        // a signal that arrives during collection stops it between reports.
        let signalled_before = cancel.borrow().is_some();
        let watcher = cancel.clone();
        let stop = move || !signalled_before && watcher.borrow().is_some();
        // Reading and parsing is blocking work; keep it off the async workers.
        let Ok((run, items)) = tokio::task::spawn_blocking(move || {
            let items = collect_run(&mut run, &stop);
            (run, items)
        })
        .await
        else {
            return;
        };
        let run_id = run.manifest.workflow_run_id.clone();
        let mut manifest = run.manifest;
        manifest.state = ManifestState::Finalized;
        // Finalize before emitting anything, so an output failure cannot leave the run unreplayable.
        let finalized = write_manifest(&run.dir, &manifest).is_ok();
        for job in &manifest.jobs {
            let Some(collection) = &job.collection else {
                continue;
            };
            emit(
                output,
                Partition::tag(
                    job.instance(),
                    json!({
                        "type":"reports_collected",
                        "run_id":run_id,
                        "run_dir":run.dir,
                        "job":job.name,
                        "outcome":collection.outcome,
                        "reports":collection.reports.len(),
                        "report_ids":collection.reports.iter().map(|r| &r.report_id).collect::<Vec<_>>(),
                        "rejected":collection.rejected,
                    }),
                ),
            )
            .await;
        }
        if !finalized {
            // Without a finalized manifest nothing is replayable; keep everything local.
            emit(output, json!({"type":"reporting_skipped", "run_id":run_id, "reason":"spool_unavailable", "pending":items.len()})).await;
            return;
        }
        if items.is_empty() {
            return;
        }
        let credentials =
            publish::resolve(self.url.as_deref(), self.key.as_deref(), &manifest.project);
        deliver(&run_id, items, credentials, limits, cancel, output).await;
    }
}

/// Collect every executed job (or instance) of a run into its pending area.
/// Instances of one job share the job's collection budget, in index order, and
/// `report_id` uniqueness is enforced across all of them. Blocking.
fn collect_run(run: &mut Run, stop: &dyn Fn() -> bool) -> Vec<Item> {
    let Manifest {
        workflow_run_id,
        workflow,
        environment,
        source,
        jobs,
        ..
    } = &mut run.manifest;
    let mut items = Vec::new();
    // `create_run` lists a job's instances consecutively, in index order, so
    // each job is one run of entries sharing one budget.
    debug_assert!(
        grouped_in_index_order(jobs),
        "manifest entries are not grouped"
    );
    for group in jobs.chunk_by_mut(|a, b| a.name == b.name) {
        let mut budget = Budget::default();
        let mut instances = Vec::with_capacity(group.len());
        for job in group.iter() {
            let partition = job.instance();
            let dirs = JobDirs::new(&run.dir, &job.dir, partition);
            let job_status = job.status.clone().unwrap_or_else(|| "unknown".into());
            let mut sink = SpoolSink {
                dirs: dirs.clone(),
                next: 0,
            };
            let collection = if !job.executed {
                Collection::not_executed()
            } else if dirs.check_base().is_err() {
                let mut collection = Collection::empty("missing_directory");
                collection.reject("not_directory");
                collection
            } else {
                let ctx = JobContext {
                    workflow_run_id,
                    workflow,
                    environment: environment.as_deref(),
                    job: &job.name,
                    job_status: &job_status,
                    source,
                    partition,
                };
                collect::collect_dir(&dirs.reports(), &ctx, &mut sink, stop, &mut budget)
            };
            instances.push((collection, sink));
        }
        collect::resolve_duplicates(&mut instances);
        for (job, (collection, sink)) in group.iter_mut().zip(instances) {
            let partition = job.instance();
            let job_status = job.status.clone().unwrap_or_else(|| "unknown".into());
            let reports: Vec<_> = collection
                .reports
                .into_iter()
                .map(|frozen| ManifestReport {
                    report_id: frozen.report_id,
                    index: frozen.index,
                })
                .collect();
            for report in &reports {
                items.push(Item {
                    job: job.name.clone(),
                    partition,
                    report_id: report.report_id.clone(),
                    index: report.index,
                    dirs: sink.dirs.clone(),
                    expected: Head {
                        workflow_run_id: workflow_run_id.clone(),
                        workflow: workflow.clone(),
                        environment: environment.clone(),
                        job: job.name.clone(),
                        job_status: job_status.clone(),
                        source: source.clone(),
                        report_id: report.report_id.clone(),
                        partition: partition.map(|p| p.index.to_string()),
                        partition_count: partition.map(|p| u64::from(p.count)),
                    },
                });
            }
            job.collection = Some(ManifestCollection {
                outcome: collection.outcome.into(),
                rejected: collection.rejected,
                reports,
            });
        }
    }
    items
}

/// Every job name forms one contiguous run of entries whose partition indexes
/// ascend, as `create_run` writes them.
fn grouped_in_index_order(jobs: &[ManifestJob]) -> bool {
    let mut seen = std::collections::HashSet::new();
    jobs.chunk_by(|a, b| a.name == b.name).all(|group| {
        seen.insert(group[0].name.as_str())
            && group.windows(2).all(|pair| {
                matches!(
                    (pair[0].partition, pair[1].partition),
                    (Some(a), Some(b)) if a.index < b.index
                )
            })
    })
}

fn create_run(
    reporting: &Reporting,
    id: &str,
    workflow: &str,
    environment: Option<&str>,
    jobs: &[&Job],
    source: Source,
) -> Result<Run> {
    let spool = reporting.spool_dir();
    dir_builder(true).create(spool)?;
    let id = id.to_owned();
    let dir = spool.join(&id);
    // A non-recursive create fails rather than sharing a directory with another invocation.
    dir_builder(false).create(&dir)?;
    let mut env = HashMap::new();
    let mut manifest_jobs = Vec::new();
    for job in jobs {
        let encoded = config::path_component(&job.name);
        let job_dir = dir.join(&encoded);
        // Every state directory exists (privately) before any job or instance
        // can run, so none of them can be pre-created as a symlink.
        dir_builder(false).create(&job_dir)?;
        for partition in job.partition_ids() {
            let base = match partition {
                Some(partition) => {
                    let base = job_dir.join(instance_dir(partition));
                    dir_builder(false).create(&base)?;
                    base
                }
                None => job_dir.clone(),
            };
            for sub in ["reports", "pending", "acks"] {
                dir_builder(false).create(base.join(sub))?;
            }
            env.insert(
                (job.name.clone(), partition),
                vec![
                    ("GOODFLOWS_RUN_ID".to_owned(), id.clone()),
                    (
                        "GOODFLOWS_REPORT_DIR".to_owned(),
                        base.join("reports").to_string_lossy().into_owned(),
                    ),
                ],
            );
            manifest_jobs.push(ManifestJob {
                name: job.name.clone(),
                dir: encoded.clone(),
                partition: partition.map(ManifestPartition::from),
                status: None,
                executed: false,
                collection: None,
            });
        }
    }
    let manifest = Manifest {
        version: manifest_version(&manifest_jobs),
        state: ManifestState::Running,
        workflow_run_id: id,
        project: reporting.project.clone(),
        workflow: workflow.to_owned(),
        environment: environment.map(str::to_owned),
        url_env: reporting.url_env.clone(),
        token_env: reporting.token_env.clone(),
        source,
        jobs: manifest_jobs,
    };
    write_manifest(&dir, &manifest)?;
    Ok(Run { dir, manifest, env })
}

/// A frozen request ready for delivery. Only its location and expected
/// identity are held; the body is read and re-validated at send time, so at
/// most one body is in memory.
pub struct Item {
    pub job: String,
    pub partition: Option<Partition>,
    pub report_id: String,
    pub index: u32,
    pub dirs: JobDirs,
    pub expected: Head,
}

impl Item {
    /// Read the snapshot and confirm it is a contract-valid request for exactly this report.
    fn load(&self) -> Result<Vec<u8>, &'static str> {
        let body = self.dirs.read_snapshot(self.index)?;
        match collect::validate_envelope(&body) {
            Ok(mut head) => {
                // Unpartitioned jobs keep whatever partition the producer reported;
                // an instance's snapshot must carry exactly its own identity.
                if self.partition.is_none() {
                    head.partition.clone_from(&self.expected.partition);
                    head.partition_count = self.expected.partition_count;
                }
                if head == self.expected {
                    Ok(body)
                } else {
                    Err("invalid_snapshot")
                }
            }
            Err(_) => Err("invalid_snapshot"),
        }
    }
}

#[derive(Default, Debug)]
pub struct Delivery {
    pub uploaded: usize,
    pub pending: usize,
    /// Permanently refused by the server and recorded as such.
    pub rejected: usize,
    /// Snapshots that failed local re-validation and were not sent.
    pub invalid: usize,
}

/// Send frozen snapshots sequentially within one shared budget and record acknowledgements.
pub async fn deliver(
    run_id: &str,
    items: Vec<Item>,
    credentials: Credentials,
    limits: Limits,
    cancel: watch::Receiver<Option<i32>>,
    output: &Output,
) -> Delivery {
    let mut delivery = Delivery::default();
    let pending = items.len();
    let skip = match &credentials {
        _ if cancel.borrow().is_some() => None,
        Credentials::Ready(_) => None,
        Credentials::Missing => Some("credentials_unavailable"),
        Credentials::InvalidUrl => Some("invalid_url"),
        Credentials::InvalidProject => Some("invalid_project"),
    };
    if let Some(reason) = skip {
        emit(output, json!({"type":"reporting_skipped", "run_id":run_id, "reason":reason, "pending":pending})).await;
        delivery.pending = pending;
        return delivery;
    }
    let mut publisher = match credentials {
        Credentials::Ready(target) if cancel.borrow().is_none() => {
            publish::Publisher::new(target, limits, cancel.clone()).ok()
        }
        _ => None,
    };
    for item in items {
        let pending = |reason: &str, http_status: Option<u16>| {
            Partition::tag(
                item.partition,
                json!({"type":"report_upload_pending", "run_id":run_id, "job":item.job, "report_id":item.report_id, "reason":reason, "http_status":http_status}),
            )
        };
        let outcome = match &mut publisher {
            _ if cancel.borrow().is_some() => publish::Outcome::pending("cancelled", None),
            Some(publisher) if publisher.exhausted() => {
                publish::Outcome::pending("budget_exhausted", None)
            }
            Some(publisher) => match item.load() {
                Ok(body) => publisher.send(&body, &item.report_id).await,
                Err(reason) => {
                    delivery.invalid += 1;
                    emit(output, pending(reason, None)).await;
                    continue;
                }
            },
            None => publish::Outcome::pending("client_unavailable", None),
        };
        let event = match outcome {
            publish::Outcome::Uploaded(ack) => {
                match item.dirs.record(&item.dirs.ack(item.index), &ack) {
                    Ok(()) => {
                        delivery.uploaded += 1;
                        Partition::tag(
                            item.partition,
                            json!({"type":"report_uploaded", "run_id":run_id, "job":item.job, "report_id":item.report_id, "server_id":ack.id, "status":ack.status, "duplicate":ack.duplicate}),
                        )
                    }
                    Err(_) => {
                        delivery.pending += 1;
                        pending("state_write_failed", None)
                    }
                }
            }
            publish::Outcome::Rejected {
                reason,
                http_status,
            } => {
                // Recorded so replay does not resend bytes the server can never accept.
                // If the marker cannot be written, replay simply sends it again.
                let marker = Rejection {
                    report_id: item.report_id.clone(),
                    reason: reason.into(),
                    http_status,
                };
                let _ = item.dirs.record(&item.dirs.rejection(item.index), &marker);
                delivery.rejected += 1;
                let mut event = pending(reason, Some(http_status));
                event["terminal"] = json!(true);
                event
            }
            publish::Outcome::Pending {
                reason,
                http_status,
            } => {
                delivery.pending += 1;
                pending(reason, http_status)
            }
        };
        emit(output, event).await;
    }
    delivery
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_status_covers_every_result_and_unknown() {
        for (status, expected) in [
            (Some(Status::Passed), "passed"),
            (Some(Status::Failed), "failed"),
            (Some(Status::TimedOut), "timed_out"),
            (Some(Status::Cancelled), "cancelled"),
            (Some(Status::Skipped), "skipped"),
            (None, "unknown"),
        ] {
            assert_eq!(job_status(status), expected);
        }
    }

    #[test]
    fn job_paths_derive_from_directory_and_index_only() {
        let dirs = JobDirs::new(Path::new("/spool/run"), "test", None);
        assert_eq!(
            dirs.snapshot(7),
            Path::new("/spool/run/test/pending/007.json")
        );
        assert_eq!(dirs.ack(1000), Path::new("/spool/run/test/acks/1000.json"));
        assert_eq!(
            dirs.rejection(2),
            Path::new("/spool/run/test/acks/002.rejected.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_directories_refuse_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(run.join("test/pending")).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, run.join("test/acks")).unwrap();
        let dirs = JobDirs::new(&run, "test", None);
        assert!(dirs.check_pending().is_ok());
        assert!(dirs.record(&dirs.ack(0), &json!({})).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        // A missing acks directory is recreated privately.
        std::fs::remove_file(run.join("test/acks")).unwrap();
        dirs.record(&dirs.ack(0), &json!({})).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(run.join("test/acks"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // A job directory swapped for a symlink is refused too.
        std::fs::rename(run.join("test"), tmp.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("moved"), run.join("test")).unwrap();
        assert!(dirs.check_pending().is_err());
        assert!(dirs.record(&dirs.ack(1), &json!({})).is_err());
    }

    #[test]
    fn instance_paths_nest_under_the_job_directory() {
        let partition = Some(Partition { index: 2, count: 3 });
        let dirs = JobDirs::new(Path::new("/spool/run"), "test", partition);
        assert_eq!(dirs.reports(), Path::new("/spool/run/test/p2/reports"));
        assert_eq!(
            dirs.snapshot(0),
            Path::new("/spool/run/test/p2/pending/000.json")
        );
        assert_eq!(dirs.ack(3), Path::new("/spool/run/test/p2/acks/003.json"));
        assert_eq!(
            JobDirs::new(Path::new("/spool/run"), "test", None).reports(),
            Path::new("/spool/run/test/reports")
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_directories_refuse_symlinks_at_both_levels() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        let partition = Some(Partition { index: 1, count: 2 });
        for sub in ["pending", "acks"] {
            std::fs::create_dir_all(run.join("test/p1").join(sub)).unwrap();
        }
        let dirs = JobDirs::new(&run, "test", partition);
        assert!(dirs.check_pending().is_ok());
        // The instance directory swapped for a symlink.
        let moved = tmp.path().join("moved-instance");
        std::fs::rename(run.join("test/p1"), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, run.join("test/p1")).unwrap();
        assert!(dirs.check_pending().is_err());
        assert!(dirs.record(&dirs.ack(0), &json!({})).is_err());
        let mut sink = SpoolSink {
            dirs: dirs.clone(),
            next: 0,
        };
        assert!(sink.freeze(b"{}").is_err());
        assert_eq!(std::fs::read_dir(moved.join("pending")).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(moved.join("acks")).unwrap().count(), 0);
        // The job directory swapped for a symlink, with a real instance inside.
        std::fs::remove_file(run.join("test/p1")).unwrap();
        std::fs::rename(&moved, run.join("test/p1")).unwrap();
        assert!(dirs.check_pending().is_ok());
        std::fs::rename(run.join("test"), tmp.path().join("moved-job")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("moved-job"), run.join("test")).unwrap();
        assert!(dirs.check_pending().is_err());
        assert!(dirs.record(&dirs.ack(0), &json!({})).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn instance_state_directories_refuse_symlinks_on_their_own() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let base = run.join("test/p1");
        std::fs::create_dir_all(&base).unwrap();
        let dirs = JobDirs::new(&run, "test", Some(Partition { index: 1, count: 2 }));
        // Only acks/ is swapped: snapshots still freeze, acknowledgements do not.
        std::fs::create_dir(base.join("pending")).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("acks")).unwrap();
        let mut sink = SpoolSink {
            dirs: dirs.clone(),
            next: 0,
        };
        assert_eq!(sink.freeze(b"{}"), Ok(0));
        assert!(dirs.record(&dirs.ack(0), &json!({})).is_err());
        assert!(dirs.record(&dirs.rejection(0), &json!({})).is_err());
        assert_eq!(dir_entries(&outside), 0);
        // Only pending/ is swapped: nothing is frozen or discarded through it.
        std::fs::remove_file(base.join("acks")).unwrap();
        std::fs::create_dir(base.join("acks")).unwrap();
        std::fs::rename(base.join("pending"), tmp.path().join("moved-pending")).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("pending")).unwrap();
        assert!(sink.freeze(b"{}").is_err());
        assert!(dirs.read_snapshot(0).is_err());
        assert!(sink.discard(0).is_err());
        assert_eq!(dir_entries(&outside), 0);
        // Acknowledgements for the real instance still work.
        dirs.record(&dirs.ack(0), &json!({})).unwrap();
        assert_eq!(dir_entries(&base.join("acks")), 1);
    }

    #[cfg(unix)]
    #[test]
    fn discarding_reports_a_snapshot_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = JobDirs::new(tmp.path(), "test", Some(Partition { index: 1, count: 1 }));
        std::fs::create_dir_all(tmp.path().join("test/p1/pending")).unwrap();
        let mut sink = SpoolSink { dirs, next: 0 };
        let index = sink.freeze(b"{}").unwrap();
        assert_eq!(sink.discard(index), Ok(()));
        // Already gone counts as discarded.
        assert_eq!(sink.discard(index), Ok(()));
        // A snapshot path that cannot be unlinked (here a non-empty directory) remains.
        let stuck = sink.dirs.snapshot(7);
        std::fs::create_dir_all(stuck.join("x")).unwrap();
        assert_eq!(sink.discard(7), Err(()));
        assert!(stuck.exists());
    }

    fn dir_entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    /// A manifest exactly as written before partitions existed.
    const OLDER_MANIFEST: &str = r#"{
  "version": 1,
  "state": "finalized",
  "workflow_run_id": "67f7946d-136b-4608-ab01-e1f251a09754",
  "project": "goodcases",
  "workflow": "default",
  "environment": null,
  "url_env": "GOODFLOWS_URL",
  "token_env": "GOODFLOWS_API_KEY",
  "source": {"commit": null, "branch": null, "dirty": null},
  "jobs": [
    {
      "name": "test",
      "dir": "test",
      "status": "passed",
      "executed": true,
      "collection": {
        "outcome": "collected",
        "rejected": {},
        "reports": [{"report_id": "r1", "index": 0}]
      }
    }
  ]
}"#;

    #[test]
    fn older_manifests_deserialize_and_unpartitioned_entries_serialize_unchanged() {
        let manifest: Manifest = serde_json::from_str(OLDER_MANIFEST).unwrap();
        assert_eq!(manifest.jobs[0].partition, None);
        let original: Value = serde_json::from_str(OLDER_MANIFEST).unwrap();
        assert_eq!(serde_json::to_value(&manifest).unwrap(), original);
        // An instance entry records its partition.
        assert_eq!(manifest_version(&manifest.jobs), MANIFEST_VERSION);
        let mut instance = manifest.jobs[0].clone();
        instance.partition = Some(Partition { index: 2, count: 3 }.into());
        let value = serde_json::to_value(&instance).unwrap();
        assert_eq!(value["partition"], json!({"index": 2, "count": 3}));
        let back: ManifestJob = serde_json::from_value(value).unwrap();
        assert_eq!(back.partition, instance.partition);
        assert_eq!(back.instance(), Some(Partition { index: 2, count: 3 }));
        // Any partition entry makes the whole manifest version 2.
        let jobs = [manifest.jobs[0].clone(), instance];
        assert_eq!(manifest_version(&jobs), PARTITIONED_MANIFEST_VERSION);
    }

    #[test]
    fn grouping_requires_contiguous_jobs_with_ascending_indexes() {
        let manifest: Manifest = serde_json::from_str(OLDER_MANIFEST).unwrap();
        let entry = |name: &str, index: Option<u32>| {
            let mut job = manifest.jobs[0].clone();
            job.name = name.into();
            job.partition = index.map(|index| ManifestPartition { index, count: 3 });
            job
        };
        assert!(grouped_in_index_order(&[
            entry("a", None),
            entry("b", Some(1)),
            entry("b", Some(2)),
            entry("c", None),
        ]));
        assert!(grouped_in_index_order(&[]));
        assert!(!grouped_in_index_order(&[
            entry("b", Some(2)),
            entry("b", Some(1)),
        ]));
        assert!(!grouped_in_index_order(&[
            entry("b", Some(1)),
            entry("a", None),
            entry("b", Some(2)),
        ]));
        assert!(!grouped_in_index_order(&[
            entry("a", None),
            entry("a", None)
        ]));
    }
}
