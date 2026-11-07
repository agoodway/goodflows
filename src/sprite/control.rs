//! Remote control records shared by the controller and the in-Sprite agent.
//!
//! Every record is versioned JSON written atomically: temporary file, fsync,
//! rename, then a directory fsync. Environment values are never persisted.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Internal controller/agent protocol. A mismatch fails before any mutation.
pub const PROTOCOL: u32 = 1;
pub const RECORD_SCHEMA: u32 = 1;
/// The Sprite-managed service that owns accepted runs.
pub const SERVICE_NAME: &str = "goodflows-supervisor";
/// Tasks API hold renewal, from the measured probe recipe.
pub const HOLD_EXPIRE_SECS: u64 = 12;
pub const HOLD_RENEW_SECS: u64 = 3;

/// Lifecycle phases. The last five are terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Accepted,
    Uploading,
    Verifying,
    Setup,
    Resume,
    Running,
    Finalizing,
    Completed,
    PreparationFailed,
    TransferFailed,
    Cancelled,
    Interrupted,
}

impl Phase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Phase::Completed
                | Phase::PreparationFailed
                | Phase::TransferFailed
                | Phase::Cancelled
                | Phase::Interrupted
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Phase::Accepted => "accepted",
            Phase::Uploading => "uploading",
            Phase::Verifying => "verifying",
            Phase::Setup => "setup",
            Phase::Resume => "resume",
            Phase::Running => "running",
            Phase::Finalizing => "finalizing",
            Phase::Completed => "completed",
            Phase::PreparationFailed => "preparation_failed",
            Phase::TransferFailed => "transfer_failed",
            Phase::Cancelled => "cancelled",
            Phase::Interrupted => "interrupted",
        }
    }
}

/// One snapshot file: path relative to the source root, content hash, size
/// and whether any execute bit is set. Ordinary modes only (0644/0755).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

/// Declared workdirs relative to the source root, keyed like the config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workdirs {
    pub settings: Option<String>,
    #[serde(default)]
    pub workflows: BTreeMap<String, String>,
    #[serde(default)]
    pub environments: BTreeMap<String, String>,
}

/// Controller-side provenance; stored only in the control record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
    pub controller_version: String,
}

/// The workflow invocation the remote runner executes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub workflow: Option<String>,
    pub environment: Option<String>,
    #[serde(default)]
    pub jobs: Vec<String>,
    pub parallelism: Option<usize>,
    pub fail_fast: Option<bool>,
    /// `--log-dir` was given: keep raw logs under the managed results path.
    pub logs: bool,
    pub history: Option<bool>,
    pub log_runs: Option<u64>,
    pub history_runs: Option<u64>,
    pub history_bytes: Option<u64>,
}

/// Submission header. Contains no environment values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub protocol: u32,
    pub run_id: String,
    pub scope_digest: String,
    pub runner_version: String,
    /// Original config text; workdirs are remapped by the agent.
    pub config_text: String,
    /// Config path relative to the source root.
    pub config_path: String,
    pub workdirs: Workdirs,
    pub manifest: Vec<ManifestEntry>,
    pub manifest_sha256: String,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub setup: String,
    pub resume: String,
    /// Names only; values travel in the separate private payload.
    pub pass_env: Vec<String>,
    pub request: Request,
    pub provenance: Provenance,
    pub upload_timeout_secs: u64,
}

impl Header {
    pub fn manifest_digest(manifest: &[ManifestEntry]) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(manifest).expect("manifest serializes");
        format!("{:x}", Sha256::digest(bytes))
    }
}

/// The accepted worker's identity, registered before its GO handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub boot_id: String,
    pub pid: u32,
    pub start_ticks: u64,
    pub generation: String,
}

/// A hook or workflow process group the worker started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildIdentity {
    pub role: String,
    pub boot_id: String,
    pub pgid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseTiming {
    pub phase: Phase,
    pub started_at_ms: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terminal {
    /// Native workflow exit code when the workflow finished.
    pub exit_code: Option<i32>,
    pub success: Option<bool>,
    /// Sanitized reason for non-completed outcomes.
    pub reason: Option<String>,
}

/// `runs/<uuid>/record.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub run_id: String,
    pub scope_digest: String,
    pub phase: Phase,
    /// Execution-generation token, present in the worker's and workflow
    /// runner's command lines; allocated at acceptance.
    pub generation: String,
    /// The accepting submit process, alive until the worker is registered.
    pub submitter: Option<ProcessIdentity>,
    pub accepted_at_ms: u64,
    pub updated_at_ms: u64,
    pub manifest_sha256: String,
    pub workflow: Option<String>,
    pub environment: Option<String>,
    pub jobs: Vec<String>,
    pub runner_version: String,
    pub protocol: u32,
    pub provenance: Provenance,
    pub hook: Option<String>,
    pub hook_digest: Option<String>,
    pub worker: Option<ProcessIdentity>,
    #[serde(default)]
    pub children: Vec<ChildIdentity>,
    #[serde(default)]
    pub timings: Vec<PhaseTiming>,
    pub terminal: Option<Terminal>,
    pub cancel_requested: bool,
    /// The workflow runner process exited; its output files are complete.
    pub workflow_exited: bool,
}

impl Record {
    pub fn new(header: &Header, now: u64, generation: String) -> Self {
        Record {
            schema: RECORD_SCHEMA,
            run_id: header.run_id.clone(),
            scope_digest: header.scope_digest.clone(),
            phase: Phase::Accepted,
            generation,
            submitter: None,
            accepted_at_ms: now,
            updated_at_ms: now,
            manifest_sha256: header.manifest_sha256.clone(),
            workflow: header.request.workflow.clone(),
            environment: header.request.environment.clone(),
            jobs: header.request.jobs.clone(),
            runner_version: header.runner_version.clone(),
            protocol: header.protocol,
            provenance: header.provenance.clone(),
            hook: None,
            hook_digest: None,
            worker: None,
            children: Vec::new(),
            timings: vec![PhaseTiming {
                phase: Phase::Accepted,
                started_at_ms: now,
                duration_ms: None,
            }],
            terminal: None,
            cancel_requested: false,
            workflow_exited: false,
        }
    }

    /// Close the current phase's timing and start `phase`.
    pub fn enter(&mut self, phase: Phase) {
        let now = now_ms();
        if let Some(last) = self.timings.last_mut() {
            if last.duration_ms.is_none() {
                last.duration_ms = Some(now.saturating_sub(last.started_at_ms));
            }
        }
        self.phase = phase;
        self.updated_at_ms = now;
        self.timings.push(PhaseTiming {
            phase,
            started_at_ms: now,
            duration_ms: if phase.terminal() { Some(0) } else { None },
        });
    }
}

/// `prepared.json`: written atomically only after setup succeeded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub schema: u32,
    pub run_id: String,
    pub setup_digest: String,
    pub prepared_at_ms: u64,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Remote control root layout under `$HOME/.goodflows`.
#[derive(Clone, Debug)]
pub struct Root {
    pub dir: PathBuf,
}

impl Root {
    pub fn from_home() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(Root {
            dir: PathBuf::from(home).join(".goodflows"),
        })
    }
    pub fn owner(&self) -> PathBuf {
        self.dir.join("owner.json")
    }
    pub fn control(&self) -> PathBuf {
        self.dir.join("control")
    }
    pub fn state(&self) -> PathBuf {
        self.dir.join("state")
    }
    pub fn runs(&self) -> PathBuf {
        self.dir.join("runs")
    }
    pub fn run(&self, id: &str) -> PathBuf {
        self.runs().join(id)
    }
    pub fn prepared(&self) -> PathBuf {
        self.dir.join("prepared.json")
    }
    pub fn callers(&self, scope: &str) -> PathBuf {
        self.dir.join("callers").join(scope)
    }
    pub fn destroying(&self) -> PathBuf {
        self.control().join("destroying")
    }
    pub fn degraded(&self) -> PathBuf {
        self.control().join("hold-degraded")
    }

    /// Create the private control tree, refusing symlinked components.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.dir.clone(),
            self.control(),
            self.state(),
            self.runs(),
            self.dir.join("callers"),
            self.dir.join("runners"),
        ] {
            ensure_private_dir(&dir)?;
        }
        Ok(())
    }
}

/// Create a 0700 directory or confirm an existing one is a real directory.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => bail!("{} is not a real directory", dir.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match crate::fsutil::dir_builder(false).create(dir) {
                Ok(()) => Ok(()),
                // A concurrent creator (two first submissions) won the race:
                // accept it only if the result is a real directory.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
                        Ok(())
                    } else {
                        bail!("{} is not a real directory", dir.display())
                    }
                }
                Err(error) => Err(error).with_context(|| format!("create {}", dir.display())),
            }
        }
        Err(error) => Err(error).with_context(|| format!("inspect {}", dir.display())),
    }
}

/// Durable atomic write: temp file (0600), fsync, rename, directory fsync.
pub fn write_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    let mut file = tempfile::Builder::new()
        .prefix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .with_context(|| format!("replace {}", path.display()))?;
    sync_dir(parent)
}

pub fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("sync {}", dir.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_durable(path, &bytes)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("read {}", path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if meta.len() > 64 * 1024 * 1024 {
        bail!("{} is unexpectedly large", path.display());
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// The boot identity recorded or reported when it cannot be read.
pub const UNKNOWN_BOOT: &str = "unknown";

/// This boot's identity. A changed value means every recorded process is
/// gone; [`UNKNOWN_BOOT`] proves nothing either way.
pub fn boot_id() -> String {
    if super::testhook::fault("boot-id-unreadable") {
        return UNKNOWN_BOOT.into();
    }
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| UNKNOWN_BOOT.into())
}

/// Whether a process recorded with boot identity `recorded` may belong to
/// boot `current`: only two different known identities prove a reboot.
pub fn same_boot(recorded: &str, current: &str) -> bool {
    recorded == current || recorded == UNKNOWN_BOOT || current == UNKNOWN_BOOT
}

/// Field 22 of `/proc/<pid>/stat`: start time in clock ticks since boot.
pub fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces or parentheses; fields follow the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Process group and session of a live (non-zombie) process.
fn proc_group_session(pid: u32) -> Option<(u32, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat[stat.rfind(')')? + 2..].split_whitespace();
    if fields.next()? == "Z" {
        return None;
    }
    let mut fields = fields.skip(1);
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// Live processes whose session or process group is `id`.
pub fn session_members(id: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            proc_group_session(*pid).is_some_and(|(group, session)| group == id || session == id)
        })
        .collect()
}

/// True when `pid` is still the exact recorded process: same boot, same
/// start ticks, and (for workers) the generation token in its command line.
pub fn process_matches(identity: &ProcessIdentity) -> bool {
    if !same_boot(&identity.boot_id, &boot_id()) {
        return false;
    }
    if start_ticks(identity.pid) != Some(identity.start_ticks) {
        return false;
    }
    std::fs::read(format!("/proc/{}/cmdline", identity.pid))
        .map(|cmdline| {
            cmdline
                .split(|b| *b == 0)
                .any(|arg| arg == identity.generation.as_bytes())
        })
        .unwrap_or(false)
}

/// True when any process still belongs to the recorded session or group.
/// Conservative: an unrelated reuse of the id also counts, which keeps state
/// blocked. Without `/proc` nothing can be proven gone.
pub fn group_alive(child: &ChildIdentity) -> bool {
    if std::fs::read_dir("/proc").is_err() {
        return true;
    }
    !session_members(child.pgid).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_private_dir_creation_is_not_an_error() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("callers");
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || ensure_private_dir(&dir))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        let file = base.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(ensure_private_dir(&file).is_err());
    }

    // Process identity comes from /proc, which exists only on the Linux
    // Sprite; macOS controllers never run this code.
    #[cfg(target_os = "linux")]
    #[test]
    fn own_process_identity_matches_and_changes_do_not() {
        let pid = std::process::id();
        let ticks = start_ticks(pid).expect("start ticks");
        let mut identity = ProcessIdentity {
            boot_id: boot_id(),
            pid,
            start_ticks: ticks,
            generation: "not-in-cmdline-1b2f".into(),
        };
        assert!(!process_matches(&identity), "generation must be in cmdline");
        let arg0 = std::env::args().next().unwrap();
        identity.generation = arg0;
        assert!(process_matches(&identity));
        identity.start_ticks += 1;
        assert!(!process_matches(&identity));
    }

    #[test]
    fn phases_classify_terminal_states() {
        assert!(Phase::Completed.terminal() && Phase::Interrupted.terminal());
        assert!(!Phase::Running.terminal() && !Phase::Finalizing.terminal());
    }
}
