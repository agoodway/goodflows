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

use crate::config::{self, Job, Reporting};
use crate::output::Output;
use crate::runner::{JobResult, ReportingEnv, Status};
use anyhow::{Context, Result};
use collect::{Collection, Head, JobContext, Sink};
use publish::{Credentials, Limits};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
pub use source::Source;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::watch;

pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_VERSION: u32 = 1;
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
    pub status: Option<String>,
    pub executed: bool,
    pub collection: Option<ManifestCollection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestCollection {
    pub outcome: String,
    pub rejected: BTreeMap<String, usize>,
    pub reports: Vec<ManifestReport>,
}

/// Snapshot `<job>/pending/NNN.json`, acknowledgement `<job>/acks/NNN.json`
/// and rejection marker `<job>/acks/NNN.rejected.json` are derived from the
/// job directory and this index only; the manifest never names a path.
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

/// The server's only accepted `workflow_run_id` spelling.
pub fn canonical_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
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

/// Write a file by atomic replacement so readers never observe partial content.
pub fn write_atomic(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

pub fn write_manifest(run_dir: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    write_atomic(&run_dir.join(MANIFEST), &bytes, true)
}

fn dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    // Reports, snapshots and acknowledgements are private to the invoking user.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// Confirm a path is a real directory, never a symlink to one.
pub fn real_dir(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other("not a real directory"))
    }
}

/// Create a private directory, or confirm an existing one is a real directory.
fn private_dir(path: &Path) -> std::io::Result<()> {
    match dir_builder(false).create(path) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => Err(error),
        _ => real_dir(path),
    }
}

/// One job's state directories inside a run directory.
#[derive(Debug, Clone)]
pub struct JobDirs {
    job: PathBuf,
}

impl JobDirs {
    pub fn new(run_dir: &Path, encoded: &str) -> Self {
        Self {
            job: run_dir.join(encoded),
        }
    }

    fn pending(&self) -> PathBuf {
        self.job.join("pending")
    }

    fn acks(&self) -> PathBuf {
        self.job.join("acks")
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

    /// Neither the job directory nor `pending/` may have been replaced by a symlink.
    fn check_pending(&self) -> std::io::Result<()> {
        real_dir(&self.job)?;
        real_dir(&self.pending())
    }

    /// The job directory must be real; `acks/` is (re)created privately if missing.
    fn ensure_acks(&self) -> std::io::Result<()> {
        real_dir(&self.job)?;
        private_dir(&self.acks())
    }

    /// Read a small marker file (never through a symlink).
    fn read_marker<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Option<T> {
        real_dir(&self.job).ok()?;
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

    fn discard(&mut self, index: u32) {
        if self.dirs.check_pending().is_ok() {
            let _ = std::fs::remove_file(self.dirs.snapshot(index));
        }
    }
}

struct Run {
    dir: PathBuf,
    manifest: Manifest,
    env: HashMap<String, Vec<(String, String)>>,
}

/// Reporting state for one `run` invocation.
pub struct Session {
    token_env: String,
    url: Option<String>,
    key: Option<String>,
    run: Option<Run>,
    skipped: Option<&'static str>,
}

/// Reporting events are best-effort: a failed write (for example a closed
/// stdout) must never change the check result or leave the run unfinalized.
async fn emit(output: &Output, event: Value) {
    let _ = output.emit(event).await;
}

impl Session {
    pub async fn start(
        reporting: &Reporting,
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
        };
        if session.key.is_some() {
            protect_process();
        }
        let reporting_jobs: Vec<_> = jobs.iter().filter(|j| !j.reports.is_empty()).collect();
        if reporting_jobs.is_empty() {
            return session;
        }
        let source = source::capture(workdir, &reporting.token_env).await;
        match create_run(reporting, workflow, environment, &reporting_jobs, source) {
            Ok(run) => session.run = Some(run),
            Err(_) => session.skipped = Some("spool_unavailable"),
        }
        session
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
        let find = |name: &str| results.iter().find(|r| r.name == name);
        for job in &mut run.manifest.jobs {
            let result = find(&job.name);
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

/// Collect every executed job of a run into its pending area. Blocking.
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
    for job in jobs.iter_mut() {
        let dirs = JobDirs::new(&run.dir, &job.dir);
        let job_status = job.status.clone().unwrap_or_else(|| "unknown".into());
        let collection = if !job.executed {
            Collection::not_executed()
        } else if real_dir(&dirs.job).is_err() {
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
            };
            let mut sink = SpoolSink {
                dirs: dirs.clone(),
                next: 0,
            };
            collect::collect_dir(&dirs.job.join("reports"), &ctx, &mut sink, stop)
        };
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
                report_id: report.report_id.clone(),
                index: report.index,
                dirs: dirs.clone(),
                expected: Head {
                    workflow_run_id: workflow_run_id.clone(),
                    workflow: workflow.clone(),
                    environment: environment.clone(),
                    job: job.name.clone(),
                    job_status: job_status.clone(),
                    source: source.clone(),
                    report_id: report.report_id.clone(),
                },
            });
        }
        job.collection = Some(ManifestCollection {
            outcome: collection.outcome.into(),
            rejected: collection.rejected,
            reports,
        });
    }
    items
}

fn create_run(
    reporting: &Reporting,
    workflow: &str,
    environment: Option<&str>,
    jobs: &[&Job],
    source: Source,
) -> Result<Run> {
    let spool = reporting.spool_dir();
    dir_builder(true).create(spool)?;
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    let dir = spool.join(&id);
    // A non-recursive create fails rather than sharing a directory with another invocation.
    dir_builder(false).create(&dir)?;
    let mut env = HashMap::new();
    let mut manifest_jobs = Vec::new();
    for job in jobs {
        let encoded = config::path_component(&job.name);
        let job_dir = dir.join(&encoded);
        // Every state directory exists (privately) before any job can run, so a
        // job cannot pre-create one of them as a symlink.
        for sub in ["", "reports", "pending", "acks"] {
            dir_builder(false).create(job_dir.join(sub))?;
        }
        env.insert(
            job.name.clone(),
            vec![
                ("GOODFLOWS_RUN_ID".to_owned(), id.clone()),
                (
                    "GOODFLOWS_REPORT_DIR".to_owned(),
                    job_dir.join("reports").to_string_lossy().into_owned(),
                ),
            ],
        );
        manifest_jobs.push(ManifestJob {
            name: job.name.clone(),
            dir: encoded,
            status: None,
            executed: false,
            collection: None,
        });
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
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
            Ok(head) if head == self.expected => Ok(body),
            _ => Err("invalid_snapshot"),
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
        let pending = |reason: &str, http_status: Option<u16>| json!({"type":"report_upload_pending", "run_id":run_id, "job":item.job, "report_id":item.report_id, "reason":reason, "http_status":http_status});
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
                        json!({"type":"report_uploaded", "run_id":run_id, "job":item.job, "report_id":item.report_id, "server_id":ack.id, "status":ack.status, "duplicate":ack.duplicate})
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
    fn workflow_run_ids_must_be_canonical_lowercase() {
        assert!(canonical_uuid("67f7946d-136b-4608-ab01-e1f251a09754"));
        for bad in [
            "67F7946D-136B-4608-AB01-E1F251A09754",
            "67f7946d136b4608ab01e1f251a09754",
            "{67f7946d-136b-4608-ab01-e1f251a09754}",
            "urn:uuid:67f7946d-136b-4608-ab01-e1f251a09754",
            "../67f7946d-136b-4608-ab01-e1f251a09754",
            "",
        ] {
            assert!(!canonical_uuid(bad), "{bad}");
        }
    }

    #[test]
    fn job_paths_derive_from_directory_and_index_only() {
        let dirs = JobDirs::new(Path::new("/spool/run"), "test");
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
        let dirs = JobDirs::new(&run, "test");
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
}
