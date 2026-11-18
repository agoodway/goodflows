//! Job caches: keys, archives, the local store, the remote store, and the
//! restore and save lifecycle around a job. A cache only ever speeds a job
//! up: every failure here is a `cache_warning` and the job runs as if the
//! cache missed.

pub mod archive;
pub mod glob;
pub mod key;
pub mod local;
pub mod remote;

use crate::config::{Cache, CacheConfig, EffectiveCache, Partition, Reporting};
use crate::expression::{Expression, Reference};
use crate::output::Output;
use crate::runner::Status;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The run's signal state, as job tasks see it.
pub type Cancel = watch::Receiver<Option<i32>>;

/// Which jobs of the run execute, so a save archives only a tree that no
/// job is writing (jobs share the workdir and may write cached paths).
pub trait Activity: Send + Sync {
    /// How many units have started executing in the run so far. Bumped
    /// after [`Activity::executing`] counts a starting unit.
    fn entered(&self) -> u64;
    /// How many units execute now.
    fn executing(&self) -> usize;
    /// Wait for a free slot of the save's invocation; dropping the guard
    /// frees it.
    fn slot(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Slot> + Send + '_>>;
}

/// A scheduler slot held by a save; dropping it frees the slot.
pub type Slot = Box<dyn Send>;

/// A job started or ran while its save archived; archive again.
#[derive(Debug)]
struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("another job ran while the cache was archived")
    }
}

impl std::error::Error for Busy {}

const CANCELLED: &str = "abandoned: the run was cancelled";

/// Resolves once the run is cancelled; never when the signal monitor is gone.
async fn cancelled(cancel: &Cancel) {
    let mut cancel = cancel.clone();
    if cancel.wait_for(Option::is_some).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Wait until no unit of the run executes and return [`Activity::entered`]
/// as read just before; `None` when the run is cancelled first.
async fn quiet(activity: &dyn Activity, cancel: &Cancel) -> Option<u64> {
    loop {
        if cancel.borrow().is_some() {
            return None;
        }
        let entered = activity.entered();
        if activity.executing() == 0 {
            return Some(entered);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            _ = cancelled(cancel) => return None,
        }
    }
}

/// Run read-only blocking `work` until `deadline` or cancellation. When
/// either comes first, `abort` is set and the work is left to stop on its
/// own (it writes nothing), so a large tree never holds the job up.
async fn detached<T: Send + 'static>(
    deadline: tokio::time::Instant,
    cancel: &Cancel,
    work: impl FnOnce(&AtomicBool) -> Result<T> + Send + 'static,
) -> Result<T> {
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let task = tokio::task::spawn_blocking(move || work(&flag));
    tokio::select! {
        joined = tokio::time::timeout_at(deadline, task) => match joined {
            Ok(joined) => joined.map_err(|_| anyhow!("a cache task panicked"))?,
            Err(_) => {
                abort.store(true, Ordering::Relaxed);
                Err(anyhow!("abandoned after the cache's timeout_secs"))
            }
        },
        _ = cancelled(cancel) => {
            abort.store(true, Ordering::Relaxed);
            Err(anyhow!(CANCELLED))
        }
    }
}

/// The name under which a Sprite controller forwards the minted cache key
/// (as [`Forwarded`] JSON) in the private forwarded-value frame. The worker
/// hands it to the runner on stdin; hooks and job commands never see it.
pub const SPRITE_CACHE_ENV: &str = "GOODFLOWS_SPRITE_CACHE";

pub fn key_hex(digest: impl AsRef<[u8]>) -> String {
    key::hex(digest)
}

/// What [`SPRITE_CACHE_ENV`] holds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forwarded {
    pub url: String,
    pub token: String,
    pub scope: Option<String>,
}

/// One cache's result, kept for the summary and the history record.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub name: String,
    pub result: &'static str,
    pub tier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<&'static str>,
}

/// Emits cache events for one cache of one job, or for the run.
pub struct Events<'a> {
    output: &'a Output,
    session: &'a Session,
    job: Option<(&'a str, Option<Partition>, &'a str)>,
}

impl Events<'_> {
    async fn emit(&self, mut event: Value) {
        if let Some((job, partition, cache)) = self.job {
            event["job"] = json!(job);
            event["cache"] = json!(cache);
            event = Partition::tag(partition, event);
        }
        let _ = self.output.emit(event).await;
    }

    pub async fn warning(&self, message: &str) {
        self.emit(json!({"type": "cache_warning", "message": message}))
            .await;
    }

    /// A warning printed at most once per run.
    pub async fn run_warning(&self, message: &str) {
        let first = self
            .session
            .warned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(message.to_owned());
        if first {
            self.warning(message).await;
        }
    }

    /// `cache_remote_unavailable`; callers make sure it is printed once.
    pub async fn remote_unavailable(&self, reason: &str) {
        let _ = self
            .output
            .emit(json!({"type": "cache_remote_unavailable", "reason": reason}))
            .await;
    }
}

/// Run-wide cache state: where the local store is, the remote client, and
/// the warnings already printed.
pub struct Session {
    store_dir: Result<PathBuf, String>,
    remote: Option<remote::Remote>,
    warned: Mutex<HashSet<String>>,
}

pub struct Start<'a> {
    pub cache: Option<&'a CacheConfig>,
    pub reporting: Option<&'a Reporting>,
    pub workdir: &'a Path,
    pub config_dir: Option<&'a Path>,
    /// `GOODFLOWS_SPRITE_STATE_DIR` on a remote Sprite run.
    pub sprite_state: Option<&'a str>,
    /// The controller's minted cache key on a remote Sprite run.
    pub forwarded: Option<&'a Forwarded>,
}

/// A scope the server accepts: 1-256 UTF-8 bytes, no control characters.
pub fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= crate::config::MAX_CACHE_SCOPE_BYTES
        && !scope.chars().any(char::is_control)
}

/// The scope `[cache] scope` yields from the process environment: `Ok(None)`
/// when it is unset or yields null or an empty string, and `Err` with a
/// warning when it fails or yields something the server would refuse.
pub fn configured_scope(cache: Option<&CacheConfig>) -> Result<Option<String>, String> {
    let Some(source) = cache.and_then(|c| c.scope.as_deref()) else {
        return Ok(None);
    };
    let value = Expression::parse(source)
        .and_then(|expression| {
            expression.evaluate(&|reference| match reference {
                Reference::Env(name) => Ok(std::env::var(name)
                    .map(Value::String)
                    .unwrap_or(Value::Null)),
                _ => Err(anyhow!("[cache] scope may read only env.*")),
            })
        })
        .map_err(|error| {
            format!("[cache] scope failed to evaluate ({error:#}); using the Git branch")
        })?;
    match value {
        Value::Null => Ok(None),
        Value::String(scope) if scope.is_empty() => Ok(None),
        Value::String(scope) if valid_scope(&scope) => Ok(Some(scope)),
        Value::String(_) => Err(
            "[cache] scope is over 256 bytes or contains control characters; using the Git branch"
                .into(),
        ),
        _ => Err("[cache] scope must yield a string; using the Git branch".into()),
    }
}

/// The run's scope: `[cache] scope`, else `branch`. Returns a warning for
/// an invalid configured scope.
pub fn resolve_scope(
    cache: Option<&CacheConfig>,
    branch: Option<String>,
) -> (Option<String>, Option<String>) {
    let branch = branch.filter(|b| valid_scope(b));
    match configured_scope(cache) {
        Ok(Some(scope)) => (Some(scope), None),
        Ok(None) => (branch, None),
        Err(warning) => (branch, Some(warning)),
    }
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `$XDG_CACHE_HOME`, else the platform's user cache directory.
fn user_cache_dir() -> Option<PathBuf> {
    if let Some(dir) = read_env("XDG_CACHE_HOME").map(PathBuf::from) {
        if dir.is_absolute() {
            return Some(dir);
        }
    }
    let home = PathBuf::from(read_env("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches")
    } else {
        home.join(".cache")
    })
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A remote URL reduced to `host/path`: no scheme, credentials, trailing
/// slash or `.git`, with a lowercase host, so `git@github.com:a/b.git` and
/// `https://github.com/a/b` are the same project.
pub fn normalize_remote(url: &str) -> String {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((authority, path)) => (authority, path),
            None => (rest, ""),
        },
        None => match url.split_once(':') {
            Some((authority, path)) if !authority.contains('/') => (authority, path),
            _ => ("", url),
        },
    };
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let mut path = path.trim_matches('/');
    path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    if host.is_empty() {
        path.to_owned()
    } else {
        format!("{host}/{path}")
    }
}

/// What identifies a project's local store: its normalized Git remote URL,
/// else its canonical repository (common Git directory) path, else the
/// config file's directory.
fn project_identity(workdir: &Path, config_dir: Option<&Path>) -> String {
    let remotes = git(workdir, &["remote"]).unwrap_or_default();
    let remote = remotes
        .lines()
        .find(|r| *r == "origin")
        .or_else(|| remotes.lines().next());
    if let Some(url) = remote.and_then(|r| git(workdir, &["remote", "get-url", r])) {
        return format!("remote:{}", normalize_remote(&url));
    }
    if let Some(common) = git(workdir, &["rev-parse", "--git-common-dir"]) {
        let path = workdir.join(common);
        let path = path.canonicalize().unwrap_or(path);
        return format!("repository:{}", path.display());
    }
    let dir = config_dir.unwrap_or(workdir);
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    format!("directory:{}", dir.display())
}

fn store_dir(start: &Start) -> Result<PathBuf, String> {
    if let Some(state) = start.sprite_state {
        return Ok(Path::new(state).join("caches"));
    }
    let base = user_cache_dir().ok_or("neither XDG_CACHE_HOME nor HOME is set")?;
    let identity = project_identity(start.workdir, start.config_dir);
    let digest = key::hex(<sha2::Sha256 as sha2::Digest>::digest(identity.as_bytes()));
    Ok(base.join("goodflows").join(&digest[..32]))
}

impl Session {
    /// Start the run's cache session; events go to `output`.
    pub async fn start(start: Start<'_>, output: &Output) -> Arc<Self> {
        let mut session = Self {
            store_dir: store_dir(&start),
            remote: None,
            warned: Mutex::new(HashSet::new()),
        };
        let cache = start.cache;
        let events = Events {
            output,
            session: &session,
            job: None,
        };
        let remote = match (cache, start.reporting) {
            (Some(cache), Some(reporting)) if cache.remote_enabled() => {
                if start.sprite_state.is_some() {
                    // Only the controller's minted key; a missing one was
                    // already reported by the controller.
                    start
                        .forwarded
                        .and_then(|f| remote_from_controller(f, reporting))
                } else {
                    local_remote(cache, reporting, start.workdir, &events).await
                }
            }
            _ => None,
        };
        session.remote = remote;
        Arc::new(session)
    }
}

fn remote_from_controller(forwarded: &Forwarded, reporting: &Reporting) -> Option<remote::Remote> {
    match crate::reporting::publish::resolve(
        Some(&forwarded.url),
        Some(&forwarded.token),
        &reporting.project,
    ) {
        crate::reporting::publish::Credentials::Ready(target) => {
            remote::Remote::new(target, forwarded.scope.clone()).ok()
        }
        _ => None,
    }
}

async fn local_remote(
    cache: &CacheConfig,
    reporting: &Reporting,
    workdir: &Path,
    events: &Events<'_>,
) -> Option<remote::Remote> {
    let url = read_env(&reporting.url_env);
    let key = read_env(&cache.token_env).or_else(|| read_env(&reporting.token_env));
    if key.is_some() {
        crate::reporting::protect_process();
    }
    use crate::reporting::publish::{resolve, Credentials};
    let target = match resolve(url.as_deref(), key.as_deref(), &reporting.project) {
        Credentials::Ready(target) => target,
        other => {
            let reason = other.unavailable().unwrap_or("credentials_unavailable");
            events.remote_unavailable(reason).await;
            return None;
        }
    };
    if key.as_deref().is_some_and(|k| k.starts_with("pk_")) {
        events.remote_unavailable("public_key").await;
        return None;
    }
    let branch = crate::reporting::Origin::Checkout(workdir)
        .source(Some(&reporting.token_env))
        .await
        .and_then(|s| s.branch);
    let (scope, warning) = resolve_scope(Some(cache), branch);
    if let Some(warning) = warning {
        events.warning(&warning).await;
    }
    match remote::Remote::new(target, scope) {
        Ok(remote) => Some(remote),
        Err(_) => {
            events.remote_unavailable("client_error").await;
            None
        }
    }
}

/// One invocation's cache settings over the run's session.
pub struct Invocation {
    session: Arc<Session>,
    settings: EffectiveCache,
    store: Option<Result<local::Store, String>>,
}

/// A cache to restore, with its evaluated `key.values` and `key.unique`.
pub struct Request {
    pub cache: Cache,
    pub values: Result<(Vec<Value>, Vec<Value>), String>,
}

struct Prepared {
    cache: Cache,
    /// `(partial, key)`, or `None` when the key could not be computed.
    keys: Option<(String, String)>,
    hit: bool,
}

/// The caches of one job after restore, waiting for the job's final status.
pub struct Pending {
    invocation: Arc<Invocation>,
    output: Output,
    job: String,
    partition: Option<Partition>,
    workdir: PathBuf,
    home: Option<PathBuf>,
    prepared: Vec<Prepared>,
    pub reports: Vec<Report>,
    cancel: Cancel,
}

/// Where and until when one cache restores.
struct Place<'a> {
    workdir: &'a Path,
    home: Option<&'a Path>,
    deadline: tokio::time::Instant,
    cancel: &'a Cancel,
}

fn stamp_path(workdir: &Path, name: &str) -> PathBuf {
    workdir
        .join(".goodflows/cache-stamps")
        .join(crate::config::path_component(name))
}

fn read_stamp(workdir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(stamp_path(workdir, name)).ok()?;
    Some(text.trim().to_owned())
}

fn write_stamp(workdir: &Path, name: &str, key: &str) -> Result<()> {
    let path = stamp_path(workdir, name);
    crate::fsutil::create_state_dir(path.parent().context("stamp directory")?)?;
    crate::fsutil::write_atomic(&path, format!("{key}\n").as_bytes(), true)
}

/// Whether a declared path exists: a `~/` directory, or at least one match
/// of a workdir glob.
fn path_exists(workdir: &Path, home: Option<&Path>, path: &str) -> bool {
    match path.strip_prefix("~/") {
        Some(rest) => home.is_some_and(|home| std::fs::symlink_metadata(home.join(rest)).is_ok()),
        None => !glob::expand(workdir, path).is_empty(),
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / 1_048_576.0)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| anyhow!("a cache task panicked"))?
}

/// Run `work` until `deadline` or cancellation. When either comes first,
/// set `abort`, let the work stop at its next check (so it cleans up what
/// it staged), and fail.
async fn bounded<T>(
    deadline: tokio::time::Instant,
    timeout_secs: u64,
    abort: &AtomicBool,
    cancel: &Cancel,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(work);
    let stop = tokio::select! {
        result = tokio::time::timeout_at(deadline, &mut work) => match result {
            Ok(result) => return result,
            Err(_) => format!("abandoned after the cache's timeout_secs ({timeout_secs}s)"),
        },
        _ = cancelled(cancel) => CANCELLED.to_owned(),
    };
    abort.store(true, Ordering::Relaxed);
    // Work that finished anyway (it passed its last abort check, as a
    // restore that is already swapping its tree into place) stands.
    work.await.map_err(|_| anyhow!(stop))
}

impl Invocation {
    pub fn new(session: Arc<Session>, settings: EffectiveCache) -> Self {
        let store = settings.local.then(|| {
            session
                .store_dir
                .clone()
                .and_then(|dir| local::Store::open(&dir).map_err(|e| format!("{e:#}")))
        });
        Self {
            session,
            settings,
            store,
        }
    }

    fn store(&self) -> Option<&local::Store> {
        self.store.as_ref()?.as_ref().ok()
    }

    fn events<'a>(
        &'a self,
        output: &'a Output,
        job: &'a str,
        partition: Option<Partition>,
        cache: &'a str,
    ) -> Events<'a> {
        Events {
            output,
            session: &self.session,
            job: Some((job, partition, cache)),
        }
    }

    /// Compute each cache's key and restore it. Called after the job's
    /// condition passed and before its command or first step.
    #[allow(clippy::too_many_arguments)]
    pub async fn restore(
        self: &Arc<Self>,
        output: &Output,
        job: &str,
        partition: Option<Partition>,
        workdir: &Path,
        home: Option<PathBuf>,
        requests: Vec<Request>,
        cancel: &Cancel,
    ) -> Pending {
        let mut prepared = Vec::new();
        let mut reports = Vec::new();
        for request in requests {
            let cache = request.cache;
            let events = self.events(output, job, partition, &cache.name);
            let started = Instant::now();
            // One bound covers hashing, the present check and the fetch.
            let deadline = tokio::time::Instant::now() + cache.timeout();
            if let Some(Err(message)) = &self.store {
                events
                    .run_warning(&format!("the local cache store is unavailable: {message}"))
                    .await;
            }
            let mut keys = match request.values {
                Ok((values, unique)) => {
                    let (dir, files) = (workdir.to_path_buf(), cache.key.files.clone());
                    let digest = detached(deadline, cancel, move |abort| {
                        Ok(key::files_digest(&dir, &files, abort)?)
                    });
                    match digest.await {
                        Ok(files) => {
                            let partial = key::partial(
                                &cache.name,
                                std::env::consts::OS,
                                std::env::consts::ARCH,
                                &values,
                            );
                            let key = key::key(&partial, &files, &unique);
                            Some((partial, key))
                        }
                        Err(error) => {
                            events
                                .warning(&format!("hashing key.files failed: {error:#}"))
                                .await;
                            None
                        }
                    }
                }
                Err(message) => {
                    events
                        .warning(&format!(
                            "a key value failed, so the cache misses: {message}"
                        ))
                        .await;
                    None
                }
            };
            if keys.is_some() && home.is_none() && cache.paths.iter().any(|p| p.starts_with("~/")) {
                events
                    .warning("HOME is not set, so ~/ paths cannot be restored or saved")
                    .await;
                keys = None;
            }
            let (result, tier, bytes, hit) = match &keys {
                None => ("miss", None, 0, false),
                Some((partial, key)) => {
                    let place = Place {
                        workdir,
                        home: home.as_deref(),
                        deadline,
                        cancel,
                    };
                    self.restore_one(&events, &place, &cache, partial, key)
                        .await
                }
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            events
                .emit(
                    json!({"type": "cache_restored", "result": result, "tier": tier,
                    "bytes": bytes, "duration_ms": duration_ms}),
                )
                .await;
            reports.push(Report {
                name: cache.name.clone(),
                result,
                tier,
                saved: None,
                skipped: None,
            });
            prepared.push(Prepared { cache, keys, hit });
        }
        Pending {
            invocation: self.clone(),
            output: output.clone(),
            job: job.to_owned(),
            partition,
            workdir: workdir.to_path_buf(),
            home,
            prepared,
            reports,
            cancel: cancel.clone(),
        }
    }

    /// `(result, tier, bytes, hit)` for one cache.
    async fn restore_one(
        &self,
        events: &Events<'_>,
        place: &Place<'_>,
        cache: &Cache,
        partial: &str,
        key: &str,
    ) -> (&'static str, Option<&'static str>, u64, bool) {
        let (workdir, home) = (place.workdir, place.home);
        {
            // A killed restore may have left the old tree aside, whether or
            // not it put the new one in place; nothing else removes it.
            // Awaited, so no cleanup runs on while the job does.
            let (dir, home, paths) = (
                workdir.to_path_buf(),
                home.map(Path::to_path_buf),
                cache.paths.clone(),
            );
            let _ = blocking(move || {
                archive::prune_asides(&archive::Declared {
                    workdir: &dir,
                    home: home.as_deref(),
                    paths: &paths,
                });
                Ok(())
            })
            .await;
        }
        let present = {
            let (dir, home, paths) = (
                workdir.to_path_buf(),
                home.map(Path::to_path_buf),
                cache.paths.clone(),
            );
            detached(place.deadline, place.cancel, move |_| {
                Ok(paths
                    .iter()
                    .all(|path| path_exists(&dir, home.as_deref(), path)))
            })
            .await
        };
        let present = match present {
            Ok(present) => present,
            Err(error) => {
                events
                    .warning(&format!("restore failed, so the cache misses: {error:#}"))
                    .await;
                return ("miss", None, 0, false);
            }
        };
        if present {
            return match read_stamp(workdir, &cache.name) {
                Some(stamp) if stamp == key => ("exact", Some("workdir"), 0, true),
                _ => ("present", Some("workdir"), 0, false),
            };
        }
        let abort = Arc::new(AtomicBool::new(false));
        let fetched = bounded(
            place.deadline,
            cache.timeout().as_secs(),
            &abort,
            place.cancel,
            self.fetch(events, workdir, home, cache, partial, key, &abort),
        )
        .await;
        match fetched {
            Ok(Some((found, tier, bytes))) => {
                if let Err(error) = write_stamp(workdir, &cache.name, &found) {
                    events
                        .warning(&format!("writing the key stamp failed: {error:#}"))
                        .await;
                }
                let exact = found == key;
                (
                    if exact { "exact" } else { "fallback" },
                    Some(tier),
                    bytes,
                    exact,
                )
            }
            Ok(None) => ("miss", None, 0, false),
            Err(error) => {
                events
                    .warning(&format!("restore failed, so the cache misses: {error:#}"))
                    .await;
                ("miss", None, 0, false)
            }
        }
    }

    /// Restore in the order exact local, exact remote, fallback local,
    /// fallback remote. Returns the restored entry's key, its tier and size.
    #[allow(clippy::too_many_arguments)]
    async fn fetch(
        &self,
        events: &Events<'_>,
        workdir: &Path,
        home: Option<&Path>,
        cache: &Cache,
        partial: &str,
        key: &str,
        abort: &Arc<AtomicBool>,
    ) -> Result<Option<(String, &'static str, u64)>> {
        let extract = |archive: PathBuf, size: u64, sha256: String| {
            let (workdir, home, paths, abort) = (
                workdir.to_path_buf(),
                home.map(Path::to_path_buf),
                cache.paths.clone(),
                abort.clone(),
            );
            blocking(move || {
                archive::extract(
                    &archive,
                    size,
                    &sha256,
                    &archive::Declared {
                        workdir: &workdir,
                        home: home.as_deref(),
                        paths: &paths,
                    },
                    &abort,
                )
            })
        };
        for (exact, local) in [(true, true), (true, false), (false, true), (false, false)] {
            if !exact && !cache.fallback {
                break;
            }
            if local {
                let Some(store) = self.store() else { continue };
                let found = if exact {
                    store.read(key).map(|r| (key.to_owned(), r))
                } else {
                    store
                        .newest(partial)
                        .and_then(|(found, _)| store.read(&found).map(|r| (found, r)))
                };
                let Some((found, reading)) = found else {
                    continue;
                };
                let (size, sha256) = (reading.meta.size, reading.meta.sha256.clone());
                let path = reading.path.clone();
                let result = extract(path, size, sha256).await;
                drop(reading);
                if let Err(error) = result {
                    if !error.is::<archive::Aborted>() {
                        store.remove(&found);
                    }
                    return Err(error.context("the local entry is unusable and was removed"));
                }
                return Ok(Some((found, "local", size)));
            }
            let Some(remote) = &self.session.remote else {
                continue;
            };
            let entry = match exact {
                true => remote.lookup(Some(key), None, events).await,
                false => remote.lookup(None, Some(partial), events).await,
            };
            let Some(entry) = entry else { continue };
            let temp = match self.store() {
                Some(store) => store.temp_path(),
                None => {
                    let dir = workdir.join(".goodflows/cache-restore");
                    crate::fsutil::create_state_dir(&dir)?;
                    dir.join(format!(".tmp{}", uuid::Uuid::new_v4().simple()))
                }
            };
            let result = async {
                remote.download(&entry, &temp, abort).await?;
                extract(temp.clone(), entry.size, entry.sha256.clone()).await?;
                if let Some(store) = self.store() {
                    let now = local::now_ms();
                    let meta = local::Meta {
                        name: cache.name.clone(),
                        partial: entry.partial.clone(),
                        sha256: entry.sha256.clone(),
                        size: entry.size,
                        created_at_ms: now,
                        accessed_at_ms: now,
                    };
                    if let Err(error) = store.put(&entry.key, &temp, &meta) {
                        events
                            .warning(&format!("keeping the downloaded entry failed: {error:#}"))
                            .await;
                    }
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            let _ = std::fs::remove_file(&temp);
            result?;
            if let Some(store) = self.store() {
                let (store_dir, bytes, keep) = (
                    store.dir().to_path_buf(),
                    self.settings.local_bytes,
                    entry.key.clone(),
                );
                let _ = blocking(move || local::Store::open(&store_dir)?.prune(bytes, &keep)).await;
            }
            return Ok(Some((entry.key, "remote", entry.size)));
        }
        Ok(None)
    }
}

impl Pending {
    /// Whether each cache was an exact hit, for `caches.NAME.hit`.
    pub fn hits(&self) -> HashMap<String, bool> {
        self.prepared
            .iter()
            .map(|p| (p.cache.name.clone(), p.hit))
            .collect()
    }

    /// Save each cache after the job ended with `status`, and return the
    /// final reports. Never fails. Saves run in `slot`, the job's slot.
    /// With `activity`, each archive is taken while no unit of the run
    /// executes, and taken again when one started meanwhile. While a save
    /// waits for that, it frees its slot, so other jobs can start, and takes
    /// one again to archive. The waits do not count against the bound; the
    /// archive attempts share it.
    pub async fn save(
        mut self,
        status: Status,
        activity: Option<Arc<dyn Activity>>,
        mut slot: Option<Slot>,
    ) -> Vec<Report> {
        let invocation = self.invocation.clone();
        for (index, prepared) in self.prepared.iter().enumerate() {
            let events = invocation.events(
                &self.output,
                &self.job,
                self.partition,
                &prepared.cache.name,
            );
            let accepted = match status {
                Status::Passed => true,
                Status::Failed => prepared.cache.saves_failed(),
                _ => false,
            };
            if accepted {
                if let Some((_, key)) = &prepared.keys {
                    if let Err(error) = write_stamp(&self.workdir, &prepared.cache.name, key) {
                        events
                            .warning(&format!("writing the key stamp failed: {error:#}"))
                            .await;
                    }
                }
            }
            let skip = if prepared.keys.is_none() {
                Some("error")
            } else if prepared.hit {
                Some("hit")
            } else if !accepted {
                Some("not_passed")
            } else if !invocation.settings.write {
                Some("read_only")
            } else {
                None
            };
            let report = &mut self.reports[index];
            if let Some(reason) = skip {
                events
                    .emit(json!({"type": "cache_save_skipped", "reason": reason}))
                    .await;
                report.skipped = Some(reason);
                continue;
            }
            let (partial, key) = prepared.keys.clone().expect("checked above");
            let started = Instant::now();
            let mut budget = prepared.cache.timeout();
            let saved = loop {
                let quiet_at = match &activity {
                    Some(activity) => {
                        if activity.executing() != 0 {
                            // Waiting is not saving: free the slot meanwhile.
                            slot = None;
                        }
                        let Some(entered) = quiet(activity.as_ref(), &self.cancel).await else {
                            break Err(anyhow!(CANCELLED));
                        };
                        if slot.is_none() {
                            slot = tokio::select! {
                                taken = activity.slot() => Some(taken),
                                _ = cancelled(&self.cancel) => break Err(anyhow!(CANCELLED)),
                            };
                            if activity.executing() != 0 || activity.entered() != entered {
                                continue;
                            }
                        }
                        Some((activity.as_ref(), entered))
                    }
                    None => None,
                };
                if budget.is_zero() {
                    break Err(anyhow!(
                        "abandoned after the cache's timeout_secs ({}s)",
                        prepared.cache.timeout().as_secs()
                    ));
                }
                let attempt_started = tokio::time::Instant::now();
                let abort = Arc::new(AtomicBool::new(false));
                let attempt = bounded(
                    attempt_started + budget,
                    prepared.cache.timeout().as_secs(),
                    &abort,
                    &self.cancel,
                    invocation.save_one(
                        &events,
                        &self.workdir,
                        self.home.as_deref(),
                        &prepared.cache,
                        &partial,
                        &key,
                        &abort,
                        quiet_at,
                        &self.cancel,
                    ),
                )
                .await;
                match attempt {
                    Err(error) if error.is::<Busy>() => {
                        budget = budget.saturating_sub(attempt_started.elapsed());
                        continue;
                    }
                    other => break other,
                }
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            match saved {
                Ok(SaveOutcome {
                    tiers,
                    bytes,
                    skipped,
                }) => {
                    if !tiers.is_empty() {
                        events
                            .emit(json!({"type": "cache_saved", "tiers": tiers, "bytes": bytes, "duration_ms": duration_ms}))
                            .await;
                        report.saved = Some(tiers.clone());
                    }
                    if let Some((reason, tier)) = skipped {
                        let mut event = json!({"type": "cache_save_skipped", "reason": reason});
                        if let Some(tier) = tier {
                            event["tier"] = json!(tier);
                        }
                        events.emit(event).await;
                        report.skipped = Some(reason);
                    }
                }
                Err(error) => {
                    events
                        .warning(&format!("saving the cache failed: {error:#}"))
                        .await;
                    events
                        .emit(json!({"type": "cache_save_skipped", "reason": "error"}))
                        .await;
                    report.skipped = Some("error");
                }
            }
        }
        self.reports
    }
}

struct SaveOutcome {
    tiers: Vec<&'static str>,
    bytes: u64,
    /// A skip reason and, when another tier saved, the tier it applies to.
    skipped: Option<(&'static str, Option<&'static str>)>,
}

impl Invocation {
    #[allow(clippy::too_many_arguments)]
    async fn save_one(
        &self,
        events: &Events<'_>,
        workdir: &Path,
        home: Option<&Path>,
        cache: &Cache,
        partial: &str,
        key: &str,
        abort: &Arc<AtomicBool>,
        quiet_at: Option<(&dyn Activity, u64)>,
        cancel: &Cancel,
    ) -> Result<SaveOutcome> {
        let skipped = |reason| SaveOutcome {
            tiers: Vec::new(),
            bytes: 0,
            skipped: Some((reason, None)),
        };
        if self.store().is_some_and(|store| store.get(key).is_some()) {
            return Ok(skipped("exists"));
        }
        if let Some(remote) = &self.session.remote {
            if remote.lookup(Some(key), None, events).await.is_some() {
                return Ok(skipped("exists"));
            }
        }
        // `local = false` with no usable remote store: say why, once.
        if self.store.is_none() && self.session.remote.as_ref().is_none_or(|r| r.disabled()) {
            events
                .run_warning(
                    "caches are not saved: the local store is off (local = false) and no remote store is available",
                )
                .await;
            return Ok(skipped("error"));
        }
        // Only the remote store would take it, and it would refuse: skip
        // building an archive that would be thrown away.
        if self.store.is_none() {
            if let Some(reason) = self.session.remote.as_ref().and_then(|r| r.refusal()) {
                return Ok(skipped(reason));
            }
        }
        let temp = match self.store() {
            Some(store) => store.temp_path(),
            None => {
                let dir = workdir.join(".goodflows/cache-save");
                crate::fsutil::create_state_dir(&dir)?;
                dir.join(format!(".tmp{}", uuid::Uuid::new_v4().simple()))
            }
        };
        let written = {
            let (temp, workdir, home, paths, key, partial, abort) = (
                temp.clone(),
                workdir.to_path_buf(),
                home.map(Path::to_path_buf),
                cache.paths.clone(),
                key.to_owned(),
                partial.to_owned(),
                abort.clone(),
            );
            blocking(move || {
                archive::write(
                    &temp,
                    &archive::Declared {
                        workdir: &workdir,
                        home: home.as_deref(),
                        paths: &paths,
                    },
                    &key,
                    &partial,
                    archive::MAX_ENTRY_BYTES,
                    &abort,
                )
            })
            .await
        };
        // A unit that ran while the tree was read may have written it.
        if let Some((activity, entered)) = quiet_at {
            if activity.executing() != 0 || activity.entered() != entered {
                let _ = std::fs::remove_file(&temp);
                return Err(Busy.into());
            }
        }
        let written = match written {
            Ok(written) => written,
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                if error.is::<archive::TooLarge>() {
                    return Ok(skipped("too_large"));
                }
                return Err(error);
            }
        };
        let mut outcome = SaveOutcome {
            tiers: Vec::new(),
            bytes: written.bytes,
            skipped: None,
        };
        let result = async {
            if let Some(store) = self.store() {
                let now = local::now_ms();
                let meta = local::Meta {
                    name: cache.name.clone(),
                    partial: partial.to_owned(),
                    sha256: written.sha256.clone(),
                    size: written.bytes,
                    created_at_ms: now,
                    accessed_at_ms: now,
                };
                // A concurrent writer of the same key counts as saved.
                store.put(key, &temp, &meta)?;
                outcome.tiers.push("local");
            }
            if let Some(remote) = &self.session.remote {
                match remote
                    .save(&cache.name, partial, key, &temp, &written, events, abort)
                    .await
                {
                    remote::Saved::Stored => outcome.tiers.push("remote"),
                    remote::Saved::Skipped(reason) => {
                        outcome.skipped = Some((reason, Some("remote")))
                    }
                    remote::Saved::Failed(message) => events.warning(&message).await,
                    remote::Saved::Unavailable => {}
                }
            }
            // An upload abandoned at the bound: the save reached its bound.
            // A local entry already written stands; only the remote tier
            // is skipped.
            if archive::check_abort(abort).is_err() {
                if outcome.tiers.is_empty() {
                    return Err(archive::Aborted.into());
                }
                let cause = if cancel.borrow().is_some() {
                    "the run was cancelled".to_owned()
                } else {
                    format!("the cache's timeout_secs ({}s)", cache.timeout().as_secs())
                };
                events
                    .warning(&format!("the remote upload was abandoned: {cause}"))
                    .await;
                outcome.skipped = Some(("error", Some("remote")));
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let _ = std::fs::remove_file(&temp);
        result?;
        if outcome.tiers.is_empty() && outcome.skipped.is_none() {
            outcome.skipped = Some(("error", None));
        }
        if outcome.tiers.is_empty() {
            if let Some((_, tier)) = &mut outcome.skipped {
                *tier = None;
            }
        }
        if let Some(store) = self.store() {
            let (dir, bytes, keep) = (
                store.dir().to_path_buf(),
                self.settings.local_bytes,
                key.to_owned(),
            );
            if let Err(error) =
                blocking(move || local::Store::open(&dir)?.prune(bytes, &keep)).await
            {
                events
                    .warning(&format!("pruning the local cache store failed: {error:#}"))
                    .await;
            }
        }
        Ok(outcome)
    }
}

/// The text line for a cache event, or `None` for other events.
pub fn text_line(event: &Value, label: &str) -> Option<String> {
    let cache = crate::output::visible(event["cache"].as_str().unwrap_or_default()).into_owned();
    let secs = |e: &Value| e["duration_ms"].as_u64().unwrap_or_default() as f64 / 1000.0;
    Some(match event["type"].as_str()? {
        "cache_restored" => match event["result"].as_str().unwrap_or_default() {
            "miss" => format!("{label} cache {cache}: miss"),
            "present" => format!("{label} cache {cache}: present (kept the existing tree)"),
            result => format!(
                "{label} cache {cache}: restored ({result}, {}, {}, {:.1}s)",
                event["tier"].as_str().unwrap_or("none"),
                mib(event["bytes"].as_u64().unwrap_or_default()),
                secs(event)
            ),
        },
        "cache_saved" => format!(
            "{label} cache {cache}: saved ({}, {})",
            event["tiers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            mib(event["bytes"].as_u64().unwrap_or_default())
        ),
        "cache_save_skipped" => format!(
            "{label} cache {cache}: not saved ({}{})",
            event["reason"].as_str().unwrap_or_default(),
            event["tier"]
                .as_str()
                .map(|t| format!(", {t}"))
                .unwrap_or_default()
        ),
        "cache_warning" => {
            let message =
                crate::output::visible(event["message"].as_str().unwrap_or_default()).into_owned();
            if event.get("cache").is_some() {
                format!("{label} cache {cache}: warning: {message}")
            } else {
                format!("cache warning: {message}")
            }
        }
        "cache_remote_unavailable" => format!(
            "cache: remote store unavailable ({}); using the local store",
            event["reason"].as_str().unwrap_or_default()
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_keeps_work_that_finished_past_its_bound() {
        let (_sender, cancel) = watch::channel(None);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(20);
        // Past its last abort check (a restore swapping in its tree).
        let abort = AtomicBool::new(false);
        let finished = bounded(deadline, 1, &abort, &cancel, async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(7)
        })
        .await;
        assert_eq!(finished.unwrap(), 7);
        assert!(abort.load(Ordering::Relaxed));
        // Work that stops at the abort reports the bound.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(20);
        let abort = AtomicBool::new(false);
        let stopped = bounded(deadline, 1, &abort, &cancel, async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            archive::check_abort(&abort).map(|_| 7)
        })
        .await;
        let error = format!("{:#}", stopped.unwrap_err());
        assert!(error.contains("timeout_secs (1s)"), "{error}");
    }

    #[test]
    fn remote_urls_normalize_to_host_and_path() {
        for url in [
            "git@github.com:agoodway/goodflows.git",
            "https://github.com/agoodway/goodflows",
            "https://user:token@GitHub.com/agoodway/goodflows.git/",
            "ssh://git@github.com/agoodway/goodflows.git",
        ] {
            assert_eq!(
                normalize_remote(url),
                "github.com/agoodway/goodflows",
                "{url}"
            );
        }
        assert_eq!(normalize_remote("/srv/repo.git"), "srv/repo");
    }

    #[test]
    fn scopes_follow_the_server_rule() {
        assert!(valid_scope("feature/login"));
        assert!(valid_scope(&"x".repeat(256)));
        assert!(!valid_scope(&"x".repeat(257)));
        assert!(!valid_scope(""));
        assert!(!valid_scope("main\n"));
    }
}
