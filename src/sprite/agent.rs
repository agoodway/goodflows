//! The in-Sprite side of Sprite runs: `goodflows __sprite-agent <op> ...`.
//!
//! The controller reaches these operations through `sprite exec`. Durable
//! execution never depends on that connection: accepted runs are handed to a
//! Sprite-managed service (`serve`), which spawns a `worker` per run. The
//! worker owns the caller-identity lock, the Tasks API hold, the preparation
//! hook and the workflow runner (`exec-workflow`), and persists terminal
//! status before it releases the hold.

use super::control::{
    self, boot_id, now_ms, process_matches, read_json, sha256_hex, start_ticks, write_durable,
    write_json, ChildIdentity, Header, Phase, Prepared, ProcessIdentity, Provenance, Record, Root,
    Terminal, HOLD_EXPIRE_SECS, HOLD_RENEW_SECS, PROTOCOL, SERVICE_NAME,
};
use super::naming::{Owner, Scope};
use super::{hold, snapshot};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Collection limits shared with the controller (documented in docs/sprites.md).
pub const MAX_BUNDLE_FILES: usize = 10_000;
pub const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
/// How long the supervisor waits for a spawned worker to register.
const REGISTER_WAIT: Duration = Duration::from_secs(70);
const HOOK_GRACE: Duration = Duration::from_secs(5);
const WORKFLOW_GRACE: Duration = Duration::from_secs(30);

pub async fn main(args: &[String]) -> u8 {
    let op = args.first().map(String::as_str).unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default();
    let result = match op {
        "exec-workflow" => exec_workflow(rest).await,
        _ => tokio::task::block_in_place(|| dispatch(op, rest)),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("goodflows agent: {error:#}");
            1
        }
    }
}

fn dispatch(op: &str, args: &[String]) -> Result<u8> {
    let arg = |i: usize| -> Result<&str> {
        args.get(i)
            .map(String::as_str)
            .with_context(|| format!("{op}: missing argument {}", i + 1))
    };
    let root = Root::from_home()?;
    match op {
        "hello" => print_json(&json!({
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": PROTOCOL,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        })),
        "ensure-supervisor" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            ensure_supervisor(&root)
        }
        "submit" => submit(&root, &decode_scope(arg(0)?)?),
        "status" => status(
            &root,
            &decode_scope(arg(0)?)?,
            args.get(1).is_some_and(|a| a == "reconcile"),
        ),
        "follow" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            // `--skip N`: leave out the first N stream lines (a reconnecting
            // controller already printed them) and acknowledge that first.
            // Runners before this option ignore extra arguments and replay
            // everything; the controller then skips by count itself.
            let skip = match args.get(2..).unwrap_or_default() {
                [] => 0,
                [flag, n] if flag == "--skip" => n.parse().context("follow: invalid --skip")?,
                _ => bail!("follow: unexpected arguments"),
            };
            follow(&root, run_id(arg(1)?)?, true, skip)
        }
        "logs" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            follow(&root, run_id(arg(1)?)?, false, 0)
        }
        "cancel" => cancel(&root, &decode_scope(arg(0)?)?, run_id(arg(1)?)?),
        "export" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            export(&root, run_id(arg(1)?)?)
        }
        "prepare-destroy" => prepare_destroy(&root, &decode_scope(arg(0)?)?),
        "check-destroy" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            let held = root.destroying().join(run_id(arg(1)?)?).is_file();
            print_json(&json!({"status": "ok", "held": held}))
        }
        "abort-destroy" => {
            verify_owner(&root, &decode_scope(arg(0)?)?)?;
            let cleared = remove_destroy_marker(&root, run_id(arg(1)?)?)?;
            print_json(&json!({"status": "ok", "cleared": cleared}))
        }
        "serve" => serve(&root),
        "worker" => worker(&root, run_id(arg(0)?)?, arg(1)?, arg(2)?),
        other => bail!("unknown agent operation '{other}'"),
    }
}

fn print_json(value: &Value) -> Result<u8> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(0)
}

fn run_id(value: &str) -> Result<&str> {
    if !crate::fsutil::canonical_uuid(value) {
        bail!("run id must be a lowercase canonical UUID");
    }
    Ok(value)
}

/// Scope travels as hex-encoded JSON so caller data is never parsed as a flag.
pub fn encode_scope(scope: &Scope) -> String {
    serde_json::to_vec(scope)
        .expect("scope serializes")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn decode_scope(hex: &str) -> Result<Scope> {
    if !hex.len().is_multiple_of(2) || hex.len() > 4096 {
        bail!("malformed scope argument");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("malformed scope argument")?;
    let scope: Scope = serde_json::from_slice(&bytes).context("malformed scope argument")?;
    super::naming::validate_identity(&scope.identity)?;
    Ok(scope)
}

fn verify_owner(root: &Root, scope: &Scope) -> Result<Owner> {
    if std::fs::symlink_metadata(&root.dir).is_ok_and(|m| !m.is_dir()) {
        bail!("{} is not a real directory", root.dir.display());
    }
    let owner: Owner = match read_json(&root.owner()) {
        Ok(owner) => owner,
        Err(_) if !root.owner().exists() => {
            bail!("unowned: this Sprite has no GoodFlows ownership record; refusing to adopt it")
        }
        Err(error) => return Err(error),
    };
    owner.verify(scope)?;
    Ok(owner)
}

// ---------------------------------------------------------------------------
// Caller-identity lock and admission

/// The process-held caller-identity lock: an open file description holding
/// an exclusive `flock`. Every descriptor sharing that description holds it,
/// so `submit` hands the description to the supervisor (SCM_RIGHTS), which
/// passes it to the worker at exec; the lock is never free between admission
/// and the worker's durable finalization.
pub struct CallerLock {
    file: std::fs::File,
}

impl CallerLock {
    fn fd(&self) -> std::os::fd::RawFd {
        std::os::fd::AsRawFd::as_raw_fd(&self.file)
    }
}

/// Serializes record ownership changes between a worker's registration and
/// a failed handoff's cleanup, which share one caller-lock description and
/// so cannot exclude each other with it. Held only for those short updates.
fn record_lock(run: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(run.join("record.lock"))
        .context("open run record lock")?;
    // SAFETY: a blocking flock on a descriptor this function owns; released on drop.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("lock run record");
    }
    Ok(file)
}

/// Send `bytes` on `stream` with `fd` attached (SCM_RIGHTS); returns the
/// number of bytes the first `sendmsg` wrote.
fn send_with_fd(
    stream: &UnixStream,
    bytes: &[u8],
    fd: std::os::fd::RawFd,
) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd;
    let fd_len = std::mem::size_of::<std::os::fd::RawFd>();
    // u64 storage keeps the control buffer aligned for cmsghdr.
    let mut control = [0u64; 8];
    // SAFETY: CMSG_* arithmetic on a zeroed, aligned buffer large enough for
    // one descriptor; sendmsg reads only the described buffers.
    unsafe {
        let space = libc::CMSG_SPACE(fd_len as u32) as usize;
        assert!(space <= std::mem::size_of_val(&control));
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr() as *mut libc::c_void,
            iov_len: bytes.len(),
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_len as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg) as *mut std::os::fd::RawFd, fd);
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let sent = libc::sendmsg(stream.as_raw_fd(), &msg, flags);
        if sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sent as usize)
    }
}

/// Receive up to `buf.len()` bytes and at most one attached descriptor,
/// close-on-exec so no unrelated child inherits it.
fn recv_with_fd(
    stream: &UnixStream,
    buf: &mut [u8],
) -> std::io::Result<(usize, Option<std::os::fd::OwnedFd>)> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    let fd_len = std::mem::size_of::<RawFd>();
    let mut control = [0u64; 8];
    // SAFETY: recvmsg into owned buffers; every received descriptor is
    // wrapped in an OwnedFd exactly once.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&control) as _;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let received = libc::recvmsg(stream.as_raw_fd(), &mut msg, flags);
        if received < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fds = Vec::new();
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                for index in 0..data / fd_len {
                    let fd = std::ptr::read_unaligned(
                        (libc::CMSG_DATA(cmsg) as *const RawFd).add(index),
                    );
                    #[cfg(not(target_os = "linux"))]
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        if msg.msg_flags & libc::MSG_CTRUNC != 0 || fds.len() > 1 {
            return Err(std::io::Error::other("unexpected descriptors in request"));
        }
        Ok((received as usize, fds.pop()))
    }
}

/// Take ownership of the caller lock the supervisor passed at exec (`fd`),
/// mark it close-on-exec again (hooks and jobs never inherit it, so a
/// daemon a hook leaves behind cannot pin the identity), and prove that it
/// is this scope's lock file and that its description holds the lock.
fn inherited_lock(root: &Root, scope_digest: &str, fd: &str) -> Result<CallerLock> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::MetadataExt;
    let fd: std::os::fd::RawFd = fd.parse().context("caller lock descriptor")?;
    // SAFETY: fcntl on a plain integer; a closed descriptor fails with EBADF.
    if fd < 3 || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        bail!("no caller lock was handed over");
    }
    // SAFETY: the descriptor is open (checked above) and owned by nothing else
    // in this process: the supervisor passed it at exec for this worker only.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let held = file.metadata().context("inspect the caller lock")?;
    let expected = std::fs::symlink_metadata(root.callers(scope_digest).join("lock"))
        .context("inspect the caller lock")?;
    if !expected.is_file() || held.dev() != expected.dev() || held.ino() != expected.ino() {
        bail!("the handed-over descriptor is not this caller's lock");
    }
    // Re-locking through the same description succeeds only if it holds the
    // lock (or nobody does); another holder means the handoff is not ours.
    // SAFETY: flock on a descriptor owned here.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("the handed-over caller lock is held by another description");
    }
    Ok(CallerLock { file })
}

fn try_lock(root: &Root, scope_digest: &str) -> Result<Option<CallerLock>> {
    control::ensure_private_dir(&root.dir.join("callers"))?;
    let dir = root.callers(scope_digest);
    control::ensure_private_dir(&dir)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("lock"))
        .context("open caller lock")?;
    // SAFETY: flock on a descriptor this function owns.
    let result = unsafe {
        libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&file),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    };
    if result == 0 {
        Ok(Some(CallerLock { file }))
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(error).context("lock caller identity")
        }
    }
}

/// The caller lock for admission. A live run's holder rejects at once; a
/// transient holder (admission, reconciliation or a finishing worker that
/// already cleared its active pointer) is waited for briefly.
fn admission_lock(root: &Root, scope_digest: &str) -> Result<Option<CallerLock>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(lock) = try_lock(root, scope_digest)? {
            return Ok(Some(lock));
        }
        let live = read_active(root, scope_digest)?.is_some_and(|active| {
            read_json::<Record>(&root.run(&active.run_id).join("record.json"))
                .map_or(true, |r| !r.phase.terminal())
        });
        if live || Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Active {
    run_id: String,
    generation: String,
}

fn active_path(root: &Root, scope_digest: &str) -> PathBuf {
    root.callers(scope_digest).join("active.json")
}

fn read_active(root: &Root, scope_digest: &str) -> Result<Option<Active>> {
    let path = active_path(root, scope_digest);
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

fn clear_active(root: &Root, scope_digest: &str) -> Result<()> {
    let path = active_path(root, scope_digest);
    match std::fs::remove_file(&path) {
        Ok(()) => control::sync_dir(&root.callers(scope_digest)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("clear active run"),
    }
}

/// Any live process whose command line carries the generation token.
fn generation_alive(generation: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .any(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|cmdline| {
                cmdline
                    .split(|b| *b == 0)
                    .any(|arg| arg == generation.as_bytes())
            })
        })
}

/// Fail closed: true unless every recorded process of the run is proven gone.
fn run_alive(record: &Record) -> bool {
    if std::fs::read_dir("/proc").is_err() {
        return true;
    }
    let boot = boot_id();
    // Without this boot's identity no recorded process can be proven gone.
    if boot == control::UNKNOWN_BOOT {
        return true;
    }
    if record.submitter.as_ref().is_some_and(process_matches) {
        return true;
    }
    if record
        .worker
        .as_ref()
        .is_some_and(|w| !control::same_boot(&w.boot_id, &boot))
    {
        // A changed boot identity: every recorded process belongs to the old boot.
        return false;
    }
    generation_alive(&record.generation)
        || record.worker.as_ref().is_some_and(|w| {
            // The worker leads the run's session; any member is live work.
            control::same_boot(&w.boot_id, &boot) && !control::session_members(w.pid).is_empty()
        })
        || record
            .children
            .iter()
            .any(|child| control::same_boot(&child.boot_id, &boot) && control::group_alive(child))
}

pub enum Admission {
    Idle,
    Busy(String),
}

/// Requires the caller lock. Persists `interrupted` only after proving the
/// recorded run's processes are gone; never replays anything.
fn reconcile(root: &Root, scope_digest: &str, by: &str) -> Result<Admission> {
    let admission = reconcile_active(root, scope_digest, by)?;
    clear_finished_degraded(root);
    if matches!(admission, Admission::Idle) {
        sweep_orphans(root, scope_digest, by);
    }
    Ok(admission)
}

/// Remove a renewal-failure marker whose named run is durably terminal and
/// no longer its caller's active run: that run blocks nothing any more. A
/// marker of a live (nonterminal) run, or one that cannot be read, stays.
fn clear_finished_degraded(root: &Root) {
    let Ok(named) = std::fs::read_to_string(root.degraded()) else {
        return;
    };
    let Ok(run_id) = run_id(named.trim()) else {
        return;
    };
    let Ok(record) = read_json::<Record>(&root.run(run_id).join("record.json")) else {
        return;
    };
    let active = read_active(root, &record.scope_digest);
    if record.phase.terminal() && active.is_ok_and(|a| a.is_none_or(|a| a.run_id != run_id)) {
        clear_degraded(&root.degraded(), run_id);
    }
}

/// Requires the caller lock and no active pointer. A nonterminal record that
/// never became (or no longer is) the active run, with every recorded process
/// proven gone (for example a submission killed between writing its record
/// and its active pointer), is recorded as interrupted. No worker can start
/// it: workers require the active pointer.
fn sweep_orphans(root: &Root, scope_digest: &str, by: &str) {
    let Ok(entries) = std::fs::read_dir(root.runs()) else {
        return;
    };
    for entry in entries.flatten() {
        let run = entry.path();
        let Ok(mut record) = read_json::<Record>(&run.join("record.json")) else {
            continue;
        };
        if record.scope_digest != scope_digest || record.phase.terminal() || run_alive(&record) {
            continue;
        }
        record.terminal = Some(Terminal {
            exit_code: None,
            success: None,
            reason: Some(format!(
                "interrupted in phase {}: abandoned without an active pointer and its processes are gone (confirmed by {by} reconciliation)",
                record.phase.name()
            )),
        });
        record.enter(Phase::Interrupted);
        let _ = write_json(&run.join("record.json"), &record);
    }
}

fn reconcile_active(root: &Root, scope_digest: &str, by: &str) -> Result<Admission> {
    let Some(active) = read_active(root, scope_digest)? else {
        return Ok(Admission::Idle);
    };
    let run = root.run(&active.run_id);
    let mut record: Record = match read_json(&run.join("record.json")) {
        Ok(record) => record,
        // An unreadable record cannot prove anything: stay blocked.
        Err(_) => return Ok(Admission::Busy(active.run_id)),
    };
    if record.phase.terminal() {
        // A worker that died after its terminal record left its marker.
        clear_degraded(&root.degraded(), &record.run_id);
        clear_active(root, scope_digest)?;
        return Ok(Admission::Idle);
    }
    if run_alive(&record) {
        return Ok(Admission::Busy(active.run_id));
    }
    record.terminal = Some(Terminal {
        exit_code: None,
        success: None,
        reason: Some(format!(
            "interrupted in phase {}: the run's worker and descendants are gone (confirmed by {by} reconciliation)",
            record.phase.name()
        )),
    });
    super::testhook::pause("reconcile-proven");
    record.enter(Phase::Interrupted);
    write_json(&run.join("record.json"), &record)?;
    if let Ok(events) = Events::open(&run) {
        events.emit(json!({"type":"sprite_phase", "run_id": record.run_id, "phase": "interrupted",
            "message": format!("sprite run {} interrupted (reconciled; not rerun)", record.run_id)}));
        events.sync();
    }
    // The dead run can no longer renew: drop its renewal-failure marker (only
    // a marker naming this run is removed).
    clear_degraded(&root.degraded(), &record.run_id);
    clear_active(root, scope_digest)?;
    let _ = hold::release(&record.run_id);
    Ok(Admission::Idle)
}

fn reconcile_all(root: &Root) {
    let Ok(entries) = std::fs::read_dir(root.dir.join("callers")) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(scope) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Ok(Some(_lock)) = try_lock(root, &scope) {
            let _ = reconcile(root, &scope, "supervisor startup");
        }
    }
}

// ---------------------------------------------------------------------------
// Events

struct Events(Mutex<std::fs::File>);

impl Events {
    fn open(run: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(run.join("events.jsonl"))
            .context("open run events")?;
        Ok(Events(Mutex::new(file)))
    }

    fn emit(&self, mut event: Value) {
        event["schema_version"] = json!(1);
        let mut line = serde_json::to_vec(&event).unwrap_or_default();
        line.push(b'\n');
        if let Ok(mut file) = self.0.lock() {
            let _ = file.write_all(&line);
        }
    }

    fn sync(&self) {
        if let Ok(file) = self.0.lock() {
            let _ = file.sync_all();
        }
    }
}

fn enter(record: &mut Record, run: &Path, events: &Events, phase: Phase) -> Result<()> {
    record.enter(phase);
    write_json(&run.join("record.json"), record)?;
    events.emit(
        json!({"type":"sprite_phase", "run_id": record.run_id, "phase": phase.name(),
        "message": format!("sprite run {}: {}", record.run_id, phase.name())}),
    );
    Ok(())
}

fn finish(
    record: &mut Record,
    run: &Path,
    events: &Events,
    phase: Phase,
    terminal: Terminal,
) -> Result<()> {
    let message = match (&terminal.reason, terminal.exit_code) {
        (Some(reason), _) => format!("sprite run {}: {}: {reason}", record.run_id, phase.name()),
        (None, Some(code)) => format!(
            "sprite run {}: {} (workflow exit {code})",
            record.run_id,
            phase.name()
        ),
        _ => format!("sprite run {}: {}", record.run_id, phase.name()),
    };
    record.terminal = Some(terminal);
    record.enter(phase);
    events.emit(
        json!({"type":"sprite_phase", "run_id": record.run_id, "phase": phase.name(),
        "terminal": record.terminal, "message": message}),
    );
    events.sync();
    write_json(&run.join("record.json"), record)
}

// ---------------------------------------------------------------------------
// Submission: admission, upload, verification and handoff

fn self_identity(generation: &str) -> ProcessIdentity {
    let pid = std::process::id();
    ProcessIdentity {
        boot_id: boot_id(),
        pid,
        start_ticks: start_ticks(pid).unwrap_or_default(),
        generation: generation.to_owned(),
    }
}

fn read_frame(input: &mut impl Read, max: u64) -> Result<Vec<u8>> {
    let mut len = [0u8; 8];
    input
        .read_exact(&mut len)
        .context("read submission frame")?;
    let len = u64::from_be_bytes(len);
    if len > max {
        bail!("submission frame too large");
    }
    let mut bytes = vec![0; len as usize];
    input
        .read_exact(&mut bytes)
        .context("read submission frame")?;
    Ok(bytes)
}

fn submit(root: &Root, scope: &Scope) -> Result<u8> {
    verify_owner(root, scope)?;
    root.ensure()?;
    let mut stdin = std::io::stdin().lock();
    let header: Header =
        serde_json::from_slice(&read_frame(&mut stdin, 256 * 1024 * 1024)?).context("header")?;
    // Values stay in memory only; never written to disk or argv.
    let env: BTreeMap<String, String> =
        serde_json::from_slice(&read_frame(&mut stdin, 64 * 1024 * 1024)?).context("env")?;
    run_id(&header.run_id)?;
    if header.protocol != PROTOCOL {
        bail!(
            "protocol mismatch: controller {} runner {PROTOCOL}",
            header.protocol
        );
    }
    if header.scope_digest != scope.digest() {
        bail!("submission scope does not match this Sprite's ownership");
    }
    if header.manifest_sha256 != Header::manifest_digest(&header.manifest) {
        bail!("snapshot manifest digest mismatch");
    }
    let run = root.run(&header.run_id);
    // A repeated UUID is only a lookup of the original record.
    if run.exists() {
        let record: Option<Record> = read_json(&run.join("record.json")).ok();
        return print_json(&json!({"status": "existing", "record": record}));
    }
    let Some(lock) = admission_lock(root, &header.scope_digest)? else {
        let active = read_active(root, &header.scope_digest)?.map(|a| a.run_id);
        return print_json(&json!({"status": "caller_busy", "active_run_id": active}));
    };
    if root.destroying().exists() {
        return print_json(
            &json!({"status": "refused", "reason": "destruction of this Sprite is in progress"}),
        );
    }
    // Reconcile first: a proven-dead run's renewal-failure marker goes with it.
    if let Admission::Busy(active) = reconcile(root, &header.scope_digest, "admission")? {
        return print_json(&json!({"status": "caller_busy", "active_run_id": active}));
    }
    if root.degraded().exists() {
        return print_json(
            &json!({"status": "refused", "reason": "wake hold renewal failed; admission is blocked until the affected run finalizes"}),
        );
    }
    super::testhook::pause("admission-locked");
    // Durable acceptance: run directory, record, then the active pointer.
    crate::fsutil::dir_builder(false)
        .create(&run)
        .context("create run directory")?;
    let generation = uuid::Uuid::new_v4().hyphenated().to_string();
    let mut record = Record::new(&header, now_ms(), generation.clone());
    record.submitter = Some(self_identity("submit"));
    write_json(&run.join("header.json"), &header)?;
    write_json(&run.join("record.json"), &record)?;
    super::testhook::pause("accept-record");
    write_json(
        &active_path(root, &header.scope_digest),
        &Active {
            run_id: header.run_id.clone(),
            generation: generation.clone(),
        },
    )?;
    let events = Events::open(&run)?;
    events.emit(
        json!({"type":"sprite_phase", "run_id": header.run_id, "phase": "accepted",
        "message": format!("sprite run {} accepted", header.run_id)}),
    );
    enter(&mut record, &run, &events, Phase::Uploading)?;
    super::testhook::pause("uploading");
    // A stalled upload abandons only this run's transfer phase.
    let watchdog = {
        let (run, root, scope_digest, done) = (
            run.clone(),
            root.clone(),
            header.scope_digest.clone(),
            Arc::new(AtomicBool::new(false)),
        );
        let flag = done.clone();
        let timeout = Duration::from_secs(header.upload_timeout_secs.max(1));
        std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !flag.load(Ordering::SeqCst) {
                let _ = fail_transfer(&root, &run, &scope_digest, "upload timed out");
                std::process::exit(3);
            }
        });
        done
    };
    let received = receive(&run, &header, &mut stdin, &mut record, &events);
    watchdog.store(true, Ordering::SeqCst);
    if let Err(error) = received {
        let reason = format!("{error:#}");
        fail_transfer(root, &run, &header.scope_digest, &reason)?;
        return print_json(&json!({"status": "transfer_failed", "reason": reason}));
    }
    // Activation is complete. The caller lock's description goes to the
    // supervisor with the handoff and on to the worker, so the identity stays
    // locked continuously; this process keeps its own reference until the
    // supervisor has answered.
    super::testhook::pause("handoff");
    let reply = handoff(root, &header.run_id, &generation, &env, &lock);
    drop(env);
    let result = match reply {
        Ok(()) => print_json(&json!({"status": "accepted", "run_id": header.run_id})),
        Err(error) => {
            let reason = format!("supervisor handoff failed: {error:#}");
            // A worker may share the lock description, so ownership changes
            // are serialized by the run's record lock instead: a worker that
            // registers later finds this terminal record and never starts.
            let _guard = record_lock(&run)?;
            let current: Record = read_json(&run.join("record.json"))?;
            if !current.phase.terminal() && current.worker.is_none() {
                let mut current = current;
                current.submitter = None;
                if !generation_alive(&generation) {
                    finish(
                        &mut current,
                        &run,
                        &events,
                        Phase::Interrupted,
                        Terminal {
                            exit_code: None,
                            success: None,
                            reason: Some(reason.clone()),
                        },
                    )?;
                    clear_active(root, &header.scope_digest)?;
                }
            }
            print_json(&json!({"status": "unresolved", "reason": reason}))
        }
    };
    drop(lock);
    result
}

fn fail_transfer(root: &Root, run: &Path, scope_digest: &str, reason: &str) -> Result<()> {
    let _ = make_writable_and_remove(&run.join("source.incoming"));
    let mut record: Record = read_json(&run.join("record.json"))?;
    record.submitter = None;
    let events = Events::open(run)?;
    finish(
        &mut record,
        run,
        &events,
        Phase::TransferFailed,
        Terminal {
            exit_code: None,
            success: None,
            reason: Some(reason.to_owned()),
        },
    )?;
    clear_active(root, scope_digest)
}

fn make_writable_and_remove(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(dir)
}

struct Hashing<R> {
    inner: R,
    hasher: sha2::Sha256,
    bytes: u64,
    limit: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let remaining = self.limit - self.bytes;
        if remaining == 0 {
            return Ok(0);
        }
        let max = buf.len().min(remaining as usize);
        let n = self.inner.read(&mut buf[..max])?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// A relative path made only of normal components.
pub fn safe_relative(path: &[u8]) -> bool {
    !path.is_empty()
        && !path.starts_with(b"/")
        && path
            .split(|b| *b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b"..")
}

fn receive(
    run: &Path,
    header: &Header,
    input: &mut impl Read,
    record: &mut Record,
    events: &Events,
) -> Result<()> {
    use sha2::Digest;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let incoming = run.join("source.incoming");
    crate::fsutil::dir_builder(false).create(&incoming)?;
    let mut expected: BTreeMap<Vec<u8>, &control::ManifestEntry> = BTreeMap::new();
    for entry in &header.manifest {
        expected.insert(snapshot::decode_path(&entry.path)?, entry);
    }
    let mut reader = Hashing {
        inner: input,
        hasher: sha2::Sha256::new(),
        bytes: 0,
        limit: header.archive_bytes,
    };
    let mut seen = BTreeSet::new();
    {
        let gz = flate2::read::GzDecoder::new(&mut reader);
        let mut archive = tar::Archive::new(gz);
        for entry in archive.entries().context("read source archive")? {
            let mut entry = entry.context("read source archive")?;
            if entry.header().entry_type() != tar::EntryType::Regular {
                bail!("source archive contains a non-regular entry");
            }
            let path = entry.path_bytes().into_owned();
            if !safe_relative(&path) {
                bail!("source archive contains an unsafe path");
            }
            let Some(manifest) = expected.get(&path) else {
                bail!(
                    "source archive contains {}, which is not in the manifest",
                    snapshot::encode_path(&path)
                );
            };
            let mode = entry.header().mode().context("read source archive")? & 0o7777;
            if mode != if manifest.executable { 0o755 } else { 0o644 } {
                bail!(
                    "{} has mode {mode:o}, which does not match the staged manifest",
                    manifest.path
                );
            }
            if !seen.insert(path.clone()) {
                bail!("source archive repeats {}", manifest.path);
            }
            let dest = incoming.join(OsStr::from_bytes(&path));
            if let Some(parent) = dest.parent() {
                crate::fsutil::dir_builder(true).create(parent)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&dest)
                .with_context(|| format!("extract {}", manifest.path))?;
            let mut hasher = sha2::Sha256::new();
            let mut size = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = entry.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n])?;
                size += n as u64;
            }
            file.sync_all()?;
            if size != manifest.size || format!("{:x}", hasher.finalize()) != manifest.sha256 {
                bail!("{} does not match the staged manifest", manifest.path);
            }
            std::fs::set_permissions(
                &dest,
                std::fs::Permissions::from_mode(if manifest.executable { 0o755 } else { 0o644 }),
            )?;
        }
    }
    std::io::copy(&mut reader, &mut std::io::sink())?;
    enter(record, run, events, Phase::Verifying)?;
    if reader.bytes != header.archive_bytes {
        bail!(
            "source archive truncated ({} of {} bytes)",
            reader.bytes,
            header.archive_bytes
        );
    }
    if format!("{:x}", reader.hasher.finalize()) != header.archive_sha256 {
        bail!("source archive checksum mismatch");
    }
    if seen.len() != expected.len() {
        bail!(
            "source archive is missing {} manifest files",
            expected.len() - seen.len()
        );
    }
    // Re-verify the extracted tree as activated: inventory, hashes, sizes, modes.
    verify_tree(&incoming, &header.manifest)?;
    std::fs::rename(&incoming, run.join("source")).context("activate verified source")?;
    control::sync_dir(run)?;
    Ok(())
}

/// The tree contains exactly the manifest's regular files with matching content.
pub fn verify_tree(dir: &Path, manifest: &[control::ManifestEntry]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let mut found = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(dir)?
                    .as_os_str()
                    .as_bytes()
                    .to_vec();
                found.insert(rel, entry.path());
            } else {
                bail!("activated source contains a non-regular entry");
            }
        }
    }
    if found.len() != manifest.len() {
        bail!(
            "activated source has {} files; manifest has {}",
            found.len(),
            manifest.len()
        );
    }
    for entry in manifest {
        let path = found
            .get(&snapshot::decode_path(&entry.path)?)
            .with_context(|| format!("{} missing after extraction", entry.path))?;
        let meta = std::fs::symlink_metadata(path)?;
        let bytes = std::fs::read(path)?;
        let executable = meta.permissions().mode() & 0o111 != 0;
        if bytes.len() as u64 != entry.size
            || sha256_hex(&bytes) != entry.sha256
            || executable != entry.executable
        {
            bail!("{} does not match the staged manifest", entry.path);
        }
    }
    Ok(())
}

fn supervisor_socket(root: &Root) -> PathBuf {
    root.control().join("supervisor.sock")
}

fn supervisor_call(root: &Root, request: &Value, timeout: Duration) -> Result<Value> {
    let mut stream = UnixStream::connect(supervisor_socket(root))
        .context("connect to the GoodFlows supervisor")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let value: Value = serde_json::from_str(&reply).context("malformed supervisor reply")?;
    if let Some(error) = value["error"].as_str() {
        bail!("{error}");
    }
    Ok(value)
}

/// Hand the accepted run to the supervisor together with the caller lock's
/// description (SCM_RIGHTS), which the supervisor passes to the worker.
fn handoff(
    root: &Root,
    run_id: &str,
    generation: &str,
    env: &BTreeMap<String, String>,
    lock: &CallerLock,
) -> Result<()> {
    let timeout = Duration::from_secs(90);
    let stream = UnixStream::connect(supervisor_socket(root))
        .context("connect to the GoodFlows supervisor")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(
        &json!({"op": "spawn", "run_id": run_id, "generation": generation, "env": env}),
    )?;
    line.push(b'\n');
    let sent = send_with_fd(&stream, &line, lock.fd()).context("hand over the caller lock")?;
    (&stream).write_all(&line[sent..])?;
    line.fill(0);
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    let value: Value = serde_json::from_str(&reply).context("malformed supervisor reply")?;
    if let Some(error) = value["error"].as_str() {
        bail!("{error}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Supervisor service

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SupervisorInfo {
    generation: String,
    pid: u32,
    start_ticks: u64,
    boot_id: String,
    runner: String,
    protocol: u32,
}

fn serve(root: &Root) -> Result<u8> {
    root.ensure()?;
    // Startup reconciliation runs before the listener opens and never relaunches.
    super::testhook::pause("startup-reconcile");
    reconcile_all(root);
    let runner = std::env::current_exe()?.to_string_lossy().into_owned();
    let info = SupervisorInfo {
        generation: uuid::Uuid::new_v4().hyphenated().to_string(),
        pid: std::process::id(),
        start_ticks: start_ticks(std::process::id()).unwrap_or_default(),
        boot_id: boot_id(),
        runner,
        protocol: PROTOCOL,
    };
    write_json(&root.control().join("supervisor.json"), &info)?;
    let socket = supervisor_socket(root);
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).context("bind supervisor socket")?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let info = Arc::new(info);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (root, info) = (root.clone(), info.clone());
        std::thread::spawn(move || {
            let _ = serve_one(&root, &info, stream);
        });
    }
    Ok(0)
}

fn serve_one(root: &Root, info: &SupervisorInfo, stream: UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    // The first segment may carry the submitter's caller lock (SCM_RIGHTS).
    let mut first = vec![0u8; 64 * 1024];
    let (received, lock_fd) = recv_with_fd(&stream, &mut first)?;
    first.truncate(received);
    let mut reader = BufReader::new(std::io::Cursor::new(first).chain(stream.try_clone()?));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    let reply = match request["op"].as_str() {
        Some("ping") => json!({"protocol": info.protocol, "generation": info.generation,
            "runner": info.runner, "version": env!("CARGO_PKG_VERSION")}),
        Some("spawn") => match lock_fd
            .context("the submission did not hand over its caller lock")
            .and_then(|lock_fd| spawn_worker(root, &request, lock_fd))
        {
            Ok(pid) => json!({"status": "registered", "pid": pid}),
            Err(error) => json!({"error": format!("{error:#}")}),
        },
        _ => json!({"error": "unknown supervisor request"}),
    };
    let mut stream = stream;
    let mut bytes = serde_json::to_vec(&reply)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    Ok(())
}

fn spawn_worker(root: &Root, request: &Value, lock: std::os::fd::OwnedFd) -> Result<u32> {
    let run_id = run_id(request["run_id"].as_str().context("run_id")?)?.to_owned();
    let generation = request["generation"]
        .as_str()
        .context("generation")?
        .to_owned();
    let run = root.run(&run_id);
    let stderr = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(run.join("worker.stderr"))?;
    let lock_fd = std::os::fd::AsRawFd::as_raw_fd(&lock);
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "__sprite-agent",
            "worker",
            &run_id,
            &generation,
            &lock_fd.to_string(),
        ])
        .current_dir(&run)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr);
    // SAFETY: setsid and fcntl in the child before exec. The worker leads a
    // session that contains every process of the run, so an unregistered
    // descendant (for example one spawned just before the worker died) still
    // blocks admission. Only this child inherits the caller lock: the
    // received descriptor is close-on-exec everywhere else.
    let mut child = unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::fcntl(lock_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    }
    .spawn()
    .context("spawn run worker")?;
    // The worker now holds the lock description; this copy is not needed.
    drop(lock);
    let pid = child.id();
    // Any failure before GO leaves a worker that must not outlive this call:
    // it is killed and reaped (it has started no user work without GO).
    let handshake = |child: &mut std::process::Child| -> Result<()> {
        if super::testhook::fault("worker-handshake") {
            bail!("injected failure during the worker handshake");
        }
        let mut stdin = child.stdin.take().context("worker stdin")?;
        let mut payload = serde_json::to_vec(&request["env"])?;
        payload.push(b'\n');
        stdin.write_all(&payload)?;
        drop(payload);
        let stdout = child.stdout.take().context("worker stdout")?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let registered = receiver.recv_timeout(REGISTER_WAIT).unwrap_or_default();
        let record: Record = read_json(&run.join("record.json"))?;
        let verified = registered.trim() == "registered"
            && record
                .worker
                .as_ref()
                .is_some_and(|w| w.pid == pid && w.generation == generation && process_matches(w));
        if !verified {
            // No GO: the worker exits on EOF without starting user work.
            bail!("worker did not register");
        }
        stdin.write_all(b"GO\n")?;
        Ok(())
    };
    if let Err(error) = handshake(&mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

fn ping(root: &Root) -> Option<Value> {
    supervisor_call(root, &json!({"op": "ping"}), Duration::from_secs(3)).ok()
}

fn active_runs(root: &Root) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.dir.join("callers")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| read_json::<Active>(&e.path().join("active.json")).ok())
        .map(|a| a.run_id)
        .collect()
}

fn ensure_supervisor(root: &Root) -> Result<u8> {
    root.ensure()?;
    // Concurrent controllers (a first-use race) must not both replace the
    // service: the second waits, then finds the first's supervisor ready.
    let serial = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.control().join("ensure.lock"))
        .context("open supervisor lock")?;
    // SAFETY: a blocking flock on a descriptor this function owns; released on drop.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&serial), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("lock supervisor service");
    }
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let before = ping(root);
    if let Some(reply) = &before {
        if reply["protocol"] == PROTOCOL && reply["runner"] == exe.as_str() {
            return print_json(&json!({"status": "ready", "generation": reply["generation"]}));
        }
    }
    // Replacing the service is safe only when no recorded run can still be
    // live: runs proven gone are reconciled by the new service's startup.
    let active: Vec<String> = active_runs(root)
        .into_iter()
        .filter(|id| {
            read_json::<Record>(&root.run(id).join("record.json"))
                .map_or(true, |r| !r.phase.terminal() && run_alive(&r))
        })
        .collect();
    if let Some(run) = active.first() {
        // The identity is busy: a contender (for example a controller with a
        // different runner build) gets the same rejection as at admission.
        return print_json(&json!({
            "status": "caller_busy",
            "active_run_id": run,
            "reason": format!(
                "the GoodFlows supervisor is {} while run {run} is active; it is not replaced under live work",
                if before.is_some() { "a different runner version" } else { "unreachable" },
            ),
        }));
    }
    let mut diagnostics = String::new();
    let mut run = |args: &[&str]| {
        if let Ok(output) = Command::new("sprite-env")
            .args(args)
            .stdin(Stdio::null())
            .output()
        {
            diagnostics.push_str(&String::from_utf8_lossy(&output.stdout));
            diagnostics.push_str(&String::from_utf8_lossy(&output.stderr));
        }
    };
    if before.is_some() || root.control().join("supervisor.json").exists() {
        run(&["services", "delete", SERVICE_NAME]);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    run(&[
        "services",
        "create",
        SERVICE_NAME,
        "--cmd",
        &exe,
        "--args",
        "__sprite-agent,serve",
        "--dir",
        &home,
        "--no-stream",
    ]);
    // The service CLI's exit status is not evidence: require protocol
    // readiness and a new generation running this exact runner.
    let old = before.as_ref().map(|r| r["generation"].clone());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Some(reply) = ping(root) {
            if reply["protocol"] == PROTOCOL
                && reply["runner"] == exe.as_str()
                && Some(&reply["generation"]) != old.as_ref()
            {
                return print_json(&json!({"status": "ready", "generation": reply["generation"]}));
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let diagnostics: String = diagnostics.chars().take(2000).collect();
    bail!(
        "the GoodFlows supervisor service did not become ready: {}",
        diagnostics.trim()
    )
}

// ---------------------------------------------------------------------------
// Worker

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_owned(), value);
        }
    }
    env.insert(
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
        }),
    );
    env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
    env
}

/// The private invocation config: the original text with every declared
/// workdir mapped into this run's verified source and managed result paths.
fn invocation_config(run: &Path, header: &Header) -> Result<String> {
    let source = run.join("source");
    let results = run.join("results");
    let mut value: toml::Table = toml::from_str(&header.config_text).context("parse config")?;
    let path = |rel: &str| -> Result<toml::Value> {
        let bytes = snapshot::decode_path(rel)?;
        let rel =
            String::from_utf8(bytes).context("non-UTF-8 workdirs are not supported remotely")?;
        Ok(toml::Value::String(
            source.join(rel).to_string_lossy().into_owned(),
        ))
    };
    let table = |parent: &mut toml::Table, key: &str| -> Result<toml::Table> {
        Ok(match parent.remove(key) {
            Some(toml::Value::Table(t)) => t,
            None => toml::Table::new(),
            Some(_) => bail!("config key {key} is not a table"),
        })
    };
    let mut settings = table(&mut value, "settings")?;
    match &header.workdirs.settings {
        Some(rel) => {
            settings.insert("workdir".into(), path(rel)?);
        }
        None => {
            settings.remove("workdir");
        }
    }
    settings.insert(
        "history_dir".into(),
        toml::Value::String(results.join("history").to_string_lossy().into_owned()),
    );
    value.insert("settings".into(), toml::Value::Table(settings));
    for (key, mapped) in [
        ("workflows", &header.workdirs.workflows),
        ("environments", &header.workdirs.environments),
    ] {
        let mut scopes = table(&mut value, key)?;
        for (name, scope) in scopes.iter_mut() {
            let toml::Value::Table(scope) = scope else {
                bail!("{key}.{name} is not a table");
            };
            let mut settings = table(scope, "settings")?;
            settings.remove("history_dir");
            if let Some(rel) = mapped.get(name) {
                settings.insert("workdir".into(), path(rel)?);
            }
            if !settings.is_empty() {
                scope.insert("settings".into(), toml::Value::Table(settings));
            }
        }
        if !scopes.is_empty() {
            value.insert(key.into(), toml::Value::Table(scopes));
        }
    }
    if let Some(toml::Value::Table(reporting)) = value.get_mut("reporting") {
        reporting.insert(
            "spool_dir".into(),
            toml::Value::String(results.join("reports").to_string_lossy().into_owned()),
        );
    }
    Ok(toml::to_string(&value)?)
}

struct Hold {
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Hold {
    fn start(root: &Root, run_id: &str) -> Result<Self> {
        let log = hold_log(&root.run(run_id));
        let created = hold::renew(run_id, HOLD_EXPIRE_SECS);
        log(json!({"op": "create", "ok": created.is_ok()}));
        created.context("create wake hold")?;
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (s, f, id, degraded) = (
            stop.clone(),
            failed.clone(),
            run_id.to_owned(),
            root.degraded(),
        );
        let thread = std::thread::spawn(move || {
            let mut last_ok = Instant::now();
            let mut degraded_written = false;
            loop {
                let wake = Instant::now() + Duration::from_secs(HOLD_RENEW_SECS);
                while !s.load(Ordering::SeqCst) && Instant::now() < wake {
                    std::thread::park_timeout(wake - Instant::now());
                }
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let renewed = hold::renew(&id, HOLD_EXPIRE_SECS);
                log(json!({"op": "renew", "ok": renewed.is_ok()}));
                match renewed {
                    Ok(()) => {
                        last_ok = Instant::now();
                        // Renewal recovered before cleanup was requested.
                        if degraded_written && !f.load(Ordering::SeqCst) {
                            clear_degraded(&degraded, &id);
                            degraded_written = false;
                        }
                    }
                    // Stopping: the run is already durably terminal.
                    Err(_) if s.load(Ordering::SeqCst) => break,
                    Err(_) => {
                        // Block new admission at once; request bounded cleanup
                        // before the hold can expire.
                        degraded_written = write_durable(&degraded, id.as_bytes()).is_ok();
                        if last_ok.elapsed()
                            >= Duration::from_secs(HOLD_EXPIRE_SECS - HOLD_RENEW_SECS)
                        {
                            f.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        });
        Ok(Hold {
            stop,
            failed,
            thread: Some(thread),
        })
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Append-only diagnostic record of this run's Tasks API hold traffic
/// (`hold.jsonl`): one line per create/renew and one before the final
/// release. Best effort; nothing depends on it, and nothing is written after
/// the final release.
fn hold_log(run: &Path) -> impl Fn(Value) + Send + 'static {
    let path = run.join("hold.jsonl");
    move |mut entry: Value| {
        entry["at_ms"] = json!(now_ms());
        let mut line = serde_json::to_vec(&entry).unwrap_or_default();
        line.push(b'\n');
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            let _ = file.write_all(&line);
        }
    }
}

/// Remove the admission block only when this run wrote it.
fn clear_degraded(path: &Path, run_id: &str) {
    if std::fs::read(path).is_ok_and(|owner| owner == run_id.as_bytes()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Termination grace after a hold renewal failure: cleanup must finish
/// before the last successful renewal's hold can expire.
const HOLD_FAILURE_GRACE: Duration = Duration::from_secs(2);

enum Outcome {
    Exited(i32),
    Cancelled(i32),
}

struct Worker<'a> {
    root: &'a Root,
    run: PathBuf,
    record: Record,
    events: Events,
    env: BTreeMap<String, String>,
    hold_failed: Arc<AtomicBool>,
}

impl Worker<'_> {
    fn cancel_requested(&self) -> bool {
        self.run.join("cancel").exists() || self.hold_failed.load(Ordering::SeqCst)
    }

    fn register_child(&mut self, role: &str, pid: u32) -> Result<()> {
        self.record.children.push(ChildIdentity {
            role: role.into(),
            boot_id: boot_id(),
            pgid: pid,
            start_ticks: start_ticks(pid).unwrap_or_default(),
        });
        write_json(&self.run.join("record.json"), &self.record)
    }

    /// Wait for a child, forwarding cancellation to it; then kill whatever
    /// remains of its session so no descendant outlives the phase.
    fn supervise(
        &mut self,
        child: &mut std::process::Child,
        group_signal: bool,
        grace: Duration,
    ) -> Result<Outcome> {
        let pid = child.id() as i32;
        let mut cancelled_at: Option<Instant> = None;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if cancelled_at.is_none() && self.cancel_requested() {
                cancelled_at = Some(Instant::now());
                if !self.record.cancel_requested {
                    self.record.cancel_requested = true;
                    write_json(&self.run.join("record.json"), &self.record)?;
                }
                // SAFETY: signalling the child (or its session group) this
                // worker spawned and has not yet reaped.
                unsafe {
                    libc::kill(if group_signal { -pid } else { pid }, libc::SIGTERM);
                }
            }
            let grace = if self.hold_failed.load(Ordering::SeqCst) {
                grace.min(HOLD_FAILURE_GRACE)
            } else {
                grace
            };
            if cancelled_at.is_some_and(|t| t.elapsed() >= grace) {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        kill_session(child.id());
        kill_own_session();
        let code = std::os::unix::process::ExitStatusExt::signal(&status)
            .map(|s| 128 + s)
            .or(status.code())
            .unwrap_or(1);
        Ok(if cancelled_at.is_some() {
            Outcome::Cancelled(code)
        } else {
            Outcome::Exited(code)
        })
    }
}

/// Kill every remaining process of a child's session/group, then wait briefly.
fn kill_session(id: u32) {
    for _ in 0..50 {
        let members = control::session_members(id);
        if members.is_empty() {
            return;
        }
        for pid in members {
            // SAFETY: plain signal delivery to members of our own child's session.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kill every other process left in the worker's own session (descendants
/// that moved to another process group), so none outlives the phase.
fn kill_own_session() {
    let me = std::process::id();
    for _ in 0..50 {
        let members: Vec<u32> = control::session_members(me)
            .into_iter()
            .filter(|pid| *pid != me)
            .collect();
        if members.is_empty() {
            return;
        }
        for pid in members {
            // SAFETY: plain signal delivery to members of this worker's session.
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_output_reader(
    reader: impl Read + Send + 'static,
    file: std::fs::File,
    events: Arc<Events>,
    run_id: String,
    hook: &'static str,
    stream: &'static str,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut file = file;
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
            let _ = file.write_all(&line);
            events.emit(
                json!({"type":"sprite_hook_output", "run_id": run_id, "hook": hook,
                "stream": stream, "data": String::from_utf8_lossy(&line)}),
            );
            line.clear();
        }
        let _ = file.sync_all();
    })
}

fn worker(root: &Root, run_id: &str, generation: &str, lock_fd: &str) -> Result<u8> {
    let run = root.run(run_id);
    let header: Header = read_json(&run.join("header.json"))?;
    // The submitter's lock description, inherited at exec: held from
    // admission until after this run's terminal record is durable.
    let lock = inherited_lock(root, &header.scope_digest, lock_fd)?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let env: BTreeMap<String, String> = serde_json::from_str(&line).context("env payload")?;
    line.clear();
    let mut record = {
        let _guard = record_lock(&run)?;
        let mut record: Record = read_json(&run.join("record.json"))?;
        let active = read_active(root, &header.scope_digest)?;
        if record.phase.terminal()
            || record.generation != generation
            || active.as_ref().is_none_or(|a| a.run_id != run_id)
        {
            bail!("run {run_id} is no longer admitted; not starting it");
        }
        record.worker = Some(self_identity(generation));
        write_json(&run.join("record.json"), &record)?;
        record
    };
    {
        let mut out = std::io::stdout().lock();
        out.write_all(b"registered\n")?;
        out.flush()?;
    }
    super::testhook::pause("registered");
    std::io::stdin().read_line(&mut line)?;
    if line.trim() != "GO" {
        bail!("no GO from the supervisor");
    }
    let events = Events::open(&run)?;
    // The submit process handed over; this worker is now the only owner.
    record.submitter = None;
    write_json(&run.join("record.json"), &record)?;
    let mut hold = match Hold::start(root, run_id) {
        Ok(hold) => hold,
        Err(error) => {
            finish(
                &mut record,
                &run,
                &events,
                Phase::Interrupted,
                Terminal {
                    exit_code: None,
                    success: None,
                    reason: Some(format!("{error:#}; no hook or job started")),
                },
            )?;
            clear_active(root, &header.scope_digest)?;
            drop(lock);
            return Ok(1);
        }
    };
    super::testhook::pause("hold-created");
    let mut worker = Worker {
        root,
        run: run.clone(),
        record,
        events,
        env,
        hold_failed: hold.failed.clone(),
    };
    let result = execute(&mut worker, &header);
    let (phase, terminal) = match result {
        Ok(done) => done,
        Err(error) => {
            // An error can leave a started hook or job (and its descendants)
            // running: kill the whole session before the identity is freed.
            kill_own_session();
            let me = std::process::id();
            if control::session_members(me).iter().any(|pid| *pid != me) {
                // Something survived SIGKILL: keep the active pointer and the
                // nonterminal record, so reconciliation keeps the identity busy
                // (the caller lock itself is freed when this worker exits).
                return Err(error.context("worker error; descendants survived the session kill"));
            }
            (
                Phase::Interrupted,
                Terminal {
                    exit_code: None,
                    success: None,
                    reason: Some(format!("worker error: {error:#}")),
                },
            )
        }
    };
    // Finalize: descendants are already gone; flush results, then persist the
    // terminal record before the identity is freed and the hold released.
    sync_tree(&run.join("results"));
    super::testhook::pause("finalizing");
    let mut record = worker.record.clone();
    finish(&mut record, &run, &worker.events, phase, terminal)?;
    // Renewal stops only now that the terminal record is durable, and before
    // the marker is cleared, so no late renewal failure can rewrite it. The
    // final release still comes last.
    hold.stop();
    super::testhook::pause("terminal-written");
    // The run is durably terminal; it no longer blocks admission.
    clear_degraded(&worker.root.degraded(), run_id);
    clear_active(root, &header.scope_digest)?;
    super::testhook::pause("pointer-cleared");
    drop(lock);
    super::testhook::pause("unlocked");
    let active = if super::testhook::fault("release-diagnostics") {
        Err(anyhow::anyhow!("injected diagnostics read failure"))
    } else {
        read_active(root, &header.scope_digest)
    };
    hold_log(&run)(
        json!({"op": "release", "terminal_phase": record.phase.name(),
        // Diagnostics only: a read error here must not skip the hold release.
        "active_cleared": active.ok().map(|a| a.is_none()),
        "session_members_left": control::session_members(std::process::id()).len().saturating_sub(1)}),
    );
    // Nothing required happens after this call or depends on its response.
    let _ = hold::release(run_id);
    Ok(0)
}

fn sync_tree(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => sync_tree(&entry.path()),
            Ok(kind) if kind.is_file() => {
                if let Ok(file) = std::fs::File::open(entry.path()) {
                    let _ = file.sync_all();
                }
            }
            _ => {}
        }
    }
    let _ = control::sync_dir(dir);
}

fn execute(worker: &mut Worker, header: &Header) -> Result<(Phase, Terminal)> {
    let run = worker.run.clone();
    let terminal = |exit_code: Option<i32>, reason: Option<String>| Terminal {
        success: exit_code.map(|c| c == 0),
        exit_code,
        reason,
    };
    // Config: the original source config is unchanged; the private copy maps paths.
    let invocation = run.join("invocation.toml");
    let text = match invocation_config(&run, header) {
        Ok(text) => text,
        Err(error) => {
            return Ok((
                Phase::PreparationFailed,
                terminal(None, Some(format!("config: {error:#}"))),
            ))
        }
    };
    write_durable(&invocation, text.as_bytes())?;
    let workdir = match crate::config::Config::load(Some(&invocation)).and_then(|(doc, _)| {
        let (_, config) = doc.workflow(
            header.request.workflow.as_deref(),
            header.request.environment.as_deref(),
        )?;
        config.settings.workdir.context("workdir")
    }) {
        Ok(dir) => dir,
        Err(error) => {
            return Ok((
                Phase::PreparationFailed,
                terminal(None, Some(format!("config: {error:#}"))),
            ))
        }
    };
    for dir in ["results", "hooks"] {
        control::ensure_private_dir(&run.join(dir))?;
    }
    let state = worker.root.state();
    // Exactly one hook: setup until it has succeeded, resume afterwards.
    let prepared = worker.root.prepared().exists();
    let (phase, hook, command) = if prepared {
        (Phase::Resume, "resume", header.resume.clone())
    } else {
        (Phase::Setup, "setup", header.setup.clone())
    };
    // A cancel acknowledged before this point (during upload, verification
    // or handoff) starts nothing; see `cancel` for the lock's role.
    let guard = record_lock(&run)?;
    if worker.cancel_requested() {
        return Ok((
            Phase::Cancelled,
            terminal(None, Some(format!("cancelled before {hook} started"))),
        ));
    }
    worker.record.hook = Some(hook.into());
    worker.record.hook_digest = Some(sha256_hex(command.as_bytes()));
    enter(&mut worker.record, &run, &worker.events, phase)?;
    if prepared {
        // Changed setup text never silently reclassifies a prepared Sprite.
        let recorded: Option<Prepared> = read_json(&worker.root.prepared()).ok();
        if recorded.is_some_and(|p| p.setup_digest != sha256_hex(header.setup.as_bytes())) {
            worker.events.emit(json!({"type": "sprite_setup_changed", "run_id": worker.record.run_id,
                "message": "setup text changed since this Sprite was prepared; running resume, not setup (destroy the idle Sprite to prepare it again)"}));
        }
    }
    let open = |name: String| {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(run.join("hooks").join(name))
    };
    let mut child = unsafe {
        Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&workdir)
            .env_clear()
            .envs(base_env())
            .envs(&worker.env)
            .env(crate::config::SPRITE_STATE_ENV, &state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .pre_exec(|| {
                // A process group inside the worker's session: any descendant,
                // registered or not, stays visible to reconciliation.
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .context("start preparation hook")?
    };
    drop(guard);
    super::testhook::pause("spawned-hook");
    if super::testhook::fault("worker-error") {
        bail!("injected worker error after {hook} started");
    }
    worker.register_child(hook, child.id())?;
    let events = Arc::new(Events(Mutex::new(
        worker.events.0.lock().unwrap().try_clone()?,
    )));
    let readers = [
        spawn_output_reader(
            child.stdout.take().context("hook stdout")?,
            open(format!("{hook}.stdout"))?,
            events.clone(),
            worker.record.run_id.clone(),
            hook,
            "stdout",
        ),
        spawn_output_reader(
            child.stderr.take().context("hook stderr")?,
            open(format!("{hook}.stderr"))?,
            events,
            worker.record.run_id.clone(),
            hook,
            "stderr",
        ),
    ];
    let outcome = worker.supervise(&mut child, true, HOOK_GRACE)?;
    for reader in readers {
        let _ = reader.join();
    }
    match outcome {
        Outcome::Cancelled(_) => {
            return Ok((
                Phase::Cancelled,
                terminal(None, Some(format!("cancelled during {hook}"))),
            ))
        }
        Outcome::Exited(0) => {}
        Outcome::Exited(code) => {
            return Ok((
                Phase::PreparationFailed,
                terminal(
                    None,
                    Some(format!("{hook} exited with {code}; no job started")),
                ),
            ))
        }
    }
    if !prepared {
        write_json(
            &worker.root.prepared(),
            &Prepared {
                schema: 1,
                run_id: worker.record.run_id.clone(),
                setup_digest: sha256_hex(command.as_bytes()),
                prepared_at_ms: now_ms(),
            },
        )?;
    }
    let guard = record_lock(&run)?;
    if worker.cancel_requested() {
        return Ok((
            Phase::Cancelled,
            terminal(None, Some("cancelled before the workflow started".into())),
        ));
    }
    enter(&mut worker.record, &run, &worker.events, Phase::Running)?;
    let create = |name: &str| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(run.join(name))
    };
    let generation = worker.record.generation.clone();
    // Evaluated before fork: the pre-exec hook must not allocate.
    let fail_caps = super::testhook::caps_fault();
    let spawned = unsafe {
        Command::new(std::env::current_exe()?)
            .args([
                "__sprite-agent",
                "exec-workflow",
                &worker.record.run_id,
                &generation,
            ])
            // The run directory travels as an argument: forwarded env (such
            // as the caller's HOME) is for user commands, never for locating
            // runner state.
            .arg(&run)
            .current_dir(&run)
            .env_clear()
            .envs(base_env())
            .envs(&worker.env)
            .env(crate::config::SPRITE_STATE_ENV, &state)
            .stdin(Stdio::null())
            .stdout(create("workflow.jsonl")?)
            .stderr(create("workflow.stderr")?)
            .pre_exec(move || {
                // A process group inside the worker's session: any descendant,
                // registered or not, stays visible to reconciliation.
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if fail_caps {
                    return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                }
                drop_capabilities()
            })
            .spawn()
    };
    drop(guard);
    // A launch that fails (for example when capabilities cannot be dropped)
    // is a definite failure before any job: nothing ran, nothing is uncertain.
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Ok((
                Phase::PreparationFailed,
                terminal(None, Some(launch_failure(&error))),
            ))
        }
    };
    super::testhook::pause("spawned-workflow");
    worker.register_child("workflow", child.id())?;
    let outcome = worker.supervise(&mut child, false, WORKFLOW_GRACE)?;
    enter(&mut worker.record, &run, &worker.events, Phase::Finalizing)?;
    worker.record.workflow_exited = true;
    write_json(&run.join("record.json"), &worker.record)?;
    for name in ["workflow.jsonl", "workflow.stderr"] {
        if let Ok(file) = std::fs::File::open(run.join(name)) {
            let _ = file.sync_all();
        }
    }
    Ok(match outcome {
        Outcome::Exited(code) => (Phase::Completed, terminal(Some(code), None)),
        Outcome::Cancelled(code) => (
            Phase::Cancelled,
            Terminal {
                exit_code: Some(code),
                success: Some(false),
                reason: Some(if worker.hold_failed.load(Ordering::SeqCst) {
                    "wake hold renewal failed; the workflow was stopped".into()
                } else {
                    "cancelled by request".into()
                }),
            },
        ),
    })
}

/// Why the workflow runner could not be launched. `drop_capabilities` (and
/// its debug fault hook) report EPERM; any other error (fork EAGAIN, a
/// missing runner, ...) is named as the launch error it is.
fn launch_failure(error: &std::io::Error) -> String {
    if error.raw_os_error() == Some(libc::EPERM) {
        format!("the workflow runner could not be launched because its capabilities could not be dropped ({error}); no job started")
    } else {
        format!("the workflow runner could not be launched ({error}); no job started")
    }
}

/// Drop every capability set before the workflow runner executes: ambient,
/// bounding (or, without CAP_SETPCAP, forbid regaining privileges through
/// setuid with no_new_privs), effective, permitted and inheritable. Verified
/// before returning; an unverifiable restriction fails the launch.
#[cfg(target_os = "linux")]
pub fn drop_capabilities() -> std::io::Result<()> {
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const VERSION_3: u32 = 0x2008_0522;
    let fail = || Err(std::io::Error::from_raw_os_error(libc::EPERM));
    // SAFETY: prctl/capset/capget with valid, owned arguments. No allocation.
    unsafe {
        if libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        ) != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
        {
            return fail();
        }
        let mut bounding_dropped = true;
        for cap in 0..64 {
            if libc::prctl(libc::PR_CAPBSET_READ, cap, 0, 0, 0) == 1
                && libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) != 0
            {
                bounding_dropped = false;
            }
        }
        if !bounding_dropped && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return fail();
        }
        let mut header = CapHeader {
            version: VERSION_3,
            pid: 0,
        };
        let data = [CapData::default(); 2];
        if libc::syscall(libc::SYS_capset, &mut header, data.as_ptr()) != 0 {
            return fail();
        }
        let mut header = CapHeader {
            version: VERSION_3,
            pid: 0,
        };
        let mut got = [CapData::default(); 2];
        if libc::syscall(libc::SYS_capget, &mut header, got.as_mut_ptr()) != 0 {
            return fail();
        }
        if got
            .iter()
            .any(|d| d.effective | d.permitted | d.inheritable != 0)
        {
            return fail();
        }
        for cap in 0..64 {
            if libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_IS_SET, cap, 0, 0) == 1 {
                return fail();
            }
        }
        if !bounding_dropped && libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 {
            return fail();
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn drop_capabilities() -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
}

/// The `source` the controller captured from the snapshot, read once from
/// the header for the root and every child invocation. `None`
/// when the controller could not read HEAD. Jobs never see it.
fn source(provenance: &Provenance) -> Option<crate::reporting::Source> {
    Some(crate::reporting::Source {
        commit: Some(provenance.commit.clone()?),
        branch: provenance.branch.clone(),
        dirty: provenance.dirty,
    })
}

/// The workflow runner: the existing engine with this run's UUID and paths.
async fn exec_workflow(args: &[String]) -> Result<u8> {
    let run_id = run_id(args.first().context("run id")?)?.to_owned();
    // Never derived from this process's env, which includes forwarded values.
    let run = PathBuf::from(args.get(2).context("run directory")?);
    let header: Header = read_json(&run.join("header.json"))?;
    let (_signals, cancel) = crate::cancellation()?;
    let output = crate::output::Output::new(true, crate::output::Mode::Stream, cancel.clone())?;
    let results = run.join("results");
    let request = &header.request;
    let invoke = async {
        let (document, _) = crate::config::Config::load(Some(&run.join("invocation.toml")))?;
        document.environment(request.environment.as_deref())?;
        let legacy_default = !document.jobs.is_empty();
        let profile = request.environment.as_deref();
        let (name, mut config) = document.workflow(request.workflow.as_deref(), profile)?;
        config.select(&request.jobs)?;
        // Called workflows run their complete graphs, exactly as locally.
        let targets = document.called_workflows(&config.jobs, profile)?;
        let history = crate::config::HistoryOverrides {
            history: request.history,
            history_dir: Some(results.join("history")),
            log_runs: request.log_runs,
            history_runs: request.history_runs,
            history_bytes: request.history_bytes,
        };
        let mut output = output.clone();
        if let Some(environment) = &request.environment {
            output = output.with_environment(environment);
        }
        let run = crate::Run {
            calls: config.jobs.iter().any(|job| job.workflow.is_some()),
            root: config,
            targets,
            profile: request.environment.clone(),
            parallelism: request.parallelism,
            fail_fast: request.fail_fast,
            history,
            requested_jobs: request.jobs.clone(),
            log_dir: request.logs.then(|| results.join("logs")),
            legacy_default,
            config_path: None,
            output,
            cancel: cancel.clone(),
            sprite_state: std::env::var(crate::config::SPRITE_STATE_ENV).ok(),
            provenance: source(&header.provenance),
        };
        crate::execute(
            run,
            name,
            crate::retention::RunIdentity::with_id(run_id.clone()),
        )
        .await
    };
    match invoke.await {
        Ok(code) => Ok(code),
        Err(error) => {
            let _ = output
                .emit(json!({"type":"error", "message": format!("{error:#}")}))
                .await;
            Ok(cancel.borrow().map(|s| (128 + s) as u8).unwrap_or(1))
        }
    }
}

// ---------------------------------------------------------------------------
// Recovery operations

fn status(root: &Root, scope: &Scope, reconcile_requested: bool) -> Result<u8> {
    let owner = verify_owner(root, scope)?;
    let digest = scope.digest();
    let mut busy = false;
    let mut reconciled = false;
    match try_lock(root, &digest)? {
        Some(_lock) => {
            if reconcile_requested {
                reconciled = true;
                busy = matches!(reconcile(root, &digest, "status")?, Admission::Busy(_));
                // An abort that died between removing its claim and the
                // directory leaves an empty marker; claims are added only
                // under this lock, so an empty one belongs to no destroy.
                if std::fs::remove_dir(root.destroying()).is_ok() {
                    control::sync_dir(&root.control())?;
                }
            } else if let Some(active) = read_active(root, &digest)? {
                busy = read_json::<Record>(&root.run(&active.run_id).join("record.json"))
                    .map_or(true, |r| !r.phase.terminal());
            }
        }
        None => busy = true,
    }
    let active = read_active(root, &digest)?;
    let mut runs: Vec<Record> = std::fs::read_dir(root.runs())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| read_json::<Record>(&e.path().join("record.json")).ok())
                .filter(|r| r.scope_digest == digest)
                .collect()
        })
        .unwrap_or_default();
    runs.sort_by_key(|r| std::cmp::Reverse(r.accepted_at_ms));
    runs.truncate(20);
    let prepared: Option<Prepared> = read_json(&root.prepared()).ok();
    let summaries: Vec<Value> = runs
        .iter()
        .map(|r| {
            json!({"run_id": r.run_id, "phase": r.phase.name(), "workflow": r.workflow,
            "environment": r.environment, "accepted_at_ms": r.accepted_at_ms,
            "terminal": r.terminal, "timings": r.timings, "path": root.run(&r.run_id)})
        })
        .collect();
    print_json(&json!({
        "status": "ok",
        "owner": {"organization": owner.organization, "prefix": owner.prefix,
                  "identity": owner.identity, "name": owner.name, "sprite_id": owner.sprite_id},
        "prepared": prepared.as_ref().map(|p| json!({"run_id": p.run_id, "setup_digest": p.setup_digest, "prepared_at_ms": p.prepared_at_ms})),
        "active_run_id": active.map(|a| a.run_id),
        "busy": busy,
        "reconciled": reconciled,
        "degraded": root.degraded().exists(),
        "destroying": root.destroying().exists(),
        "supervisor": ping(root),
        "runs": summaries,
    }))
}

/// Print saved events then workflow events; with `wait`, keep following until
/// the run is terminal and every file is fully read. Reads only.
fn follow(root: &Root, run_id: &str, wait: bool, skip: usize) -> Result<u8> {
    let run = root.run(run_id);
    if !run.join("record.json").exists() {
        bail!("run {run_id} does not exist on this Sprite");
    }
    let mut out = std::io::stdout().lock();
    if skip > 0 {
        serde_json::to_writer(&mut out, &json!({"type": "sprite_follow", "skip": skip}))?;
        writeln!(out)?;
        out.flush()?;
    }
    // Stream lines still to leave out; the order of the merged stream is the
    // same on every connection, so these are exactly the lines already sent.
    let mut skip = skip;
    let mut send = |out: &mut std::io::StdoutLock, line: &[u8]| -> std::io::Result<()> {
        if skip > 0 {
            skip -= 1;
            return Ok(());
        }
        out.write_all(line)
    };
    let (mut events_offset, mut workflow_offset) = (0u64, 0u64);
    let mut in_workflow = false;
    // When every process of a nonterminal run is proven gone (checked twice,
    // a second apart), nothing will ever finish it: stop following, report it
    // as not live, and leave the durable interruption to reconciliation.
    let mut gone_since: Option<Instant> = None;
    let mut last_check = Instant::now();
    loop {
        // Read the record first: a terminal record implies complete files.
        let record: Record = read_json(&run.join("record.json"))?;
        let mut progressed = false;
        if in_workflow {
            let exited = record.workflow_exited || record.phase.terminal();
            for (line, end) in read_lines(&run.join("workflow.jsonl"), workflow_offset)? {
                send(&mut out, &line)?;
                workflow_offset = end;
                progressed = true;
            }
            if exited || !wait {
                in_workflow = false;
                continue;
            }
        } else {
            for (line, end) in read_lines(&run.join("events.jsonl"), events_offset)? {
                send(&mut out, &line)?;
                events_offset = end;
                progressed = true;
                if serde_json::from_slice::<Value>(&line)
                    .is_ok_and(|v| v["type"] == "sprite_phase" && v["phase"] == "running")
                {
                    in_workflow = true;
                    break;
                }
            }
            if !in_workflow && !progressed && (record.phase.terminal() || !wait) {
                serde_json::to_writer(
                    &mut out,
                    &json!({"type": "sprite_record", "record": record}),
                )?;
                writeln!(out)?;
                out.flush()?;
                return Ok(0);
            }
        }
        out.flush()?;
        if wait
            && !progressed
            && !record.phase.terminal()
            && last_check.elapsed() >= Duration::from_secs(1)
        {
            last_check = Instant::now();
            if run_alive(&record) {
                gone_since = None;
            } else if gone_since.get_or_insert_with(Instant::now).elapsed()
                >= Duration::from_secs(1)
            {
                serde_json::to_writer(
                    &mut out,
                    &json!({"type": "sprite_record", "record": record, "live": false}),
                )?;
                writeln!(out)?;
                out.flush()?;
                return Ok(0);
            }
        }
        if !progressed {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Complete lines after `offset`, each with the offset just past it.
fn read_lines(path: &Path, offset: u64) -> Result<Vec<(Vec<u8>, u64)>> {
    use std::io::Seek;
    let mut lines = Vec::new();
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok(lines);
    };
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut position = offset;
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || !line.ends_with(b"\n") {
            return Ok(lines);
        }
        position += n as u64;
        lines.push((line, position));
    }
}

fn cancel(root: &Root, scope: &Scope, run_id: &str) -> Result<u8> {
    verify_owner(root, scope)?;
    let run = root.run(run_id);
    let record: Record = read_json(&run.join("record.json"))
        .with_context(|| format!("run {run_id} does not exist on this Sprite"))?;
    if record.scope_digest != scope.digest() {
        bail!("run {run_id} belongs to a different caller identity");
    }
    // Acknowledged under the record lock, which the worker holds from its
    // cancellation check until a hook or workflow process exists: a request
    // is either seen before anything starts or forwarded to what started.
    let _guard = record_lock(&run)?;
    let record: Record = read_json(&run.join("record.json"))?;
    if record.phase.terminal() {
        return print_json(
            &json!({"status": "already_terminal", "run_id": run_id, "phase": record.phase.name()}),
        );
    }
    // UUID-bound: only this run's worker reads this path.
    write_durable(&run.join("cancel"), now_ms().to_string().as_bytes())?;
    print_json(&json!({"status": "requested", "run_id": run_id, "phase": record.phase.name()}))
}

/// Saved results as a gzip tar: `bundle.json` (inventory, hashes, completeness)
/// first, then each listed regular file.
/// Files up to this size are read into memory once, while the in-memory
/// total stays under [`EXPORT_INLINE_TOTAL`]; larger files are hashed while
/// they are read and streamed into the bundle without being buffered.
const EXPORT_INLINE_FILE: u64 = 1024 * 1024;
const EXPORT_INLINE_TOTAL: u64 = 64 * 1024 * 1024;

/// At most `size` bytes of `inner`, hashed as they are read.
fn hashing<R: Read>(inner: R, size: u64) -> Hashing<R> {
    use sha2::Digest;
    Hashing {
        inner,
        hasher: sha2::Sha256::new(),
        bytes: 0,
        limit: size,
    }
}

/// The hash and byte count of what a [`Hashing`] reader passed through.
fn hashed<R>(reader: Hashing<R>) -> (String, u64) {
    use sha2::Digest;
    (format!("{:x}", reader.hasher.finalize()), reader.bytes)
}

enum ExportBody {
    Inline(Vec<u8>),
    Stream(PathBuf),
}

struct ExportFile {
    rel: String,
    size: u64,
    sha256: String,
    body: ExportBody,
}

fn export(root: &Root, run_id: &str) -> Result<u8> {
    let run = root.run(run_id);
    let record: Record = read_json(&run.join("record.json"))
        .with_context(|| format!("run {run_id} does not exist on this Sprite"))?;
    let mut files: Vec<ExportFile> = Vec::new();
    let mut total = 0u64;
    let mut inline_total = 0u64;
    let mut add = |rel: String, path: &Path| -> Result<()> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Ok(());
        }
        // Finite limits from metadata, before any byte is read.
        let size = meta.len();
        if files.len() >= MAX_BUNDLE_FILES || total + size > MAX_BUNDLE_BYTES {
            bail!("saved results exceed the collection limits ({MAX_BUNDLE_FILES} files, {MAX_BUNDLE_BYTES} bytes)");
        }
        total += size;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        // A file still being written is exported as the prefix that existed
        // when it was listed; it never grows past its listed size here.
        let entry = if size <= EXPORT_INLINE_FILE && inline_total + size <= EXPORT_INLINE_TOTAL {
            let mut bytes = Vec::with_capacity(size as usize);
            file.take(size).read_to_end(&mut bytes)?;
            inline_total += bytes.len() as u64;
            ExportFile {
                rel,
                size: bytes.len() as u64,
                sha256: sha256_hex(&bytes),
                body: ExportBody::Inline(bytes),
            }
        } else {
            let mut reader = hashing(file, size);
            std::io::copy(&mut reader, &mut std::io::sink())?;
            let (sha256, count) = hashed(reader);
            if count != size {
                bail!("{rel} changed while it was exported; collect again");
            }
            ExportFile {
                rel,
                size,
                sha256,
                body: ExportBody::Stream(path.to_path_buf()),
            }
        };
        files.push(entry);
        Ok(())
    };
    for name in [
        "record.json",
        "header.json",
        "events.jsonl",
        "workflow.jsonl",
        "workflow.stderr",
        "worker.stderr",
    ] {
        if run.join(name).exists() {
            add(name.into(), &run.join(name))?;
        }
    }
    for dir in ["hooks", "results"] {
        let mut stack = vec![run.join(dir)];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file() {
                    let rel = entry
                        .path()
                        .strip_prefix(&run)?
                        .to_str()
                        .context("result paths must be UTF-8")?
                        .to_owned();
                    add(rel, &entry.path())?;
                }
            }
        }
    }
    let manifest = json!({
        "schema": 1,
        "run_id": run_id,
        "complete": record.phase.terminal(),
        "phase": record.phase.name(),
        // The remote run root named by saved events and history (summary
        // `log_dir`, history paths); each file maps to `remote_path` there.
        "remote_root": run.to_str().context("the run directory must be UTF-8")?,
        "files": files.iter().map(|f| json!({"path": f.rel, "size": f.size, "sha256": f.sha256,
            "remote_path": run.join(&f.rel)})).collect::<Vec<_>>(),
    });
    let stdout = std::io::stdout().lock();
    let gz = flate2::write::GzEncoder::new(stdout, flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    let manifest = serde_json::to_vec(&manifest)?;
    let header = |size: u64| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(size);
        header.set_mode(0o600);
        header.set_mtime(0);
        header
    };
    builder.append_data(
        &mut header(manifest.len() as u64),
        "bundle.json",
        &manifest[..],
    )?;
    for file in &files {
        match &file.body {
            ExportBody::Inline(bytes) => {
                builder.append_data(&mut header(file.size), &file.rel, &bytes[..])?;
            }
            ExportBody::Stream(path) => {
                // The second read must reproduce the inventory's hash; any
                // change fails this export (exit nonzero) instead of
                // shipping bytes that do not match the inventory.
                let source = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)?;
                let mut reader = hashing(source, file.size);
                let appended = builder.append_data(&mut header(file.size), &file.rel, &mut reader);
                let (sha256, count) = hashed(reader);
                if count != file.size || sha256 != file.sha256 {
                    bail!("{} changed while it was exported; collect again", file.rel);
                }
                appended?;
            }
        }
    }
    let mut out = builder.into_inner()?.finish()?;
    out.flush()?;
    Ok(0)
}

fn prepare_destroy(root: &Root, scope: &Scope) -> Result<u8> {
    // A definite refusal before any claim exists, so the controller can tell
    // it from a failure that may have left one.
    let owner = match verify_owner(root, scope) {
        Ok(owner) => owner,
        Err(error) => {
            return print_json(&json!({"status": "refused", "reason": format!("{error:#}")}))
        }
    };
    let digest = scope.digest();
    let Some(_lock) = admission_lock(root, &digest)? else {
        let active = read_active(root, &digest)?.map(|a| a.run_id);
        return print_json(&json!({"status": "caller_busy", "active_run_id": active}));
    };
    if let Admission::Busy(active) = reconcile(root, &digest, "destroy check")? {
        return print_json(&json!({"status": "caller_busy", "active_run_id": active}));
    }
    // New admissions check this marker under the same lock.
    let token = add_destroy_marker(root)?;
    print_json(&json!({"status": "ready", "sprite_id": owner.sprite_id, "token": token}))
}

/// A destroy marker older than this belongs to a destroy that should no
/// longer be running (the controller bounds each destroy well below it), for
/// example one whose controller was killed. Only a later destroy sweeps it.
/// A suspended controller can outlive its claim, so every destroy checks
/// its claim (`check-destroy`) just before the delete call.
const DESTROY_STALE_MS: u64 = 15 * 60 * 1000;

/// Add this destroy's token to the destruction marker (`control/destroying/`,
/// one file per in-flight destroy) and sweep stale tokens of earlier ones.
/// The marker exists, refusing new runs, while any token is present. Runs
/// under the caller lock, like every admission that checks the marker.
fn add_destroy_marker(root: &Root) -> Result<String> {
    let dir = root.destroying();
    let token = uuid::Uuid::new_v4().hyphenated().to_string();
    loop {
        control::ensure_private_dir(&dir)?;
        match write_token(&dir, &token, &now_ms().to_string()) {
            Ok(()) => break,
            // The last aborting destroy removed the directory in between.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("write the destruction marker"),
        }
    }
    control::sync_dir(&dir)?;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        if entry.file_name().to_str() == Some(token.as_str()) {
            continue;
        }
        let written = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok());
        // Unreadable or undated tokens count as stale too: no live destroy
        // writes one.
        if written.is_none_or(|ms| now_ms().saturating_sub(ms) >= DESTROY_STALE_MS) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(token)
}

fn write_token(dir: &Path, name: &str, written_ms: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(name))?;
    file.write_all(written_ms.as_bytes())?;
    file.sync_all()
}

/// Remove only this destroy's token; the marker disappears with the last
/// token (`rmdir` fails while another destroy's token remains). Returns
/// whether no destruction marker is left.
fn remove_destroy_marker(root: &Root, token: &str) -> Result<bool> {
    let dir = root.destroying();
    match std::fs::remove_file(dir.join(token)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove the destruction marker"),
    }
    if std::fs::remove_dir(&dir).is_ok() {
        control::sync_dir(&root.control())?;
    }
    Ok(!dir.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_round_trip_through_hex() {
        let scope = Scope {
            organization: "org".into(),
            prefix: "gf".into(),
            identity: "--weird 'id'\u{e9}".into(),
        };
        assert_eq!(decode_scope(&encode_scope(&scope)).unwrap(), scope);
        assert!(decode_scope("zz").is_err());
    }

    #[test]
    fn launch_failures_blame_capabilities_only_for_eperm() {
        let caps = launch_failure(&std::io::Error::from_raw_os_error(libc::EPERM));
        assert!(caps.contains("capabilities could not be dropped"), "{caps}");
        for errno in [libc::EAGAIN, libc::ENOENT, libc::ENOMEM] {
            let other = launch_failure(&std::io::Error::from_raw_os_error(errno));
            assert!(!other.contains("capabilit"), "{other}");
            assert!(
                other.starts_with("the workflow runner could not be launched ("),
                "{other}"
            );
            assert!(other.ends_with("; no job started"), "{other}");
        }
    }

    #[test]
    fn unsafe_archive_paths_are_rejected() {
        for bad in [&b"../x"[..], b"/abs", b"a/../b", b"a//b", b"", b"./a"] {
            assert!(!safe_relative(bad), "{}", String::from_utf8_lossy(bad));
        }
        assert!(safe_relative(b"a/b c/d"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capability_drop_leaves_no_capabilities() {
        let output = unsafe {
            Command::new("sh")
                .args([
                    "-c",
                    "grep -E '^(CapInh|CapPrm|CapEff|CapAmb|CapBnd|NoNewPrivs)' /proc/self/status",
                ])
                .pre_exec(drop_capabilities)
                .output()
                .unwrap()
        };
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for field in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
            assert!(
                text.lines()
                    .any(|l| l.starts_with(field) && l.ends_with("0000000000000000")),
                "{text}"
            );
        }
        let bounding_empty = text
            .lines()
            .any(|l| l.starts_with("CapBnd") && l.ends_with("0000000000000000"));
        let no_new_privs = text.lines().any(|l| l == "NoNewPrivs:\t1");
        assert!(bounding_empty || no_new_privs, "{text}");
    }
}
