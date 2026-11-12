//! `goodflows run --sprite <identity>` and `goodflows sprite ...`: run a
//! complete workflow on a caller identity's owned, retained Sprite.
//!
//! The controller validates locally, snapshots the working tree, provisions
//! a matching runner, and submits one archive. A Sprite-managed supervisor
//! owns the accepted run (see `agent`); the controller only follows, collects
//! and publishes. See docs/sprites.md.

pub mod agent;
pub mod collect;
pub mod control;
pub mod hold;
pub mod naming;
pub mod snapshot;
mod testhook;
pub mod transport;

use crate::config::{self, Config, SpriteConfig};
use crate::output::Output;
use anyhow::{bail, Context, Result};
use control::{Header, Phase, Provenance, Record, Request, PROTOCOL};
use naming::{Owner, Scope};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio::sync::watch;
use transport::Sprite;

/// Caller credentials that never leave the controller.
pub const CONTROLLER_CREDENTIALS: [&str; 2] = ["SPRITE_TOKEN", "SPRITES_TOKEN"];
/// Variables the remote runner owns.
pub const RUNNER_OWNED: [&str; 7] = [
    "GOODFLOWS_RUN_ID",
    "GOODFLOWS_REPORT_DIR",
    "GOODFLOWS_PARTITION",
    "GOODFLOWS_PARTITION_COUNT",
    config::ENVIRONMENT_ENV,
    config::OUTPUT_ENV,
    config::SPRITE_STATE_ENV,
];
pub(crate) const UPLOAD_TIMEOUT_SECS: u64 = 600;
/// Reconnect backoff for an attached run's follow stream.
const FOLLOW_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_secs(1);
const FOLLOW_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(15);
/// A follow connection that stays up this long (or the whole reconnect
/// budget, if shorter) counts as working even without new output.
const FOLLOW_ESTABLISHED: std::time::Duration = std::time::Duration::from_secs(30);
/// Forwarded values shorter than this are not searched for in source files
/// (they would match ordinary content); credential values always are.
const MIN_SENTINEL_BYTES: usize = 8;

fn parse_run(value: &str) -> std::result::Result<String, String> {
    if crate::fsutil::canonical_uuid(value) {
        Ok(value.to_owned())
    } else {
        Err("run must be a lowercase canonical UUID".into())
    }
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Show ownership, preparation and runs; records confirmed process loss
    Status {
        #[arg(value_parser = naming::parse_identity)]
        identity: String,
    },
    /// Print a run's saved lifecycle and workflow events (reads only)
    Logs {
        #[arg(value_parser = naming::parse_identity)]
        identity: String,
        #[arg(long, value_name = "UUID", value_parser = parse_run)]
        run: String,
    },
    /// Download a run's saved results into a verified local bundle (never publishes)
    Collect {
        #[arg(value_parser = naming::parse_identity)]
        identity: String,
        #[arg(long, value_name = "UUID", value_parser = parse_run)]
        run: String,
        /// Bundle base directory (default .goodflows/sprites/<scope>/<run>)
        #[arg(long, value_name = "DIRECTORY")]
        output: Option<PathBuf>,
    },
    /// Request cancellation of exactly this run (hooks or workflow)
    Cancel {
        #[arg(value_parser = naming::parse_identity)]
        identity: String,
        #[arg(long, value_name = "UUID", value_parser = parse_run)]
        run: String,
    },
    /// Permanently destroy an owned, idle Sprite with its caches and results
    ///
    /// Refuses while a run is active or unresolved. Asks for confirmation on a
    /// terminal; automation must pass --yes. Dependency caches, state and
    /// uncollected results are lost permanently. Remote runs are otherwise never
    /// pruned, so retained Sprites keep using storage. A reused Sprite is one
    /// trust domain: fresh per-run source directories do not isolate hostile jobs.
    Destroy {
        #[arg(value_parser = naming::parse_identity)]
        identity: String,
        /// Skip the interactive confirmation (required without a terminal)
        #[arg(long)]
        yes: bool,
    },
}

pub struct RunInput {
    pub document: Config,
    pub config_file: PathBuf,
    pub name: String,
    pub selected: Config,
    /// Every workflow the selection's calls reach, with its complete jobs
    /// under the selected profile.
    pub targets: BTreeMap<String, Config>,
    pub identity: String,
    pub request: Request,
    pub rerun_config: Option<String>,
}

// ---------------------------------------------------------------------------
// Local preflight

fn denied_reason(name: &str, token_env: Option<&str>) -> Option<&'static str> {
    if CONTROLLER_CREDENTIALS.contains(&name) {
        Some("Sprite credentials stay on the controller")
    } else if token_env == Some(name) {
        Some("the reporting token stays on the controller")
    } else if RUNNER_OWNED.contains(&name) {
        Some("the remote runner sets it itself")
    } else {
        None
    }
}

/// Remote-only rules for forwarding and every configured env layer,
/// including unselected workflows and profiles. Names only, never values.
pub fn check_remote_env(document: &Config, sprite: &SpriteConfig) -> Result<()> {
    let token = document.reporting.as_ref().map(|r| r.token_env.as_str());
    for name in &sprite.pass_env {
        if let Some(reason) = denied_reason(name, token) {
            bail!("[sprite] pass_env must not forward {name}: {reason}");
        }
    }
    let mut layers: Vec<(String, Vec<&String>)> =
        vec![("settings".into(), document.settings.env.keys().collect())];
    for (name, workflow) in &document.workflows {
        layers.push((
            format!("workflow '{name}' settings"),
            workflow.settings.env.keys().collect(),
        ));
    }
    for (name, environment) in &document.environments {
        layers.push((
            format!("environment '{name}' settings"),
            environment.settings.env.keys().collect(),
        ));
    }
    let jobs = document.jobs.iter().map(|j| ("default", j)).chain(
        document
            .workflows
            .iter()
            .flat_map(|(w, wf)| wf.jobs.iter().map(move |j| (w.as_str(), j))),
    );
    for (workflow, job) in jobs {
        layers.push((
            format!("workflow '{workflow}' job '{}'", job.name),
            job.env.keys().collect(),
        ));
        for step in &job.steps {
            layers.push((
                format!(
                    "workflow '{workflow}' job '{}' step '{}'",
                    job.name, step.id
                ),
                step.env.keys().collect(),
            ));
        }
    }
    for (scope, keys) in layers {
        for key in keys {
            if let Some(reason) = denied_reason(key, token) {
                bail!("{scope} env must not set {key} for Sprite runs: {reason}");
            }
        }
    }
    Ok(())
}

/// Unset fails (naming only the variable); empty counts as set.
fn resolve_pass_env(names: &[String]) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for name in names {
        match std::env::var_os(name) {
            Some(value) => {
                let value = value.into_string().map_err(|_| {
                    anyhow::anyhow!("[sprite] pass_env variable {name} is not valid UTF-8")
                })?;
                values.insert(name.clone(), value);
            }
            None => bail!("[sprite] pass_env variable {name} is not set"),
        }
    }
    Ok(values)
}

fn sprite_cli() -> Result<PathBuf> {
    transport::find_on_path("sprite").context(
        "the Sprite CLI (`sprite`) is not installed or not on PATH; see https://docs.sprites.dev/cli/installation",
    )
}

fn prefix_of(document: &Config) -> Result<String> {
    document
        .settings
        .sprite_prefix
        .clone()
        .context("Sprite operations require [settings] sprite_prefix in the config")
}

fn controller_secrets(document: &Config) -> Vec<String> {
    let mut names: Vec<&str> = CONTROLLER_CREDENTIALS.to_vec();
    if let Some(reporting) = &document.reporting {
        names.push(&reporting.token_env);
    }
    names
        .into_iter()
        .filter_map(|n| std::env::var(n).ok())
        .filter(|v| !v.is_empty())
        .collect()
}

/// `<base>/.goodflows/sprites/<scope-digest>/<run>`; `--output` replaces it.
fn bundle_base(scope: &Scope, run_id: &str, output: Option<&Path>) -> Result<PathBuf> {
    Ok(match output {
        Some(dir) => std::env::current_dir()?.join(dir),
        None => std::env::current_dir()?
            .join(".goodflows/sprites")
            .join(scope.digest())
            .join(run_id),
    })
}

async fn emit(output: &Output, mut event: Value, message: String) {
    event["message"] = json!(message);
    let _ = output.emit(event).await;
}

/// Unresolved execution or failed transport: exit 3 with a recovery reference.
async fn unresolved(
    output: &Output,
    identity: &str,
    run_id: Option<&str>,
    reason: String,
) -> Result<u8> {
    let recover = format!(
        "goodflows sprite status {}",
        crate::output::shell_word(identity)
    );
    emit(
        output,
        json!({"type": "sprite_unresolved", "identity": identity, "run_id": run_id, "reason": reason, "recover": recover}),
        match run_id {
            Some(id) => format!("Sprite run {id} is unresolved: {reason}\nRecover with: {recover}"),
            None => format!("Sprite operation failed: {reason}"),
        },
    )
    .await;
    Ok(3)
}

/// A definite refusal established before submission (ownership, runner
/// compatibility, release artifacts): a runtime error with exit 1, unlike an
/// unresolved transport failure (exit 3).
#[derive(Debug)]
struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

fn refusal(message: String) -> anyhow::Error {
    Refusal(message).into()
}

// ---------------------------------------------------------------------------
// Remote environment

/// Read the ownership record without any GoodFlows runner installed.
async fn read_owner(sprite: &Sprite) -> Result<Option<Owner>> {
    let captured = sprite
        .exec(&["sh", "-c", r#"cat "$HOME/.goodflows/owner.json""#])
        .await?;
    if !captured.success {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&captured.stdout).context("malformed ownership record")?,
    ))
}

fn agent_json(captured: &transport::Captured, sprite: &Sprite, what: &str) -> Result<Value> {
    if !captured.success {
        bail!("{what} failed: {}", sprite.diagnostic(captured));
    }
    let line = captured
        .stdout
        .split(|b| *b == b'\n')
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    serde_json::from_slice(line).with_context(|| format!("{what} returned unexpected output"))
}

/// Overall bound on one release download request (connect, headers, body).
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// This executable when it is a Linux build for the Sprite's architecture.
/// Returns the path and its digest.
async fn local_runner(arch: &str) -> Result<Option<(PathBuf, String)>> {
    let exe = std::env::current_exe()?;
    if cfg!(target_os = "linux") && std::env::consts::ARCH == arch {
        let bytes = tokio::fs::read(&exe).await?;
        return Ok(Some((exe, control::sha256_hex(&bytes))));
    }
    Ok(None)
}

/// A verified release runner on the controller: its path, digest, the
/// temporary directory that holds it when it could not be cached, and
/// whether it came from the cache.
type ReleaseRunner = (PathBuf, String, Option<tempfile::TempDir>, bool);

/// The exact version's checksummed release runner for `arch`, from the
/// controller cache (`.goodflows/sprites/runners/v<version>-<arch>/`, keyed by
/// version and architecture) when an intact copy is there, otherwise
/// downloaded, verified and cached.
async fn release_runner(arch: &str, git_root: &Path) -> Result<ReleaseRunner> {
    let version = env!("CARGO_PKG_VERSION");
    // One cache per project: the Git root of the selected config, whatever
    // directory the run starts from (snapshots never include `.goodflows`).
    let cache = git_root
        .join(".goodflows/sprites/runners")
        .join(format!("v{version}-{arch}"));
    // The digest file lives next to the binary, so it only detects damage;
    // what makes the copy trustworthy is that nobody else could write it.
    let trusted = {
        let cache = cache.clone();
        tokio::task::spawn_blocking(move || cache_trusted(&cache)).await?
    };
    if let (true, Ok(binary), Ok(recorded)) = (
        trusted,
        tokio::fs::read(cache.join("goodflows")).await,
        tokio::fs::read_to_string(cache.join("goodflows.sha256")).await,
    ) {
        // Only an intact copy of what was verified at download time is used.
        let digest = control::sha256_hex(&binary);
        if digest == recorded.trim() {
            return Ok((cache.join("goodflows"), digest, None, true));
        }
    }
    let archive = format!("goodflows-v{version}-{arch}-unknown-linux-gnu.tar.gz");
    let base = format!(
        "{}/v{version}",
        testhook::release_base()
            .unwrap_or_else(|| "https://github.com/agoodway/goodflows/releases/download".into())
    );
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    let client = &client;
    let fetch = |url: String| async move {
        let response = client.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(refusal(format!(
                "no published Linux {arch} runner for goodflows v{version} ({url} returned HTTP {}); install a matching release or run from a Linux controller",
                response.status()
            )));
        }
        Ok::<_, anyhow::Error>(response.bytes().await?)
    };
    let bytes = fetch(format!("{base}/{archive}")).await?;
    let checksum = fetch(format!("{base}/{archive}.sha256")).await?;
    let expected = String::from_utf8_lossy(&checksum)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    if control::sha256_hex(&bytes) != expected {
        return Err(refusal(format!(
            "downloaded runner {archive} does not match its published checksum"
        )));
    }
    let dir = tempfile::tempdir()?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    let path = dir.path().join("goodflows");
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.file_name() == Some(std::ffi::OsStr::new("goodflows"))
            && entry.header().entry_type() == tar::EntryType::Regular
        {
            let mut content = Vec::new();
            entry.read_to_end_sync(&mut content)?;
            let digest = control::sha256_hex(&content);
            // A cache that cannot be written only costs a later download.
            if let Ok(cached) = cache_runner(&cache, &content, &digest) {
                return Ok((cached, digest, None, false));
            }
            std::fs::write(&path, &content)?;
            return Ok((path, digest, Some(dir), false));
        }
    }
    Err(refusal(format!(
        "release archive {archive} has no goodflows executable"
    )))
}

/// Whether the runner cache at `dir` can be used: the directory is private
/// (owned by this user, no group or other access), both files are regular
/// files owned by this user and not writable by others, and Git does not
/// track anything there (a runner committed to the repository is never a
/// cache entry).
fn cache_trusted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let owned = |path: &Path, private_dir: bool| {
        std::fs::symlink_metadata(path).is_ok_and(|m| {
            m.uid() == uid
                && if private_dir {
                    m.is_dir() && m.mode() & 0o077 == 0
                } else {
                    m.is_file() && m.mode() & 0o022 == 0
                }
        })
    };
    if !owned(dir, true)
        || !owned(&dir.join("goodflows"), false)
        || !owned(&dir.join("goodflows.sha256"), false)
    {
        return false;
    }
    // Outside a Git repository nothing is tracked.
    match std::process::Command::new("git")
        .args(["ls-files", "-z", "--"])
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => out.stdout.is_empty(),
        _ => true,
    }
}

/// Store a verified runner in the private controller cache: the binary first,
/// then its digest, each replaced atomically. A cache directory that is not
/// private to this user is left alone.
fn cache_runner(dir: &Path, content: &[u8], digest: &str) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    crate::fsutil::create_state_dir(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        bail!("{} is not a private directory", dir.display());
    }
    for (name, bytes) in [
        ("goodflows", content),
        ("goodflows.sha256", format!("{digest}\n").as_bytes()),
    ] {
        let mut staged = tempfile::Builder::new()
            .prefix(".cache-")
            .tempfile_in(dir)?;
        std::io::Write::write_all(&mut staged, bytes)?;
        staged.as_file().sync_all()?;
        staged.persist(dir.join(name))?;
    }
    Ok(dir.join("goodflows"))
}

trait ReadSync {
    fn read_to_end_sync(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize>;
}

impl<R: std::io::Read> ReadSync for R {
    fn read_to_end_sync(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        std::io::Read::read_to_end(self, buf)
    }
}

const INSTALL_RUNNER: &str = r#"set -eu
umask 077
d="$HOME/.goodflows/runners/$1"
mkdir -p "$d"
t="$d/.goodflows.$$"
cat > "$t"
s=$(sha256sum "$t" | cut -d' ' -f1)
if [ "$s" != "$2" ]; then rm -f "$t"; echo "runner checksum mismatch" >&2; exit 1; fi
chmod 0755 "$t"
# Never replace a runner a supervisor may be executing (its /proc/self/exe
# would dangle): link without clobbering, and accept an existing identical one.
# The path is content-addressed, so different content there is refused.
if ln "$t" "$d/goodflows" 2>/dev/null; then rm -f "$t"; exit 0; fi
rm -f "$t"
e=$(sha256sum "$d/goodflows" | cut -d' ' -f1)
if [ "$e" = "$2" ]; then exit 0; fi
echo "runner-conflict"
echo "$d/goodflows exists with different content (sha256 $e, expected $2); refusing to replace it" >&2
exit 1"#;

/// Write the ownership record from stdin without ever replacing one.
const INIT_OWNER: &str = r#"set -eu
umask 077
d="$HOME/.goodflows"
mkdir -p "$d"
t="$d/.owner.$$"
cat > "$t"
sync "$t" 2>/dev/null || true
if ln "$t" "$d/owner.json"; then rm -f "$t"; sync "$d" 2>/dev/null || true; else rm -f "$t"; exit 1; fi"#;

const SET_CURRENT: &str = r#"set -eu
umask 077
printf '%s\n' "$1" > "$HOME/.goodflows/runners/.current.$$"
mv -f "$HOME/.goodflows/runners/.current.$$" "$HOME/.goodflows/runners/current""#;

/// The live run the identity is busy with, asked of the installed runner
/// with the same reconciliation admission performs. `None` when there is no
/// usable runner yet or the answer is inconclusive: admission still decides.
async fn busy_before_install(sprite: &Sprite, scope: &Scope) -> Option<String> {
    let scope_hex = agent::encode_scope(scope);
    let captured = sprite
        .agent("current", &["status", &scope_hex, "reconcile"])
        .await
        .ok()?;
    let status = agent_json(&captured, sprite, "status").ok()?;
    if status["busy"] != true {
        return None;
    }
    let active = status["active_run_id"].as_str()?;
    // Only a recorded, nonterminal active run: a transient lock holder (or a
    // run that is just finishing) is left to admission's own short wait.
    let phase = status["runs"]
        .as_array()?
        .iter()
        .find(|r| r["run_id"] == active)?
        .get("phase")?
        .clone();
    let phase: Phase = serde_json::from_value(phase).ok()?;
    (!phase.terminal()).then(|| active.to_owned())
}

/// Install (if needed) and verify the exact runner; returns its id.
async fn provision_runner(
    sprite: &Sprite,
    scope: &Scope,
    output: &Output,
    git_root: &Path,
) -> Result<String> {
    let arch = sprite.exec(&["uname", "-m"]).await?;
    if !arch.success {
        bail!("could not inspect the Sprite: {}", sprite.diagnostic(&arch));
    }
    let arch = match String::from_utf8_lossy(&arch.stdout).trim() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => return Err(refusal(format!("unsupported Sprite architecture {other}"))),
    };
    let mut fallback = None;
    if let Some((path, digest)) = local_runner(arch).await? {
        match install_runner(sprite, scope, arch, &path, &digest).await? {
            Ok(id) => return Ok(id),
            Err(diagnostic) => {
                // The platform still answers, so the runner itself failed to
                // run there (for example this controller needs a newer glibc
                // than the Sprite has): use the exact release build instead.
                let reachable = sprite.exec(&["true"]).await?;
                if !reachable.success {
                    bail!(
                        "the installed runner could not be verified: {diagnostic}; the Sprite then failed a plain command: {}",
                        sprite.diagnostic(&reachable)
                    );
                }
                fallback = Some(diagnostic);
            }
        }
    }
    let (path, digest, _keep, cached) = match (release_runner(arch, git_root).await, &fallback) {
        (Ok(release), _) => release,
        (Err(error), Some(diagnostic)) => {
            return Err(error.context(format!(
                "this goodflows executable does not run on the Sprite ({diagnostic}), and the published release runner could not be used"
            )))
        }
        (Err(error), None) => return Err(error),
    };
    if let Some(diagnostic) = fallback {
        let version = env!("CARGO_PKG_VERSION");
        emit(
            output,
            json!({"type": "sprite_runner_fallback", "reason": diagnostic,
                "release_version": version, "release_runner": format!("{version}-{}", &digest[..16]),
                "cached": cached}),
            format!(
                "This goodflows executable does not run on the Sprite ({diagnostic}); using the published goodflows v{version} release runner instead{}",
                if cached { " (cached)" } else { "" }
            ),
        )
        .await;
    }
    match install_runner(sprite, scope, arch, &path, &digest).await? {
        Ok(id) => Ok(id),
        Err(diagnostic) => bail!("the installed runner could not be verified: {diagnostic}"),
    }
}

/// Install (if needed) and verify one runner build. `Ok(Err(diagnostic))`
/// when the installed runner did not answer its version check at all.
async fn install_runner(
    sprite: &Sprite,
    scope: &Scope,
    arch: &str,
    path: &Path,
    digest: &str,
) -> Result<std::result::Result<String, String>> {
    let id = format!("{}-{}", env!("CARGO_PKG_VERSION"), &digest[..16]);
    let hello = |captured: &transport::Captured| -> bool {
        captured.success
            && serde_json::from_slice::<Value>(&captured.stdout).is_ok_and(|v| {
                v["version"] == env!("CARGO_PKG_VERSION")
                    && v["protocol"] == PROTOCOL
                    && v["arch"] == arch
            })
    };
    let first = sprite.agent(&id, &["hello"]).await?;
    if hello(&first) {
        return Ok(Ok(id));
    }
    // Already installed with exactly this content but it does not answer:
    // uploading it again cannot help. (Other content at that path goes on to
    // the install, which refuses to replace it.)
    let present = sprite
        .exec(&[
            "sh",
            "-c",
            r#"f="$HOME/.goodflows/runners/$1/goodflows"; test -f "$f" && test "$(sha256sum "$f" | cut -d' ' -f1)" = "$2""#,
            "check-runner",
            &id,
            digest,
        ])
        .await?;
    if present.success {
        // One more try before giving up on this build: a single failed call
        // (a dropped session, say) must not switch to the release runner.
        let again = sprite.agent(&id, &["hello"]).await?;
        if hello(&again) {
            return Ok(Ok(id));
        }
        let first = again;
        if first.success {
            return Err(refusal(format!(
                "the installed runner does not match this goodflows (version/protocol/architecture): {}",
                String::from_utf8_lossy(&first.stdout).trim()
            )));
        }
        return Ok(Err(sprite.diagnostic(&first)));
    }
    // A contender that would install a different runner learns first
    // whether the identity is busy, so a rejection usually leaves nothing
    // behind (advisory; admission still decides).
    if let Some(active) = busy_before_install(sprite, scope).await {
        return Err(busy(&scope.identity, &active, ""));
    }
    // Large reads: each read of a tokio file is a blocking-pool round trip.
    let mut file = tokio::io::BufReader::with_capacity(1 << 20, tokio::fs::File::open(path).await?);
    let installed = sprite
        .exec_stdin(
            &["sh", "-c", INSTALL_RUNNER, "install-runner", &id, digest],
            &mut file,
        )
        .await?;
    if !installed.success {
        let message = format!(
            "could not install the GoodFlows runner: {}",
            sprite.diagnostic(&installed)
        );
        if installed.stdout.starts_with(b"runner-conflict") {
            // A different runner under the content-addressed path is never
            // replaced: destroy the idle Sprite or remove it manually.
            return Err(refusal(format!(
                "{message}; destroy the idle Sprite (`goodflows sprite destroy`) or remove that file after checking no supervisor runs it"
            )));
        }
        bail!(message);
    }
    let check = sprite.agent(&id, &["hello"]).await?;
    if check.success && !hello(&check) {
        return Err(refusal(format!(
            "the installed runner does not match this goodflows (version/protocol/architecture): {}",
            String::from_utf8_lossy(&check.stdout).trim()
        )));
    }
    if !hello(&check) {
        return Ok(Err(sprite.diagnostic(&check)));
    }
    Ok(Ok(id))
}

/// Listings (one second apart) that a new Sprite gets to appear with its id.
const CREATE_LIST_ATTEMPTS: u32 = 10;

/// Create or reuse the identity's Sprite and verify its ownership. Only this
/// call's own successful create may initialize an unowned Sprite.
async fn ensure_owned(
    sprite: &Sprite,
    scope: &Scope,
    found: Option<Option<String>>,
    output: &Output,
    git_root: &Path,
) -> Result<String> {
    let created = match found {
        Some(_) => false,
        None => {
            emit(
                output,
                json!({"type": "sprite_create", "sprite": sprite.name}),
                format!("Creating Sprite {} in {}", sprite.name, sprite.org),
            )
            .await;
            sprite.create().await?
        }
    };
    if created {
        // Ownership is recorded before anything else is installed, so a later
        // provisioning failure leaves an owned Sprite that a retry can reuse.
        // The record needs the platform's stable id (destroy verifies it), and
        // a new Sprite may take a moment to be listed with one.
        let mut id = None;
        for attempt in 0..CREATE_LIST_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            id = sprite.lookup().await?.flatten();
            if id.is_some() {
                break;
            }
        }
        if id.is_none() {
            return Err(refusal(format!(
                "Sprite {} was created, but the platform did not list its resource id within {CREATE_LIST_ATTEMPTS} listings, so its ownership was not recorded and GoodFlows will not use or destroy it; check it and delete it with the Sprite CLI (`sprite destroy -o {} {}`), then run again",
                sprite.name, sprite.org, sprite.name
            )));
        }
        let mut record =
            serde_json::to_vec_pretty(&scope.owner(id, control::now_ms().to_string()))?;
        record.push(b'\n');
        let init = sprite
            .exec_stdin(
                &["sh", "-c", INIT_OWNER, "init-owner"],
                &mut std::io::Cursor::new(record),
            )
            .await?;
        if !init.success {
            match read_owner(sprite).await? {
                // Another initializer won: accept only an exactly matching record.
                Some(owner) => owner
                    .verify(scope)
                    .map_err(|error| refusal(format!("{error:#}")))?,
                None => bail!(
                    "ownership initialization failed: {}",
                    sprite.diagnostic(&init)
                ),
            }
        }
        return provision_runner(sprite, scope, output, git_root).await;
    }
    // Reuse, a lost create acknowledgement, or a racing creator: wait only for
    // bounded ownership initialization, then verify or fail. Never adopt.
    let deadline = Instant::now() + testhook::owner_wait();
    loop {
        match sprite.lookup().await? {
            None => bail!(
                "Sprite {} could not be created and does not exist",
                sprite.name
            ),
            Some(_) => {
                if let Some(owner) = read_owner(sprite).await? {
                    owner
                        .verify(scope)
                        .map_err(|error| refusal(format!("{error:#}")))?;
                    return provision_runner(sprite, scope, output, git_root).await;
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(refusal(format!(
                "Sprite {} exists without GoodFlows ownership; refusing to adopt, modify or destroy it (reconcile it manually)",
                sprite.name
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// `Ok(Some((run, reason)))` when the identity is busy with that live run.
async fn ensure_supervisor(
    sprite: &Sprite,
    runner: &str,
    scope: &Scope,
) -> Result<Option<(String, String)>> {
    let scope_hex = agent::encode_scope(scope);
    let reply = sprite
        .agent(runner, &["ensure-supervisor", &scope_hex])
        .await?;
    let value = agent_json(&reply, sprite, "supervisor readiness")?;
    if value["status"] == "caller_busy" {
        let field = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        return Ok(Some((field("active_run_id"), field("reason"))));
    }
    if value["status"] != "ready" {
        bail!("the GoodFlows supervisor is not ready");
    }
    let current = sprite
        .exec(&["sh", "-c", SET_CURRENT, "set-current", runner])
        .await?;
    if !current.success {
        bail!(
            "could not record the current runner: {}",
            sprite.diagnostic(&current)
        );
    }
    Ok(None)
}

/// The conflict rejection for a contender (exit 1); the run was not modified.
fn busy(identity: &str, active: &str, reason: &str) -> anyhow::Error {
    let active = if active.is_empty() {
        "(unknown)"
    } else {
        active
    };
    let reason = if reason.is_empty() {
        String::new()
    } else {
        format!(" ({})", transport::sanitize(reason.as_bytes(), &[]))
    };
    refusal(format!(
        "Sprite identity {identity} is busy with run {active}{reason}; it was not modified (check `goodflows sprite status`)"
    ))
}

// ---------------------------------------------------------------------------
// Remote run

pub async fn run(
    input: RunInput,
    output: &mut Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let RunInput {
        document,
        config_file,
        name,
        selected,
        targets,
        identity,
        request,
        rerun_config,
    } = input;
    let started = Instant::now();
    // Local preflight: nothing below contacts a Sprite until it all passes.
    let prefix = prefix_of(&document)?;
    let sprite_config = document.sprite.clone().unwrap_or_default();
    let setup = sprite_config.setup.clone().context(
        "Sprite runs require [sprite] setup (use `true` for a deliberate no-op); setup scripts are never discovered",
    )?;
    let resume = sprite_config.resume.clone().context(
        "Sprite runs require [sprite] resume (use `true` for a deliberate no-op); resume scripts are never discovered",
    )?;
    check_remote_env(&document, &sprite_config)?;
    let forwarded = resolve_pass_env(&sprite_config.pass_env)?;
    let cli = sprite_cli()?;
    let config_dir = config_file
        .parent()
        .context("config has no parent directory")?;
    let root = snapshot::git_root(config_dir)?;
    let workdirs = snapshot::map_workdirs(&root, &document)?;
    let hook_workdir = selected
        .settings
        .workdir
        .clone()
        .context("workflow workdir")?;
    let mut secrets = controller_secrets(&document);
    let sensitive = snapshot::Sensitive {
        values: secrets
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .chain(
                forwarded
                    .values()
                    .filter(|v| v.len() >= MIN_SENTINEL_BYTES)
                    .map(|v| v.as_bytes().to_vec()),
            )
            .collect(),
    };
    secrets.extend(forwarded.values().filter(|v| v.len() >= 4).cloned());
    let snap = tokio::task::block_in_place(|| {
        snapshot::take(
            &root,
            &config_file,
            workdirs,
            &[("setup", &setup), ("resume", &resume)],
            &hook_workdir,
            &sensitive,
        )
    })?;
    drop(sensitive);
    let snapshot_ms = started.elapsed().as_millis() as u64;
    testhook::pause("snapshot-taken");
    emit(
        output,
        json!({"type": "sprite_snapshot", "files": snap.manifest.len(), "archive_bytes": snap.archive_bytes,
            "excluded": snap.excluded.iter().map(|(p, r)| json!({"path": p, "reason": r})).collect::<Vec<_>>(),
            "duration_ms": snapshot_ms}),
        {
            let mut message = format!(
                "Snapshot: {} files, {} bytes compressed",
                snap.manifest.len(),
                snap.archive_bytes
            );
            for (path, reason) in &snap.excluded {
                message.push_str(&format!("\n  excluded {path} ({reason})"));
            }
            message
        },
    )
    .await;
    // Remote phase.
    // Before any receipt or submission an organization failure (often
    // authentication) is a preflight error.
    let org = transport::resolve_org(&cli).await?;
    let scope = Scope {
        organization: org.clone(),
        prefix,
        identity: identity.clone(),
    };
    let run_id = uuid::Uuid::new_v4().hyphenated().to_string();
    let sprite = Sprite {
        cli,
        org,
        name: scope.sprite_name(),
        secrets,
    };
    // Ctrl-C before submission stops at the next safe point (after the
    // current Sprite CLI call) and submits nothing; it never interrupts a
    // create or install half way.
    let stop_unsubmitted = |cancel: &watch::Receiver<Option<i32>>| -> Result<()> {
        if cancel.borrow().is_some() {
            bail!("interrupted before submission; nothing was submitted");
        }
        Ok(())
    };
    stop_unsubmitted(&cancel)?;
    // The platform must report the requested organization before any
    // receipt, create or exec: a mismatch is a configuration error (exit 1).
    let found = match sprite.lookup().await {
        Ok(found) => found,
        Err(error) if error.is::<transport::OrgMismatch>() => return Err(error),
        Err(error) => return unresolved(output, &identity, None, format!("{error:#}")).await,
    };
    // The private receipt is durable before the first remote submission.
    let base = bundle_base(&scope, &run_id, None)?;
    crate::fsutil::create_state_dir(&base)?;
    control::write_json(
        &base.join("receipt.json"),
        &json!({"schema": 1, "run_id": run_id, "organization": scope.organization, "prefix": scope.prefix,
            "identity": scope.identity, "sprite": sprite.name, "scope_digest": scope.digest(),
            "created_at_ms": control::now_ms()}),
    )?;
    emit(
        output,
        json!({"type": "sprite_run", "run_id": run_id, "identity": identity, "sprite": sprite.name,
            "organization": sprite.org, "receipt": base.join("receipt.json")}),
        format!("Sprite run {run_id} on {} ({})", sprite.name, sprite.org),
    )
    .await;
    stop_unsubmitted(&cancel)?;
    let provision_start = Instant::now();
    let runner = match ensure_owned(&sprite, &scope, found, output, &root).await {
        Ok(runner) => runner,
        Err(error) => {
            if error
                .chain()
                .any(|cause| cause.is::<Refusal>() || cause.is::<transport::OrgMismatch>())
            {
                return Err(error);
            }
            return unresolved(output, &identity, None, format!("{error:#}")).await;
        }
    };
    stop_unsubmitted(&cancel)?;
    match ensure_supervisor(&sprite, &runner, &scope).await {
        Ok(None) => {}
        Ok(Some((active, reason))) => return Err(busy(&identity, &active, &reason)),
        Err(error) => return unresolved(output, &identity, None, format!("{error:#}")).await,
    }
    let provision_ms = provision_start.elapsed().as_millis() as u64;
    let header = Header {
        protocol: PROTOCOL,
        run_id: run_id.clone(),
        scope_digest: scope.digest(),
        runner_version: env!("CARGO_PKG_VERSION").into(),
        config_text: snap.config_text.clone(),
        config_path: snap.config_path.clone(),
        workdirs: snap.workdirs.clone(),
        manifest_sha256: Header::manifest_digest(&snap.manifest),
        manifest: snap.manifest.clone(),
        archive_sha256: snap.archive_sha256.clone(),
        archive_bytes: snap.archive_bytes,
        setup,
        resume,
        pass_env: sprite_config.pass_env.clone(),
        request,
        // Captured from the archive itself; jobs never see it.
        provenance: Provenance {
            commit: snap.source.as_ref().and_then(|s| s.commit.clone()),
            branch: snap.source.as_ref().and_then(|s| s.branch.clone()),
            dirty: snap.source.as_ref().and_then(|s| s.dirty),
            controller_version: env!("CARGO_PKG_VERSION").into(),
        },
        upload_timeout_secs: UPLOAD_TIMEOUT_SECS,
    };
    // Header frame, then the environment as a separate private frame, then
    // the archive: nothing secret is ever placed in argv.
    let mut frames = Vec::new();
    for frame in [
        serde_json::to_vec(&header)?,
        serde_json::to_vec(&forwarded)?,
    ] {
        frames.extend_from_slice(&(frame.len() as u64).to_be_bytes());
        frames.extend_from_slice(&frame);
    }
    drop(forwarded);
    let archive = tokio::fs::File::open(&snap.archive).await?;
    let mut input = std::io::Cursor::new(frames).chain(archive);
    let transfer_start = Instant::now();
    let scope_hex = agent::encode_scope(&scope);
    testhook::pause("before-submit");
    stop_unsubmitted(&cancel)?;
    let submitted = sprite
        .agent_stdin(&runner, &["submit", &scope_hex], &mut input)
        .await;
    drop(input);
    let transfer_ms = transfer_start.elapsed().as_millis() as u64;
    drop(snap);
    let reply = match submitted.and_then(|c| agent_json(&c, &sprite, "submission")) {
        Ok(reply) => reply,
        Err(error) => {
            return unresolved(output, &identity, Some(&run_id), format!("{error:#}")).await
        }
    };
    match reply["status"].as_str() {
        Some("accepted" | "existing") => {}
        Some("caller_busy") => {
            return Err(busy(
                &identity,
                reply["active_run_id"].as_str().unwrap_or_default(),
                "",
            ))
        }
        Some("refused") => bail!(
            "Sprite refused the run: {}",
            reply["reason"].as_str().unwrap_or("unknown reason")
        ),
        Some("transfer_failed") => {
            return unresolved(
                output,
                &identity,
                Some(&run_id),
                format!(
                    "source transfer failed: {}",
                    reply["reason"].as_str().unwrap_or("unknown")
                ),
            )
            .await
        }
        _ => {
            return unresolved(
                output,
                &identity,
                Some(&run_id),
                reply["reason"]
                    .as_str()
                    .unwrap_or("unexpected submission reply")
                    .to_owned(),
            )
            .await
        }
    }
    // Follow the durable stream; reconnect without replaying printed lines.
    // A lost connection is retried with backoff until no new output has
    // arrived for the whole reconnect budget; the run itself never depends on
    // this connection.
    let budget = testhook::follow_budget();
    let idle = testhook::follow_idle();
    let mut deadline = Instant::now() + budget;
    let mut backoff = FOLLOW_BACKOFF_MIN;
    let mut last_error = String::new();
    let mut printed = 0usize;
    let mut summary: Option<Value> = None;
    // Native workflow events name their own workflow (a called one for
    // child events); lifecycle events take this run's context.
    let relay = output.relay();
    // Called invocations' own summaries, which stream through like every
    // other event, in arrival order.
    let mut children: Vec<Value> = Vec::new();
    let mut record: Option<Record> = None;
    let mut cancel_sent = false;
    let mut cancel_lost = false;
    let mut not_live = false;
    let mut cancel_watch = cancel.clone();
    'follow: loop {
        let printed_before = printed;
        // A replayed line (or a skip acknowledgement) proves nothing new. A
        // connection counts as established only if it delivered new lines, or
        // delivered something and stayed up for a while; losing an
        // established connection restarts the budget. A stream that replays
        // and drops again (a failing agent, or sessions cut after seconds), or
        // one that never delivers anything (a stalled session cut by the idle
        // bound), uses up the budget and ends with exit 3.
        let mut connected_for = std::time::Duration::ZERO;
        // Whether this connection delivered anything at all: every live agent
        // sends at least the replayed `accepted` event or a skip
        // acknowledgement, so a connection that stays silent from the start
        // (a stalled session) never counts as working.
        let mut delivered = false;
        // A reconnect asks the runner to leave out what was already printed.
        let skip = printed.to_string();
        let mut follow_args = vec!["follow", scope_hex.as_str(), run_id.as_str()];
        if printed > 0 {
            follow_args.extend(["--skip", skip.as_str()]);
        }
        match sprite.agent_stream(&runner, &follow_args) {
            Err(error) => last_error = format!("{error:#}"),
            Ok(mut stream) => 'stream: {
                let connected_at = Instant::now();
                let Some(stdout) = stream.child.stdout.take() else {
                    last_error = "the follow stream has no output".to_owned();
                    let _ = stream.child.kill().await;
                    break 'stream;
                };
                let mut reader = tokio::io::BufReader::new(stdout);
                // One stream line, kept across interrupted reads (read_until
                // appends; it is cancel safe) and cleared once complete.
                let mut buf = Vec::new();
                let mut seen = 0usize;
                loop {
                    let read = tokio::select! {
                        read = tokio::time::timeout(idle, reader.read_until(b'\n', &mut buf)) => match read {
                            Ok(line) => line,
                            Err(_) => {
                                // A silent connection may be dead: reconnect
                                // (the stream replays; printed lines are skipped).
                                last_error = format!("no data for {}s; reconnecting", idle.as_secs());
                                let _ = stream.child.kill().await;
                                break;
                            }
                        },
                        _ = cancel_watch.changed(), if !cancel_sent => {
                            cancel_sent = true;
                            if !forward_cancel(&sprite, &runner, &scope_hex, &run_id, output).await {
                                // The caller asked to stop; without an acknowledgement
                                // the run may continue, so report it as unresolved now.
                                cancel_lost = true;
                                let _ = stream.child.kill().await;
                                break 'follow;
                            }
                            continue;
                        }
                    };
                    // Only a complete line counts. A fragment before EOF (the
                    // connection dropped mid-line) is discarded untouched: the
                    // runner sends it again, in full, on the next connection.
                    if read.is_err() || !buf.ends_with(b"\n") {
                        break;
                    }
                    let line = std::mem::take(&mut buf);
                    if !delivered && printed > 0 {
                        // A runner that honoured `--skip` acknowledges it
                        // first; an older runner replays from the start.
                        if let Ok(ack) = serde_json::from_slice::<Value>(&line) {
                            if ack["type"] == "sprite_follow" && ack["skip"] == json!(printed) {
                                delivered = true;
                                seen = printed;
                                continue;
                            }
                        }
                    }
                    delivered = true;
                    seen += 1;
                    if seen <= printed {
                        continue;
                    }
                    printed = seen;
                    let Ok(event) = serde_json::from_slice::<Value>(&line) else {
                        continue;
                    };
                    match event["type"].as_str() {
                        // The root summary closes the run after collection;
                        // a called workflow's summary has a parent.
                        // The first root summary for this run is held; any
                        // other parentless summary is never the root's.
                        Some("summary") if event["parent_run_id"].is_null() => {
                            if summary.is_none() && event["run_id"] == run_id.as_str() {
                                summary = Some(event)
                            }
                        }
                        Some("summary") => {
                            children.push(event.clone());
                            let _ = relay.emit(event).await;
                        }
                        Some("sprite_record") => {
                            record = serde_json::from_value(event["record"].clone()).ok();
                            if event["live"] == false {
                                not_live = true;
                            }
                        }
                        _ if event.get("workflow").is_some() => {
                            let _ = relay.emit(event).await;
                        }
                        _ => {
                            let _ = output.emit(event).await;
                        }
                    }
                }
                let _ = stream.child.wait().await;
                connected_for = connected_at.elapsed();
                let diagnostic = stream.stderr().await;
                if !diagnostic.is_empty() {
                    last_error = transport::sanitize(&diagnostic, &sprite.secrets);
                }
            }
        }
        if not_live || record.as_ref().is_some_and(|r| r.phase.terminal()) {
            break;
        }
        // The budget counts only time without a working connection or new
        // output: a long quiet run that drops its stream still reconnects.
        let established = printed > printed_before
            || (delivered && connected_for >= FOLLOW_ESTABLISHED.min(budget));
        if established {
            deadline = Instant::now() + budget;
            backoff = FOLLOW_BACKOFF_MIN;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff.min(deadline - now)) => {}
            _ = cancel_watch.changed(), if !cancel_sent => {
                cancel_sent = true;
                if !forward_cancel(&sprite, &runner, &scope_hex, &run_id, output).await {
                    cancel_lost = true;
                    break 'follow;
                }
            }
        }
        backoff = (backoff * 2).min(FOLLOW_BACKOFF_MAX);
    }
    let Some(record) = record.filter(|r| r.phase.terminal() && !cancel_lost) else {
        let reason = if cancel_lost {
            "cancellation acknowledgement was lost and no terminal result was retrieved".to_owned()
        } else if not_live {
            "the run's worker and descendants are gone without a terminal result; `goodflows sprite status` records the interruption (nothing is rerun)".to_owned()
        } else {
            format!(
                "the connection was lost before a terminal result was retrieved and reconnecting for {}s brought no new output ({}); the run continues remotely",
                budget.as_secs(),
                if last_error.is_empty() { "no diagnostic" } else { last_error.as_str() }
            )
        };
        return unresolved(output, &identity, Some(&run_id), reason).await;
    };
    // Required collection, then controller-side publication, then the summary.
    let collect_start = Instant::now();
    let collected = collect_run(&sprite, &runner, &scope_hex, &run_id, &base).await;
    let collect_ms = collect_start.elapsed().as_millis() as u64;
    let collected = match collected {
        Ok(collected) => {
            emit(
                output,
                json!({"type": "sprite_collected", "run_id": run_id, "bundle": collected.dir,
                    "complete": collected.complete, "files": collected.files}),
                format!("Collected results: {}", collected.dir.display()),
            )
            .await;
            Some(collected)
        }
        Err(error) => {
            emit(
                output,
                json!({"type": "sprite_collect_failed", "run_id": run_id, "reason": format!("{error:#}")}),
                format!("Result collection failed: {error:#}"),
            )
            .await;
            None
        }
    };
    if let (Some(collected), Some(reporting)) = (&collected, &selected.reporting) {
        // Like a local run: every called invocation's own session, then the
        // root's. A child spool is published only for a reachable target
        // that a summary of this run's tree names as a call's child.
        let mut spools: Vec<(String, &str, &Config)> = summary
            .as_ref()
            .map(|root| bound_children(&run_id, root, &children))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, workflow)| {
                let (workflow, target) = targets.get_key_value(workflow)?;
                Some((id.to_owned(), workflow.as_str(), target))
            })
            .collect();
        spools.push((run_id.clone(), &name, &selected));
        for (id, workflow, config) in spools {
            let spool = collected.dir.join("results/reports").join(&id);
            if !spool.join(crate::reporting::MANIFEST).exists() {
                continue;
            }
            let publish = trusted_spool(
                &spool,
                &id,
                reporting,
                workflow,
                header.request.environment.as_deref(),
                config,
            );
            match publish {
                Ok(()) => {
                    let mut replay_output = output.clone();
                    let _ = crate::reporting::replay::upload(
                        &spool,
                        &mut replay_output,
                        cancel.clone(),
                        None,
                    )
                    .await;
                }
                Err(error) => {
                    emit(
                        output,
                        json!({"type": "sprite_publication_refused", "run_id": id, "reason": format!("{error:#}")}),
                        format!("Report publication refused: {error:#}"),
                    )
                    .await
                }
            }
        }
    }
    emit(
        output,
        json!({"type": "sprite_timing", "run_id": run_id, "snapshot_ms": snapshot_ms, "provision_ms": provision_ms,
            "transfer_ms": transfer_ms, "collect_ms": collect_ms, "remote": record.timings}),
        format!(
            "Sprite timing: snapshot {snapshot_ms}ms, provision {provision_ms}ms, transfer {transfer_ms}ms, collection {collect_ms}ms"
        ),
    )
    .await;
    let signal = *cancel.borrow();
    if collected.as_ref().is_none_or(|c| !c.complete) {
        return unresolved(
            output,
            &identity,
            Some(&run_id),
            "required result collection is incomplete; retry with `goodflows sprite collect`"
                .into(),
        )
        .await;
    }
    let terminal = record.terminal.clone();
    match record.phase {
        Phase::Completed => {
            let code = terminal.and_then(|t| t.exit_code).unwrap_or(1);
            if let Some(summary) = summary {
                output
                    .emit_summary(
                        summary,
                        rerun_config.as_deref(),
                        Some(&identity),
                        signal.is_some(),
                    )
                    .await?;
            }
            Ok(signal.map(|s| (128 + s) as u8).unwrap_or(code as u8))
        }
        Phase::Cancelled => {
            if let Some(summary) = summary {
                output
                    .emit_summary(summary, rerun_config.as_deref(), Some(&identity), true)
                    .await?;
            }
            // An attached interrupt keeps the local signal code; otherwise the
            // remote runner's own signal-derived code, or 1 before jobs started.
            let native = terminal.and_then(|t| t.exit_code).unwrap_or(1);
            Ok(signal.map(|s| (128 + s) as u8).unwrap_or(native as u8))
        }
        Phase::PreparationFailed => {
            let reason = terminal
                .and_then(|t| t.reason)
                .unwrap_or_else(|| "preparation failed".into());
            bail!("Sprite run {run_id} failed before any job started: {reason}")
        }
        _ => {
            let reason = terminal
                .and_then(|t| t.reason)
                .unwrap_or_else(|| record.phase.name().into());
            unresolved(output, &identity, Some(&run_id), reason).await
        }
    }
}

/// The called invocations of run `run_id` whose spools may be published, as
/// `(run id, target workflow)` in arrival order. A child summary counts only
/// with a canonical UUID (it becomes a bundle path) and only when a summary
/// already accepted into this run's tree, the root's first, lists it as a
/// call result's `child_run_id` for that same target workflow.
fn bound_children<'a>(
    run_id: &str,
    root: &'a Value,
    children: &'a [Value],
) -> Vec<(&'a str, &'a str)> {
    if root["run_id"].as_str() != Some(run_id) {
        return Vec::new();
    }
    let calls = |summary: &'a Value| {
        summary["results"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r["child_run_id"].as_str()?, r["workflow"].as_str()?)))
    };
    let mut tree: Vec<&Value> = vec![root];
    let mut bound: Vec<(usize, &str, &str)> = Vec::new();
    loop {
        let next = children.iter().enumerate().find_map(|(at, child)| {
            let id = child["run_id"].as_str()?;
            let workflow = child["workflow"].as_str()?;
            let parent = child["parent_run_id"].as_str()?;
            let linked = crate::fsutil::canonical_uuid(id)
                && tree.iter().all(|s| s["run_id"].as_str() != Some(id))
                && tree.iter().any(|s| {
                    s["run_id"].as_str() == Some(parent) && calls(s).any(|c| c == (id, workflow))
                });
            linked.then_some((at, child, id, workflow))
        });
        let Some((at, child, id, workflow)) = next else {
            break;
        };
        tree.push(child);
        bound.push((at, id, workflow));
    }
    bound.sort_by_key(|(at, ..)| *at);
    bound.into_iter().map(|(_, id, w)| (id, w)).collect()
}

/// Forward an attached interrupt as a UUID-bound cancellation; `true` when
/// the supervisor side acknowledged it.
async fn forward_cancel(
    sprite: &Sprite,
    runner: &str,
    scope_hex: &str,
    run_id: &str,
    output: &Output,
) -> bool {
    let ack = sprite.agent(runner, &["cancel", scope_hex, run_id]).await;
    let acknowledged = ack
        .as_ref()
        .ok()
        .and_then(|c| agent_json(c, sprite, "cancel").ok())
        .is_some();
    emit(
        output,
        json!({"type": "sprite_cancel", "run_id": run_id, "acknowledged": acknowledged}),
        if acknowledged {
            format!("Cancellation of {run_id} requested")
        } else {
            format!("Cancellation of {run_id} was not acknowledged")
        },
    )
    .await;
    acknowledged
}

/// Stream the run's export and stop reading as soon as it exceeds the
/// finite collection ceiling: the controller never buffers more than that.
async fn collect_run(
    sprite: &Sprite,
    runner: &str,
    scope_hex: &str,
    run_id: &str,
    base: &Path,
) -> Result<collect::Collected> {
    let mut stream = sprite.agent_stream(runner, &["export", scope_hex, run_id])?;
    let stdout = stream.child.stdout.take().context("export stdout")?;
    let idle = transport::call_bound(transport::CALL_TIMEOUT);
    let bytes = match collect::read_bounded(stdout, collect::MAX_EXPORT_BYTES, idle).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = stream.child.kill().await;
            return Err(error);
        }
    };
    let status = stream.child.wait().await?;
    let diagnostic = stream.stderr().await;
    if !status.success() {
        bail!(
            "export failed: {}",
            transport::sanitize(&diagnostic, &sprite.secrets)
        );
    }
    let (base, run_id) = (base.to_path_buf(), run_id.to_owned());
    tokio::task::spawn_blocking(move || collect::publish(&base, &run_id, &bytes)).await?
}

/// Automatic publication only for spools bound to this trusted invocation:
/// the same run, project, workflow, profile, jobs/partitions and credential
/// variable names, and a run record only when the trusted configuration
/// opted into uploading one. A mismatch never triggers a lookup of other
/// variables.
pub fn trusted_spool(
    spool: &Path,
    run_id: &str,
    reporting: &config::Reporting,
    workflow: &str,
    environment: Option<&str>,
    selected: &Config,
) -> Result<()> {
    let manifest = crate::reporting::replay::load_manifest(spool)?;
    if manifest.workflow_run_id != run_id {
        bail!("spool belongs to a different run");
    }
    if manifest.project != reporting.project
        || manifest.workflow != workflow
        || manifest.environment.as_deref() != environment
    {
        bail!("spool scope (project, workflow or profile) does not match this invocation");
    }
    if manifest.url_env != reporting.url_env || manifest.token_env != reporting.token_env {
        bail!("spool names credential variables that differ from this invocation's configuration");
    }
    // A remote config that turned upload on cannot make the controller send
    // command text its caller did not opt into.
    if manifest.run_record.is_some() && !reporting.history {
        bail!("spool lists a run record, but this invocation's configuration does not set [reporting] history = true");
    }
    for job in &manifest.jobs {
        let configured = selected
            .jobs
            .iter()
            .find(|j| j.name == job.name && !j.reports.is_empty())
            .with_context(|| format!("spool job {:?} is not a selected reporting job", job.name))?;
        let count = configured.partitions.map(|p| p.count.get());
        if job.partition.map(|p| p.count) != count {
            bail!(
                "spool partition scope for job {:?} does not match",
                job.name
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recovery and destruction commands

/// Resolves once the user interrupts (never if the signal source is gone).
async fn interrupted(cancel: &mut watch::Receiver<Option<i32>>) {
    if cancel.wait_for(Option::is_some).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Recovery commands stop on Ctrl-C (exit 128+signal). Status, logs, collect
/// and cancel only read or make idempotent requests, so they stop at once;
/// destroy handles the interruption itself so it never leaves its own claim
/// behind before its delete call.
pub async fn command(
    cli: &crate::Cli,
    command: &Command,
    output: &mut Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let mut watch = cancel.clone();
    if matches!(command, Command::Destroy { .. }) {
        return command_inner(cli, command, output, cancel).await;
    }
    tokio::select! {
        result = command_inner(cli, command, output, cancel) => result,
        _ = interrupted(&mut watch) => match command {
            // The request may already have reached the supervisor.
            Command::Cancel { identity, run } => bail!(
                "interrupted; the cancellation of run {run} may or may not have been delivered (check `goodflows sprite status {identity}`)"
            ),
            _ => bail!("interrupted"),
        },
    }
}

/// `what`, unless the user interrupts first (nothing changed yet).
async fn or_interrupt<T>(
    cancel: &mut watch::Receiver<Option<i32>>,
    what: impl std::future::Future<Output = T>,
) -> Result<T> {
    tokio::select! {
        value = what => Ok(value),
        _ = interrupted(cancel) => bail!("interrupted; nothing was changed"),
    }
}

async fn command_inner(
    cli: &crate::Cli,
    command: &Command,
    output: &mut Output,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    if let Some(flag) = cli.run_only_flag() {
        bail!("{flag} cannot be used with sprite commands");
    }
    let identity = match command {
        Command::Status { identity }
        | Command::Logs { identity, .. }
        | Command::Collect { identity, .. }
        | Command::Cancel { identity, .. }
        | Command::Destroy { identity, .. } => identity.clone(),
    };
    // Recovery needs only the project prefix: no source, hooks or run-only values.
    let (document, _) = Config::load(cli.config.as_deref())?;
    let prefix = prefix_of(&document)?;
    if let Command::Destroy { yes: false, .. } = command {
        // A detached thread: an interrupted prompt must not keep the
        // runtime waiting on the terminal at exit.
        let (sender, answer) = tokio::sync::oneshot::channel();
        let asked = identity.clone();
        std::thread::spawn(move || {
            let _ = sender.send(confirm_destroy(&asked));
        });
        or_interrupt(&mut cancel, answer).await???;
    }
    let cli_path = sprite_cli()?;
    // Recovery reports an unreachable platform as unresolved (exit 3).
    let org = match or_interrupt(&mut cancel, transport::resolve_org(&cli_path)).await? {
        Ok(org) => org,
        Err(error) if error.is::<transport::Unreachable>() => {
            return unresolved(output, &identity, None, format!("{error:#}")).await
        }
        Err(error) => return Err(error),
    };
    let scope = Scope {
        organization: org.clone(),
        prefix,
        identity: identity.clone(),
    };
    let sprite = Sprite {
        cli: cli_path,
        org,
        name: scope.sprite_name(),
        secrets: controller_secrets(&document),
    };
    let scope_hex = agent::encode_scope(&scope);
    // An organization mismatch is refused (exit 1) before any exec, even a
    // read-only status: another organization's Sprite is never read.
    let exists = match or_interrupt(&mut cancel, sprite.lookup()).await? {
        Ok(found) => found,
        Err(error) if error.is::<transport::OrgMismatch>() => return Err(error),
        Err(error) => return unresolved(output, &identity, None, format!("{error:#}")).await,
    };
    let Some(sprite_id) = exists else {
        emit(
            output,
            json!({"type": "sprite_status", "identity": identity, "sprite": sprite.name, "state": "absent"}),
            format!("No Sprite {} exists for identity {identity}", sprite.name),
        )
        .await;
        return Ok(if matches!(command, Command::Status { .. }) {
            0
        } else {
            1
        });
    };
    match command {
        Command::Status { .. } => {
            let reply = sprite
                .agent("current", &["status", &scope_hex, "reconcile"])
                .await
                .and_then(|c| agent_json(&c, &sprite, "status"));
            match reply {
                Ok(status) => {
                    let message = status_text(&identity, &sprite.name, &status);
                    let mut event = status;
                    if let Some(map) = event.as_object_mut() {
                        map.remove("status");
                    }
                    event["type"] = json!("sprite_status");
                    event["identity"] = json!(identity);
                    event["sprite"] = json!(sprite.name);
                    event["state"] = json!(if event["busy"] == true {
                        "active"
                    } else {
                        "idle"
                    });
                    emit(output, event, message).await;
                    Ok(0)
                }
                Err(error) => {
                    emit(
                        output,
                        json!({"type": "sprite_status", "identity": identity, "sprite": sprite.name, "state": "unreachable"}),
                        format!(
                            "Sprite {} is unreachable or unowned: {}",
                            sprite.name,
                            crate::output::visible(&format!("{error:#}"))
                        ),
                    )
                    .await;
                    Ok(3)
                }
            }
        }
        Command::Logs { run, .. } => {
            let captured = match sprite
                .agent_bulk("current", &["logs", &scope_hex, run])
                .await
            {
                Ok(captured) => captured,
                Err(error) => {
                    return unresolved(output, &identity, Some(run), format!("{error:#}")).await
                }
            };
            if !captured.success {
                return unresolved(output, &identity, Some(run), sprite.diagnostic(&captured))
                    .await;
            }
            for line in captured.stdout.split(|b| *b == b'\n') {
                if let Ok(mut event) = serde_json::from_slice::<Value>(line) {
                    if event["type"] == "sprite_record" {
                        let phase = event["record"]["phase"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_owned();
                        event["message"] = json!(format!("Run {run}: {phase}"));
                    }
                    let _ = output.emit(event).await;
                }
            }
            Ok(0)
        }
        Command::Collect {
            run,
            output: target,
            ..
        } => {
            let base = bundle_base(&scope, run, target.as_deref())?;
            match collect_run(&sprite, "current", &scope_hex, run, &base).await {
                Ok(collected) => {
                    emit(
                        output,
                        json!({"type": "sprite_collected", "run_id": run, "bundle": collected.dir,
                            "complete": collected.complete, "phase": collected.phase, "files": collected.files,
                            "already_collected": collected.existing}),
                        format!(
                            "Collected {} results ({}): {}",
                            if collected.complete { "final" } else { "partial" },
                            collected.phase,
                            collected.dir.display()
                        ),
                    )
                    .await;
                    Ok(0)
                }
                Err(error) => unresolved(output, &identity, Some(run), format!("{error:#}")).await,
            }
        }
        Command::Cancel { run, .. } => {
            let reply = sprite
                .agent("current", &["cancel", &scope_hex, run])
                .await
                .and_then(|c| agent_json(&c, &sprite, "cancel"));
            match reply {
                Ok(reply) => {
                    let status = reply["status"].as_str().unwrap_or_default().to_owned();
                    emit(
                        output,
                        json!({"type": "sprite_cancel", "run_id": run, "status": status, "acknowledged": true}),
                        if status == "already_terminal" {
                            format!("Run {run} already finished; nothing was signalled")
                        } else {
                            format!("Cancellation of {run} requested")
                        },
                    )
                    .await;
                    Ok(0)
                }
                Err(error) => unresolved(output, &identity, Some(run), format!("{error:#}")).await,
            }
        }
        Command::Destroy { .. } => {
            destroy(&sprite, &scope_hex, sprite_id, &identity, output, cancel).await
        }
    }
}

/// Multi-line status text. Values come from the Sprite, so each is escaped
/// and the only newlines left are line breaks (see `sprite_status` in
/// src/output.rs).
fn status_text(identity: &str, name: &str, status: &Value) -> String {
    use crate::output::visible;
    let mut text = format!(
        "Sprite {} (identity {})\n  prepared: {}\n  active run: {}",
        visible(name),
        visible(identity),
        if status["prepared"].is_null() {
            "no"
        } else {
            "yes"
        },
        visible(status["active_run_id"].as_str().unwrap_or("none"))
    );
    if status["degraded"] == true {
        text.push_str("\n  admission blocked: wake hold renewal failed");
    }
    if status["destroying"] == true {
        text.push_str("\n  destruction in progress");
    }
    for run in status["runs"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "\n  {}  {}  {}",
            visible(run["run_id"].as_str().unwrap_or_default()),
            visible(run["phase"].as_str().unwrap_or_default()),
            visible(run["workflow"].as_str().unwrap_or("default"))
        ));
    }
    text
}

fn confirm_destroy(identity: &str) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        bail!("refusing to destroy without confirmation; pass --yes to confirm non-interactively");
    }
    eprint!(
        "Destroy the Sprite for identity {identity:?}? Its dependency caches, state and uncollected results are permanently lost. [y/N] "
    );
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        bail!("destruction cancelled");
    }
    Ok(())
}

async fn destroy(
    sprite: &Sprite,
    scope_hex: &str,
    expected_id: Option<String>,
    identity: &str,
    output: &Output,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    // Never interrupted mid-call: it either returns the claim this destroy
    // can remove, or (bounded by the call timeout) it fails.
    let reply = sprite
        .agent("current", &["prepare-destroy", scope_hex])
        .await
        .and_then(|c| agent_json(&c, sprite, "ownership check"));
    let reply = match reply {
        Ok(reply) => reply,
        // No further exec in a Sprite whose organization is not proven.
        Err(error) if error.chain().any(|c| c.is::<transport::OrgMismatch>()) => {
            return Err(error)
        }
        // No runner on the Sprite: no agent ran, so nothing was claimed.
        Err(error) if format!("{error:#}").contains(transport::NO_RUNNER) => bail!(
            "refusing to destroy {}: it has no GoodFlows runner, so its ownership cannot be verified ({error:#})",
            sprite.name
        ),
        // A failed or unanswered ownership check may still have written this
        // destroy's claim (a lost acknowledgement looks like any failure),
        // and only the agent knows its token: report it as unresolved.
        Err(error) => {
            return unresolved(
                output,
                identity,
                None,
                format!(
                    "the ownership check of {} did not complete ({error:#}); nothing was deleted. If it reached the Sprite, its destruction claim keeps refusing new runs until a `goodflows sprite destroy` run 15 minutes or more later sweeps it (that destroy also deletes the Sprite if it can)",
                    sprite.name
                ),
            )
            .await
        }
    };
    if reply["status"] == "refused" {
        bail!(
            "refusing to destroy {}: ownership could not be verified ({})",
            sprite.name,
            reply["reason"].as_str().unwrap_or("no reason given")
        );
    }
    if reply["status"] == "caller_busy" {
        bail!(
            "refusing to destroy {}: run {} is active or unresolved; cancel or reconcile it first",
            sprite.name,
            reply["active_run_id"].as_str().unwrap_or("(unknown)")
        );
    }
    testhook::pause("destroy-prepared");
    // This destroy's own claim on the destruction marker: aborting removes
    // only it, never the claim of a concurrent destroy.
    let token = reply["token"].as_str().map(str::to_owned);
    // Set just before the delete call: from then on an interruption keeps
    // the claim, because the delete may be in progress.
    let issued = std::sync::atomic::AtomicBool::new(false);
    let outcome = tokio::select! {
        outcome = tokio::time::timeout(
            DESTROY_BOUND,
            destroy_prepared(sprite, scope_hex, expected_id, identity, output, reply, &issued),
        ) => outcome.unwrap_or_else(|_| {
            Ok(DestroyEnd::InProgress(format!(
                "destruction of {} did not finish within {}s; its outcome is unknown",
                sprite.name,
                DESTROY_BOUND.as_secs()
            )))
        }),
        _ = interrupted(&mut cancel) => {
            if issued.load(std::sync::atomic::Ordering::SeqCst) {
                bail!(
                    "interrupted after the delete call for {} was issued: deletion may be in progress, so this destroy's claim stays and new runs stay refused; check `goodflows sprite status` and run `goodflows sprite destroy` again",
                    sprite.name
                );
            }
            let removed = match &token {
                Some(token) => sprite
                    .agent("current", &["abort-destroy", scope_hex, token])
                    .await
                    .map_err(|e| format!("{e:#}"))
                    .and_then(|c| if c.success { Ok(()) } else { Err(sprite.diagnostic(&c)) }),
                None => Ok(()),
            };
            match removed {
                Ok(()) => bail!(
                    "interrupted before the delete call; nothing was deleted and this destroy's claim was removed"
                ),
                Err(error) => bail!(
                    "interrupted before the delete call; nothing was deleted, but this destroy's claim could not be removed ({error}), so new runs stay refused until `goodflows sprite destroy` runs again"
                ),
            }
        }
    };
    // The marker is cleared only when deletion definitely did not happen and
    // will not happen: never once a delete may be in progress, and never
    // after an organization mismatch (no further exec in that Sprite).
    let abort = match &outcome {
        Ok(DestroyEnd::NotDeleted(_)) => true,
        Ok(DestroyEnd::Gone(_) | DestroyEnd::InProgress(_) | DestroyEnd::Untouchable(_)) => false,
        Err(error) => !error.chain().any(|c| c.is::<transport::OrgMismatch>()),
    };
    if let (true, Some(token)) = (abort, &token) {
        let _ = sprite
            .agent("current", &["abort-destroy", scope_hex, token])
            .await;
    }
    match outcome? {
        DestroyEnd::Gone(code) => Ok(code),
        DestroyEnd::NotDeleted(reason) | DestroyEnd::Untouchable(reason) => {
            unresolved(output, identity, None, reason).await
        }
        DestroyEnd::InProgress(reason) => {
            unresolved(
                output,
                identity,
                None,
                format!(
                    "{reason}; destruction may still be in progress, so new runs stay refused. Check `goodflows sprite status` and run `goodflows sprite destroy` again"
                ),
            )
            .await
        }
    }
}

/// Upper bound on one destroy after its ownership check; destruction marker
/// claims older than the agent's stale age (15 minutes) cannot belong to a
/// destroy that is still running.
const DESTROY_BOUND: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// Interval between listings while a delete may still be completing.
const DESTROY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a failed delete call's diagnostic is a definite refusal by the
/// platform API: an `API error (status 4xx)` other than 408 (timeout).
fn definite_rejection(diagnostic: &str) -> bool {
    diagnostic.match_indices("status ").any(|(at, _)| {
        let code = &diagnostic[at + "status ".len()..];
        let digits: String = code.chars().take_while(char::is_ascii_digit).collect();
        digits.len() == 3 && digits.starts_with('4') && digits != "408"
    })
}

/// How a prepared destruction ended.
enum DestroyEnd {
    /// The owned Sprite is gone (or replaced, so its marker went with it).
    Gone(u8),
    /// The platform definitely refused the delete call and the same Sprite
    /// is still listed after polling: nothing was deleted, so the marker is
    /// cleared.
    NotDeleted(String),
    /// The delete succeeded or its outcome is unknown, and the Sprite is
    /// still listed or could not be listed: the marker stays.
    InProgress(String),
    /// The platform no longer proves the organization: no further exec.
    Untouchable(String),
}

async fn destroy_prepared(
    sprite: &Sprite,
    scope_hex: &str,
    expected_id: Option<String>,
    identity: &str,
    output: &Output,
    reply: Value,
    issued: &std::sync::atomic::AtomicBool,
) -> Result<DestroyEnd> {
    // Re-check the exact resource immediately before deleting it. Without a
    // stable platform id on both sides the resource cannot be proven to be
    // the owned one, so destruction fails closed.
    let owned_id = reply["sprite_id"].as_str().map(str::to_owned);
    let current = sprite.lookup().await.with_context(|| {
        format!(
            "refusing to destroy {}: the resource could not be re-checked",
            sprite.name
        )
    })?;
    let refuse = match (&current, &owned_id) {
        (Some(Some(id)), Some(owned)) if id == owned && expected_id.as_ref() == Some(id) => None,
        (Some(Some(_)), Some(_)) => Some("the resource changed during the ownership check"),
        (None, _) => Some("the Sprite disappeared during the ownership check"),
        _ => Some(
            "the platform reported no stable resource id, so the owned resource cannot be verified; destroy it manually after checking",
        ),
    };
    if let Some(reason) = refuse {
        bail!("refusing to destroy {}: {reason}", sprite.name);
    }
    let current = current.flatten();
    // The claim must still be ours: a destroy whose controller was suspended
    // past the stale age may have had it swept by another destroy, which may
    // since have readmitted runs. Never delete without it.
    let token = reply["token"].as_str().unwrap_or_default();
    let held = sprite
        .agent("current", &["check-destroy", scope_hex, token])
        .await
        .and_then(|c| agent_json(&c, sprite, "destruction claim check"))
        .map(|v| v["held"] == true);
    match held {
        Ok(true) => {}
        Ok(false) => bail!(
            "refusing to destroy {}: this destroy's claim was removed as stale (its controller was suspended or too slow); nothing was deleted, run `goodflows sprite destroy` again",
            sprite.name
        ),
        Err(error) => bail!(
            "refusing to destroy {}: the destruction claim could not be verified ({error:#}); nothing was deleted",
            sprite.name
        ),
    }
    // From here on a delete may be in progress: no error may clear the marker.
    issued.store(true, std::sync::atomic::Ordering::SeqCst);
    let issued = sprite.destroy().await;
    testhook::pause("destroy-issued");
    let call_failed = issued.as_ref().map_or(true, |c| !c.success);
    let diagnostic = match &issued {
        Ok(captured) => sprite.diagnostic(captured),
        Err(error) => format!("{error:#}"),
    };
    // An accepted delete may complete asynchronously: re-list for a bounded
    // time before deciding.
    let started = Instant::now();
    let budget = testhook::destroy_poll();
    let mut listings = 0u32;
    let last = loop {
        let after = sprite.lookup().await;
        listings += 1;
        match &after {
            Ok(None) => {
                emit(
                    output,
                    json!({"type": "sprite_destroyed", "identity": identity, "sprite": sprite.name,
                        "listings": listings, "waited_ms": started.elapsed().as_millis() as u64}),
                    format!(
                        "Destroyed {} (identity {identity}); its cached state and uncollected results are gone",
                        sprite.name
                    ),
                )
                .await;
                return Ok(DestroyEnd::Gone(0));
            }
            Ok(Some(id)) if id.is_some() && *id != current => {
                // A replacement with the same name is never deleted blindly.
                let code = unresolved(
                    output,
                    identity,
                    None,
                    format!(
                        "{} was replaced by a new resource; it was not deleted",
                        sprite.name
                    ),
                )
                .await
                .unwrap_or(3);
                return Ok(DestroyEnd::Gone(code));
            }
            Err(error) if error.chain().any(|c| c.is::<transport::OrgMismatch>()) => {
                return Ok(DestroyEnd::Untouchable(format!(
                    "deletion of {} was not confirmed: {error:#}",
                    sprite.name
                )));
            }
            _ => {}
        }
        let elapsed = started.elapsed();
        if elapsed >= budget {
            break after;
        }
        tokio::time::sleep(DESTROY_POLL_INTERVAL.min(budget - elapsed)).await;
    };
    let still_same = matches!(&last, Ok(Some(Some(id))) if Some(id) == current.as_ref());
    let waited = started.elapsed().as_secs();
    // Only a definite platform refusal proves the delete was not accepted; a
    // transport error, timeout or server error may hide an accepted delete
    // that finishes after the polling budget.
    let rejected =
        matches!(&issued, Ok(captured) if !captured.success && definite_rejection(&diagnostic));
    Ok(if rejected && still_same {
        DestroyEnd::NotDeleted(format!(
            "deletion of {} was refused by the platform ({diagnostic}) and it is still listed after {waited}s; nothing was deleted and runs are admitted again",
            sprite.name
        ))
    } else {
        let listed = match &last {
            Ok(_) => "is still listed".to_owned(),
            Err(error) => format!("could not be listed ({error:#})"),
        };
        DestroyEnd::InProgress(format!(
            "deletion of {} was {} but the Sprite {listed} after {waited}s",
            sprite.name,
            if call_failed {
                format!("not confirmed ({diagnostic})")
            } else {
                "accepted".to_owned()
            }
        ))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_children_named_by_their_parents_call_results_are_published() {
        const ROOT: &str = "11111111-1111-4111-8111-111111111111";
        const CHILD: &str = "22222222-2222-4222-8222-222222222222";
        const GRAND: &str = "33333333-3333-4333-8333-333333333333";
        const OTHER: &str = "44444444-4444-4444-8444-444444444444";
        let summary = |id: &str, workflow: &str, parent: Option<&str>, calls: &[(&str, &str)]| {
            let mut event = json!({"type": "summary", "run_id": id, "workflow": workflow,
                "results": calls.iter().map(|(child, target)| json!({"name": "call",
                    "workflow": target, "child_run_id": child})).collect::<Vec<_>>()});
            if let Some(parent) = parent {
                event["parent_run_id"] = json!(parent);
            }
            event
        };
        let root = summary(ROOT, "ci", None, &[(CHILD, "mid"), ("../x", "mid")]);
        let children = [
            // Arrives first (it finishes first) and binds through `mid`.
            summary(GRAND, "leaf", Some(CHILD), &[]),
            summary(CHILD, "mid", Some(ROOT), &[(GRAND, "leaf")]),
            // Listed by the root, but not a UUID: never a path.
            summary("../x", "mid", Some(ROOT), &[]),
            // Not listed by its claimed parent, or listed for another target.
            summary(OTHER, "mid", Some(ROOT), &[]),
            summary(OTHER, "leaf", Some(CHILD), &[]),
            // A parent outside the tree, and the root's own id.
            summary(OTHER, "mid", Some(OTHER), &[]),
            summary(ROOT, "mid", Some(ROOT), &[]),
        ];
        assert_eq!(
            super::bound_children(ROOT, &root, &children),
            [(GRAND, "leaf"), (CHILD, "mid")]
        );
        // A root summary for another run binds nothing.
        assert!(super::bound_children(OTHER, &root, &children).is_empty());
    }

    use super::*;

    fn document(source: &str) -> Config {
        let mut config: Config = toml::from_str(source).unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn remote_env_rules_cover_every_layer_and_name() {
        let base = r#"
[reporting]
project = "p"
token_env = "MY_KEY"
[[jobs]]
name = "a"
command = "true"
"#;
        let ok = document(base);
        check_remote_env(&ok, &SpriteConfig::default()).unwrap();
        for name in [
            "SPRITE_TOKEN",
            "SPRITES_TOKEN",
            "MY_KEY",
            "GOODFLOWS_RUN_ID",
            "GOODFLOWS_OUTPUT",
        ] {
            let sprite = SpriteConfig {
                pass_env: vec![name.into()],
                ..Default::default()
            };
            let error = check_remote_env(&ok, &sprite).unwrap_err().to_string();
            assert!(error.contains(name), "{error}");
        }
        // The url_env is eligible unless it names a denied variable.
        let sprite = SpriteConfig {
            pass_env: vec!["GOODFLOWS_URL".into()],
            ..Default::default()
        };
        check_remote_env(&ok, &sprite).unwrap();
        for layer in [
            "[settings.env]\nSPRITE_TOKEN = \"x\"",
            "[workflows.w.settings.env]\nSPRITES_TOKEN = \"x\"\n[[workflows.w.jobs]]\nname = \"j\"\ncommand = \"true\"",
            "[environments.e.settings.env]\nMY_KEY_X = \"ok\"\nSPRITE_TOKEN = \"x\"",
        ] {
            let config = document(&format!("{layer}\n{base}"));
            let error = check_remote_env(&config, &SpriteConfig::default())
                .unwrap_err()
                .to_string();
            assert!(error.contains("SPRITE"), "{error}");
            assert!(!error.contains("\"x\""), "{error}");
        }
        let job = document(&format!(
            "{base}\n[[jobs]]\nname = \"b\"\ncommand = \"true\"\nenv = {{ GOODFLOWS_OUTPUT = \"f\" }}"
        ));
        assert!(check_remote_env(&job, &SpriteConfig::default()).is_err());
    }

    fn spool(dir: &Path, edit: impl FnOnce(&mut crate::reporting::Manifest)) {
        use crate::reporting::{Manifest, ManifestJob, ManifestPartition, ManifestState};
        let jobs: Vec<ManifestJob> = (1..=2)
            .map(|index| ManifestJob {
                name: "test".into(),
                dir: "test".into(),
                partition: Some(ManifestPartition { index, count: 2 }),
                status: Some("failed".into()),
                executed: true,
                collection: None,
            })
            .collect();
        let mut manifest = Manifest {
            version: crate::reporting::manifest_version(&jobs, false),
            state: ManifestState::Finalized,
            workflow_run_id: RUN.into(),
            project: "demo".into(),
            workflow: "ci".into(),
            environment: Some("prod".into()),
            url_env: "GOODFLOWS_URL".into(),
            token_env: "GOODFLOWS_API_KEY".into(),
            source: Default::default(),
            jobs,
            run_record: None,
        };
        edit(&mut manifest);
        manifest.version =
            crate::reporting::manifest_version(&manifest.jobs, manifest.run_record.is_some());
        crate::reporting::write_manifest(dir, &manifest).unwrap();
    }

    const RUN: &str = "0b0e2f55-6c1f-4f4d-9a59-3c3c3f0c1a10";
    const SPOOLED: &str = r#"
[reporting]
project = "demo"
[workflows.ci.settings]
[[workflows.ci.jobs]]
name = "test"
command = "false"
reports = ["test-timings"]
partitions = 2
[environments.prod.settings]
"#;

    #[test]
    fn automatic_publication_requires_a_spool_bound_to_this_invocation() {
        let document = document(SPOOLED);
        let (_, selected) = document.workflow(Some("ci"), Some("prod")).unwrap();
        let reporting = selected.reporting.clone().unwrap();
        let check = |edit: &dyn Fn(&mut crate::reporting::Manifest)| {
            let dir = tempfile::tempdir().unwrap();
            spool(dir.path(), edit);
            trusted_spool(dir.path(), RUN, &reporting, "ci", Some("prod"), &selected)
                .map_err(|e| e.to_string())
        };
        check(&|_| {}).unwrap();
        // A run record is published only under a trusted `history = true`.
        let run_record = |m: &mut crate::reporting::Manifest| {
            m.run_record = Some(crate::reporting::ManifestRunRecord { schema_version: 1 })
        };
        let error = check(&run_record).unwrap_err();
        assert!(error.contains("[reporting] history = true"), "{error}");
        let mut opted_in = reporting.clone();
        opted_in.history = true;
        let dir = tempfile::tempdir().unwrap();
        spool(dir.path(), run_record);
        trusted_spool(dir.path(), RUN, &opted_in, "ci", Some("prod"), &selected).unwrap();
        // Opting in does not relax any other check.
        let dir = tempfile::tempdir().unwrap();
        spool(dir.path(), |m| {
            run_record(m);
            m.token_env = "OTHER_KEY".into();
        });
        let error = trusted_spool(dir.path(), RUN, &opted_in, "ci", Some("prod"), &selected)
            .unwrap_err()
            .to_string();
        assert!(error.contains("credential variables"), "{error}");
        for (edit, message) in [
            (
                Box::new(|m: &mut crate::reporting::Manifest| m.token_env = "OTHER_KEY".into())
                    as Box<dyn Fn(&mut crate::reporting::Manifest)>,
                "credential variables",
            ),
            (
                Box::new(|m| m.url_env = "OTHER_URL".into()),
                "credential variables",
            ),
            (Box::new(|m| m.environment = None), "scope"),
            (Box::new(|m| m.workflow = "other".into()), "scope"),
            (Box::new(|m| m.project = "elsewhere".into()), "scope"),
            (
                Box::new(|m| m.workflow_run_id = "1b0e2f55-6c1f-4f4d-9a59-3c3c3f0c1a10".into()),
                "different run",
            ),
            (
                Box::new(|m| {
                    for job in &mut m.jobs {
                        job.partition = job.partition.map(|mut p| {
                            p.count = 3;
                            p
                        });
                    }
                }),
                "partition scope",
            ),
            (
                Box::new(|m| {
                    for job in &mut m.jobs {
                        job.name = "unselected".into();
                        job.dir = "unselected".into();
                    }
                }),
                "not a selected reporting job",
            ),
        ] {
            let error = check(&*edit).unwrap_err();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn only_explicit_client_errors_are_definite_delete_rejections() {
        for refused in [
            "Error: failed to destroy sprite: API error (status 409)",
            "API error (status 403): forbidden",
            "API error (status 400)",
        ] {
            assert!(definite_rejection(refused), "{refused}");
        }
        for unknown in [
            "Error: destroy failed: API error (status 500)",
            "API error (status 502)",
            "API error (status 408)",
            "Error: websocket: close 1006 (abnormal closure)",
            "Error: context deadline exceeded",
            "status 4",
            "",
        ] {
            assert!(!definite_rejection(unknown), "{unknown}");
        }
    }

    #[test]
    fn pass_env_distinguishes_unset_from_empty() {
        std::env::set_var("GF_SPRITE_TEST_EMPTY", "");
        let values = resolve_pass_env(&["GF_SPRITE_TEST_EMPTY".into()]).unwrap();
        assert_eq!(values["GF_SPRITE_TEST_EMPTY"], "");
        let error = resolve_pass_env(&["GF_SPRITE_TEST_UNSET_X".into()])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "[sprite] pass_env variable GF_SPRITE_TEST_UNSET_X is not set"
        );
    }

    #[test]
    fn status_text_escapes_every_sprite_value_and_breaks_lines() {
        let bad = "x\x1b[2J\r\n\0\u{9b}y";
        let status = json!({"prepared": {}, "active_run_id": bad, "runs": [
            {"run_id": bad, "phase": bad, "workflow": bad}]});
        let esc = r"x\u{1b}[2J\r\n\u{0}\u{9b}y";
        assert_eq!(
            status_text(bad, bad, &status),
            format!(
                "Sprite {esc} (identity {esc})\n  prepared: yes\n  active run: {esc}\n  {esc}  {esc}  {esc}"
            )
        );
    }
}
