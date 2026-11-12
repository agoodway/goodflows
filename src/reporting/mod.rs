//! Opt-in collection and publishing of per-job test timing reports.
//!
//! Capture and delivery are separate: each reporting invocation gets a
//! persistent spool directory, report-producing jobs write JSONL into their own
//! isolated directory, and only after every job has finished are those files
//! frozen into immutable request snapshots and published. See README
//! "Test timing reports" for the on-disk layout.

pub mod ci;
pub mod collect;
pub mod contract;
pub mod history_client;
pub mod publish;
pub mod replay;
pub mod run_contract;
mod source;

use crate::config::{self, Job, Partition, Reporting};
use crate::fsutil::{dir_builder, real_dir, write_atomic, Dir};
use crate::output::Output;
use crate::runner::{JobResult, ReportingEnv, ReportingKey, Status};
use anyhow::Result;
use collect::{Budget, Collection, Head, JobContext, Sink};
use publish::{Credentials, Limits, Publisher, RunAck};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub(crate) use source::{valid_branch, valid_commit};
pub use source::{Origin, Source};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
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
/// Version of manifests that list a run record, with or without partition
/// entries. Goodflows releases without run-record upload refuse it with a
/// clear version error rather than silently dropping the record.
pub const RUN_RECORD_MANIFEST_VERSION: u32 = 3;
/// The frozen run record, its acknowledgement and its rejection marker, in
/// the run directory. The `.` in each name is always encoded by
/// `config::path_component`, so no job directory can collide with them.
pub const RUN_RECORD: &str = "run-record.json";
pub const RUN_RECORD_ACK: &str = "run-record.ack.json";
pub const RUN_RECORD_REJECTED: &str = "run-record.rejected.json";
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
    /// Present once `run-record.json` is frozen. Absent from version 1 and
    /// 2 manifests, so their bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_record: Option<ManifestRunRecord>,
}

/// The run record a version 3 manifest lists. Its location is always
/// [`RUN_RECORD`] in the run directory, never read from the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRunRecord {
    pub schema_version: u32,
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

/// The version a manifest with these entries, and with or without a run
/// record, is written as (and must carry).
pub fn manifest_version(jobs: &[ManifestJob], run_record: bool) -> u32 {
    if run_record {
        RUN_RECORD_MANIFEST_VERSION
    } else if jobs.iter().any(|job| job.partition.is_some()) {
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

/// A terminal server refusal (400, 409, 415) of the frozen run record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunRejection {
    pub run_id: String,
    pub reason: String,
    pub http_status: u16,
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

/// A run directory whose manifest was finalized, where the run record is
/// frozen.
struct Finalized {
    dir: PathBuf,
    manifest: Manifest,
}

/// What happens to this invocation's run record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunRecord {
    /// `[reporting] history` is off: nothing is built, frozen or sent.
    Off,
    /// Built, validated, frozen and sent after the outcome snapshot.
    Upload,
    /// Nothing is built or kept; `run_upload_skipped` with this reason is
    /// emitted at upload time.
    Skipped(&'static str),
}

/// Reporting state for one `run` invocation.
pub struct Session {
    token_env: String,
    project: String,
    url: Option<String>,
    key: Option<String>,
    limits: Limits,
    /// Created on first use and shared by test reports and the run record, so
    /// one budget covers every request of the invocation.
    publisher: Option<Publisher>,
    run: Option<Run>,
    /// Set by `finish` once the manifest is finalized.
    finalized: Option<Finalized>,
    skipped: Option<&'static str>,
    run_record: RunRecord,
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
    /// A remote `origin` is an invocation on a Sprite, which never has a key
    /// and freezes its run record for the controller to publish.
    pub async fn start(
        reporting: &Reporting,
        run_id: &str,
        workflow: &str,
        environment: Option<&str>,
        jobs: &[Job],
        origin: Origin<'_>,
    ) -> Self {
        // Capture credentials once; the publisher keeps them in this process only.
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let mut session = Self {
            token_env: reporting.token_env.clone(),
            project: reporting.project.clone(),
            url: read(&reporting.url_env),
            key: read(&reporting.token_env),
            limits: Limits::default(),
            publisher: None,
            run: None,
            finalized: None,
            skipped: None,
            run_record: RunRecord::Off,
            source: None,
        };
        if session.key.is_some() {
            protect_process();
        }
        if reporting.history {
            // Resolved once: a laptop without a key keeps nothing for a run
            // record nobody will replay.
            let credentials = publish::resolve(
                session.url.as_deref(),
                session.key.as_deref(),
                &reporting.project,
            );
            session.run_record = match credentials.unavailable() {
                Some(reason) if !origin.remote() => RunRecord::Skipped(reason),
                _ => RunRecord::Upload,
            };
        }
        let reporting_jobs: Vec<_> = jobs.iter().filter(|j| !j.reports.is_empty()).collect();
        if reporting_jobs.is_empty() && session.run_record != RunRecord::Upload {
            return session;
        }
        session.source = origin.source(Some(&reporting.token_env)).await;
        match create_run(
            reporting,
            run_id,
            workflow,
            environment,
            &reporting_jobs,
            session.source.clone().unwrap_or_default(),
        ) {
            Ok(run) => session.run = Some(run),
            Err(_) => {
                if !reporting_jobs.is_empty() {
                    session.skipped = Some("spool_unavailable");
                }
                if session.run_record == RunRecord::Upload {
                    session.run_record = RunRecord::Skipped("spool_unavailable");
                }
            }
        }
        session
    }

    /// Whether this invocation builds and uploads a run record, so the
    /// caller should pass one to [`Session::publish_run_record`].
    pub fn uploads_run_record(&self) -> bool {
        self.run_record != RunRecord::Off
    }

    /// The source context captured at start, if report-producing jobs or the
    /// run record made the session capture one.
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
        if finalized {
            self.finalized = Some(Finalized {
                dir: run.dir.clone(),
                manifest: manifest.clone(),
            });
        }
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
            // A run directory made only for the run record reports that on its own.
            if !manifest.jobs.is_empty() {
                emit(output, json!({"type":"reporting_skipped", "run_id":run_id, "reason":"spool_unavailable", "pending":items.len()})).await;
            }
            return;
        }
        if items.is_empty() {
            return;
        }
        let credentials =
            publish::resolve(self.url.as_deref(), self.key.as_deref(), &manifest.project);
        deliver(
            &run_id,
            items,
            credentials,
            &mut self.publisher,
            self.limits,
            cancel,
            output,
        )
        .await;
    }

    /// Validate, freeze and send this invocation's run record, after `finish`
    /// and the outcome snapshot. Emits one
    /// `run_uploaded`, `run_upload_pending` or `run_upload_skipped` event.
    /// Never changes check results or the exit code.
    pub async fn publish_run_record(
        &mut self,
        record: &impl Serialize,
        run_id: &str,
        cancel: watch::Receiver<Option<i32>>,
        output: &Output,
    ) {
        let skipped =
            |reason: &str| json!({"type":"run_upload_skipped", "run_id":run_id, "reason":reason});
        match self.run_record {
            RunRecord::Off => return,
            RunRecord::Skipped(reason) => return emit(output, skipped(reason)).await,
            RunRecord::Upload => {}
        }
        // Validate the bytes that are frozen and sent, as a replay reads them back.
        let checked = serde_json::to_vec(record).map_err(drop).and_then(|body| {
            let value: Value = serde_json::from_slice(&body).map_err(drop)?;
            Ok((run_contract::validate(&value, body.len()), body))
        });
        let body = match checked {
            Ok((Ok(()), body)) => body,
            Ok((Err(violation), _)) => {
                let mut event = skipped("invalid_record");
                event["path"] = json!(violation.path);
                return emit(output, event).await;
            }
            Err(()) => {
                let mut event = skipped("invalid_record");
                event["path"] = json!("body");
                return emit(output, event).await;
            }
        };
        let Some(dir) = self
            .finalized
            .as_mut()
            .and_then(|run| freeze_run_record(run, &body).ok())
        else {
            return emit(output, skipped("spool_unavailable")).await;
        };
        let credentials = publish::resolve(self.url.as_deref(), self.key.as_deref(), &self.project);
        let outcome = send_run_record(
            &mut self.publisher,
            credentials,
            self.limits,
            &body,
            run_id,
            cancel,
        )
        .await;
        settle_run_record(&dir, run_id, outcome, output).await;
    }
}

/// Send a frozen run record with the invocation's publisher, created here on
/// first use from `credentials`, within what is left of the budget that
/// test-report delivery started.
async fn send_run_record(
    publisher: &mut Option<Publisher>,
    credentials: Credentials,
    limits: Limits,
    body: &[u8],
    run_id: &str,
    cancel: watch::Receiver<Option<i32>>,
) -> publish::Outcome<RunAck> {
    if cancel.borrow().is_some() {
        return publish::Outcome::pending("cancelled", None);
    }
    if publisher.is_none() {
        match credentials {
            Credentials::Ready(target) => {
                *publisher = Publisher::new(target, limits, cancel).ok();
            }
            credentials => {
                let reason = credentials.unavailable().unwrap_or("client_unavailable");
                return publish::Outcome::pending(reason, None);
            }
        }
    }
    match publisher {
        Some(publisher) if publisher.exhausted() => {
            publish::Outcome::pending("budget_exhausted", None)
        }
        Some(publisher) => publisher.send_run(body, run_id).await,
        None => publish::Outcome::pending("client_unavailable", None),
    }
}

/// Record a run record's delivery outcome in its run directory
/// (`run-record.ack.json` or `run-record.rejected.json`) and emit one
/// `run_uploaded` or `run_upload_pending` event. Returns the record's state
/// as `reports_upload_summary` names it.
async fn settle_run_record(
    dir: &Path,
    run_id: &str,
    outcome: publish::Outcome<RunAck>,
    output: &Output,
) -> &'static str {
    let pending = |reason: &str, http_status: Option<u16>| json!({"type":"run_upload_pending", "run_id":run_id, "reason":reason, "http_status":http_status});
    let (state, event) = match outcome {
        publish::Outcome::Uploaded(ack) => match record_marker(dir, RUN_RECORD_ACK, &ack) {
            Ok(()) => (
                "acknowledged",
                json!({"type":"run_uploaded", "run_id":run_id, "server_id":ack.id, "duplicate":ack.duplicate}),
            ),
            Err(_) => ("pending", pending("state_write_failed", None)),
        },
        publish::Outcome::Rejected {
            reason,
            http_status,
        } => {
            // Recorded so replay does not resend bytes the server can never
            // accept. If the marker cannot be written, replay sends it again.
            let marker = RunRejection {
                run_id: run_id.to_owned(),
                reason: reason.into(),
                http_status,
            };
            let _ = record_marker(dir, RUN_RECORD_REJECTED, &marker);
            let mut event = pending(reason, Some(http_status));
            event["terminal"] = json!(true);
            ("rejected", event)
        }
        publish::Outcome::Pending {
            reason,
            http_status,
        } => ("pending", pending(reason, http_status)),
    };
    emit(output, event).await;
    state
}

/// Read a run-record marker from a run directory that is still real, never
/// through a symlink.
fn read_run_marker<T: serde::de::DeserializeOwned>(run_dir: &Path, name: &str) -> Option<T> {
    real_dir(run_dir).ok()?;
    let bytes = collect::read_bounded(&run_dir.join(name), MAX_MARKER_BYTES).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Freeze the run record into a finalized run directory: create
/// `run-record.json` new with mode 0600 relative to the open run directory,
/// refusing a symlink at either, then atomically rewrite the manifest to list
/// it as version 3. Returns the run directory.
fn freeze_run_record(run: &mut Finalized, body: &[u8]) -> std::io::Result<PathBuf> {
    Dir::open(&run.dir)?.write_new(OsStr::new(RUN_RECORD), body)?;
    let mut manifest = run.manifest.clone();
    manifest.run_record = Some(ManifestRunRecord { schema_version: 1 });
    manifest.version = manifest_version(&manifest.jobs, true);
    real_dir(&run.dir)?;
    write_manifest(&run.dir, &manifest).map_err(std::io::Error::other)?;
    run.manifest = manifest;
    Ok(run.dir.clone())
}

/// Atomically write a run-record acknowledgement or rejection marker into a
/// run directory that is still real.
fn record_marker<T: Serialize>(run_dir: &Path, name: &str, value: &T) -> Result<()> {
    real_dir(run_dir)?;
    write_atomic(&run_dir.join(name), &serde_json::to_vec(value)?, true)
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
    crate::fsutil::create_state_dir(spool)?;
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
        version: manifest_version(&manifest_jobs, false),
        state: ManifestState::Running,
        workflow_run_id: id,
        project: reporting.project.clone(),
        workflow: workflow.to_owned(),
        environment: environment.map(str::to_owned),
        url_env: reporting.url_env.clone(),
        token_env: reporting.token_env.clone(),
        source,
        jobs: manifest_jobs,
        run_record: None,
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

/// Send frozen snapshots sequentially within one shared budget and record
/// acknowledgements. `publisher` is created here on first use with `limits`;
/// an existing one keeps its budget.
pub async fn deliver(
    run_id: &str,
    items: Vec<Item>,
    credentials: Credentials,
    publisher: &mut Option<Publisher>,
    limits: Limits,
    cancel: watch::Receiver<Option<i32>>,
    output: &Output,
) -> Delivery {
    let mut delivery = Delivery::default();
    let pending = items.len();
    let skip = match &credentials {
        _ if cancel.borrow().is_some() => None,
        credentials => credentials.unavailable(),
    };
    if let Some(reason) = skip {
        emit(output, json!({"type":"reporting_skipped", "run_id":run_id, "reason":reason, "pending":pending})).await;
        delivery.pending = pending;
        return delivery;
    }
    if publisher.is_none() && cancel.borrow().is_none() {
        if let Credentials::Ready(target) = credentials {
            *publisher = Publisher::new(target, limits, cancel.clone()).ok();
        }
    }
    for item in items {
        let pending = |reason: &str, http_status: Option<u16>| {
            Partition::tag(
                item.partition,
                json!({"type":"report_upload_pending", "run_id":run_id, "job":item.job, "report_id":item.report_id, "reason":reason, "http_status":http_status}),
            )
        };
        let outcome = match publisher {
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
        assert_eq!(manifest_version(&manifest.jobs, false), MANIFEST_VERSION);
        let mut instance = manifest.jobs[0].clone();
        instance.partition = Some(Partition { index: 2, count: 3 }.into());
        let value = serde_json::to_value(&instance).unwrap();
        assert_eq!(value["partition"], json!({"index": 2, "count": 3}));
        let back: ManifestJob = serde_json::from_value(value).unwrap();
        assert_eq!(back.partition, instance.partition);
        assert_eq!(back.instance(), Some(Partition { index: 2, count: 3 }));
        // Any partition entry makes the whole manifest version 2.
        let jobs = [manifest.jobs[0].clone(), instance];
        assert_eq!(manifest_version(&jobs, false), PARTITIONED_MANIFEST_VERSION);
    }

    /// `OLDER_MANIFEST` with its version, its first job's partition and its
    /// run record replaced.
    fn manifest_text(version: u32, partition: bool, run_record: Option<Value>) -> Value {
        let mut value: Value = serde_json::from_str(OLDER_MANIFEST).unwrap();
        value["version"] = json!(version);
        if partition {
            value["jobs"][0]["partition"] = json!({"index": 1, "count": 2});
        }
        if let Some(run_record) = run_record {
            value["run_record"] = run_record;
        }
        value
    }

    /// Write `value` as a run directory's manifest and load it as replay does.
    fn load(value: &Value) -> Result<Manifest> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
        replay::load_manifest(dir.path())
    }

    #[test]
    fn manifest_versions_round_trip_and_are_accepted() {
        let record = || Some(json!({"schema_version": 1}));
        for (version, partition, run_record) in [
            (MANIFEST_VERSION, false, None),
            (PARTITIONED_MANIFEST_VERSION, true, None),
            (RUN_RECORD_MANIFEST_VERSION, false, record()),
            (RUN_RECORD_MANIFEST_VERSION, true, record()),
        ] {
            let original = manifest_text(version, partition, run_record.clone());
            let manifest: Manifest = serde_json::from_value(original.clone()).unwrap();
            // Serialized unchanged: versions 1 and 2 never gain a `run_record` key.
            assert_eq!(serde_json::to_value(&manifest).unwrap(), original);
            assert_eq!(manifest.run_record.is_some(), run_record.is_some());
            assert_eq!(
                manifest_version(&manifest.jobs, manifest.run_record.is_some()),
                version
            );
            let loaded = load(&original).unwrap_or_else(|e| panic!("version {version}: {e:#}"));
            assert_eq!(serde_json::to_value(&loaded).unwrap(), original);
        }
    }

    #[test]
    fn manifest_versions_must_match_the_run_record() {
        let record = || Some(json!({"schema_version": 1}));
        for (manifest, message) in [
            (
                manifest_text(3, false, None),
                "manifest version 3 requires a run record",
            ),
            (
                manifest_text(3, true, None),
                "manifest version 3 requires a run record",
            ),
            (
                manifest_text(2, true, record()),
                "manifest version 2 requires at least one job with partition entries and no run record",
            ),
            (
                manifest_text(1, false, record()),
                "manifest version 1 requires no partition entries and no run record",
            ),
            (
                manifest_text(3, false, Some(json!({"schema_version": 2}))),
                "run record schema_version must be 1",
            ),
            (
                manifest_text(3, false, Some(json!({"schema_version": 1, "path": "x"}))),
                "invalid manifest",
            ),
            (
                manifest_text(3, false, Some(Value::Null)),
                "manifest version 3 requires a run record",
            ),
            (
                manifest_text(4, false, record()),
                "unsupported manifest version 4",
            ),
        ] {
            let error = format!("{:#}", load(&manifest).unwrap_err());
            assert!(error.contains(message), "{message}: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn freezing_writes_a_private_record_and_lists_it_or_refuses_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let manifest: Manifest = serde_json::from_str(OLDER_MANIFEST).unwrap();
        let finalized = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            write_manifest(dir, &manifest).unwrap();
            Finalized {
                dir: dir.to_owned(),
                manifest: manifest.clone(),
            }
        };
        let saved = |dir: &Path| -> Value {
            serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).unwrap()).unwrap()
        };

        let dir = tmp.path().join("run");
        let mut run = finalized(&dir);
        assert_eq!(freeze_run_record(&mut run, b"{\"a\":1}").unwrap(), dir);
        assert_eq!(std::fs::read(dir.join(RUN_RECORD)).unwrap(), b"{\"a\":1}");
        let mode = std::fs::metadata(dir.join(RUN_RECORD))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let listed = saved(&dir);
        assert_eq!(listed["version"], 3);
        assert_eq!(listed["run_record"], json!({"schema_version": 1}));
        assert_eq!(
            listed["jobs"],
            serde_json::to_value(&manifest.jobs).unwrap()
        );
        assert_eq!(run.manifest.run_record.unwrap().schema_version, 1);
        // Created new: a second freeze never replaces the first record.
        assert!(freeze_run_record(&mut run, b"{}").is_err());
        assert_eq!(std::fs::read(dir.join(RUN_RECORD)).unwrap(), b"{\"a\":1}");

        // A record name already taken by a symlink is refused, not followed.
        let planted = tmp.path().join("planted");
        let mut run = finalized(&planted);
        let target = tmp.path().join("target.json");
        std::os::unix::fs::symlink(&target, planted.join(RUN_RECORD)).unwrap();
        assert!(freeze_run_record(&mut run, b"{}").is_err());
        assert!(!target.exists());
        assert_eq!(saved(&planted)["version"], 1);

        // A run directory swapped for a symlink is refused.
        let real = tmp.path().join("real");
        let mut run = finalized(&real);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        run.dir = link;
        assert!(freeze_run_record(&mut run, b"{}").is_err());
        assert!(!real.join(RUN_RECORD).exists());
        assert_eq!(saved(&real)["version"], 1);
        assert!(saved(&real).get("run_record").is_none());
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

    #[tokio::test]
    async fn the_run_record_shares_the_report_budget() {
        use std::io::Read;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        // Accepts connections and never answers, so every report ack is slow.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let count = connections.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                count.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut sink = [0; 4096];
                    while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
                });
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("goodflows.toml");
        std::fs::write(
            &path,
            "[reporting]\nproject = \"goodcases\"\nurl_env = \"GOODFLOWS_TEST_UNSET_URL\"\n\
             token_env = \"GOODFLOWS_TEST_UNSET_KEY\"\n\n\
             [[jobs]]\nname = \"test\"\ncommand = \"true\"\nreports = [\"test-timings\"]\n",
        )
        .unwrap();
        let (document, _) = config::Config::load(Some(&path)).unwrap();
        let (workflow, config) = document.workflow(None, None).unwrap();
        let reporting = document.reporting.clone().unwrap();
        let run_id = "67f7946d-136b-4608-ab01-e1f251a09754";
        let mut session = Session::start(
            &reporting,
            run_id,
            &workflow,
            None,
            &config.jobs,
            Origin::Checkout(tmp.path()),
        )
        .await;
        session.url = Some(url);
        session.key = Some("k".into());
        session.limits = Limits {
            budget: Duration::from_millis(600),
            connect: Duration::from_millis(500),
            request: Duration::from_millis(400),
            request_per_mib: Duration::ZERO,
            backoffs: [Duration::from_millis(20), Duration::from_millis(40)],
        };
        let reports = tmp
            .path()
            .join(".goodflows/reports")
            .join(run_id)
            .join("test/reports");
        for id in ["r1", "r2"] {
            std::fs::write(
                reports.join(format!("{id}.jsonl")),
                collect::tests::complete(id),
            )
            .unwrap();
        }
        let result = JobResult {
            name: "test".into(),
            partition: None,
            command: Some("true".into()),
            workflow: None,
            child_run_id: None,
            status: Status::Passed,
            duration_ms: 0,
            exit_code: Some(0),
            error: None,
            outputs: Default::default(),
            steps: Vec::new(),
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: None,
            stderr_truncated: None,
            output: None,
            executed: true,
            started: true,
            blocked_ms: 0,
            queued_ms: 0,
            commands: Vec::new(),
            fail_fast_skip: false,
        };

        let (_tx, cancel) = watch::channel(None);
        let output = Output::discard(true, cancel.clone()).unwrap();
        let start = std::time::Instant::now();
        session.finish(&[result], cancel.clone(), &output).await;
        assert!(start.elapsed() < Duration::from_millis(1500));
        // Give a stray connection time to be accepted before counting.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let sent = connections.load(Ordering::SeqCst);
        assert!(sent >= 1, "the reports were never sent");

        let credentials = publish::resolve(
            session.url.as_deref(),
            session.key.as_deref(),
            &session.project,
        );
        let outcome = send_run_record(
            &mut session.publisher,
            credentials,
            session.limits,
            b"{}",
            run_id,
            cancel,
        )
        .await;
        assert!(
            matches!(
                outcome,
                publish::Outcome::Pending {
                    reason: "budget_exhausted",
                    http_status: None
                }
            ),
            "{outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(connections.load(Ordering::SeqCst), sent);
    }
}
