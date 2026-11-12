//! Sprite execution through the public CLI against a fake Sprite CLI
//! (tests/fixtures/fake-sprite). Each fake Sprite is a local directory; no
//! network or real Sprite is involved. Linux-only: the remote side relies on
//! /proc for process identity.
#![cfg(target_os = "linux")]

mod common;

use common::{stderr, stdout};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SENTINEL: &str = "sentinel-secret-7f3a9c";

struct Fake {
    root: PathBuf,
    project: PathBuf,
    api: Arc<Mutex<Vec<String>>>,
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

impl Fake {
    fn new(config: &str) -> Self {
        // Short paths: the supervisor's Unix socket lives under the fake HOME.
        let root = PathBuf::from(format!(
            "/tmp/gfs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["bin", "cli", "s", "p", "pause"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-sprite");
        std::fs::copy(fixtures.join("sprite"), root.join("cli/sprite")).unwrap();
        std::fs::copy(fixtures.join("sprite-env"), root.join("bin/sprite-env")).unwrap();
        let api = Arc::new(Mutex::new(Vec::new()));
        let listener = UnixListener::bind(root.join("api.sock")).unwrap();
        let log = api.clone();
        let api_root = root.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let request = first.trim().to_owned();
                // Final-release ordering: what is durable when the hold goes.
                if let Some(run) = request
                    .strip_prefix("DELETE /v1/tasks/gf-")
                    .and_then(|rest| rest.split_whitespace().next())
                {
                    log.lock().unwrap().push(release_state(&api_root, run));
                }
                log.lock().unwrap().push(request);
                let reply: &[u8] = if api_root.join("api.fail").exists() {
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
                };
                let _ = stream.write_all(reply);
            }
        });
        let project = root.join("p");
        git(&project, &["init", "-q"]);
        std::fs::write(project.join("goodflows.toml"), config).unwrap();
        std::fs::write(project.join(".gitignore"), "target/\n.goodflows/\n").unwrap();
        git(&project, &["add", "."]);
        git(&project, &["commit", "-qm", "init"]);
        Fake { root, project, api }
    }

    fn command(&self) -> Command {
        self.command_with(Path::new(env!("CARGO_BIN_EXE_goodflows")))
    }

    /// The controller environment for another `goodflows` executable.
    fn command_with(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        let path = format!(
            "{}:{}",
            self.root.join("cli").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .current_dir(&self.project)
            .env("PATH", path)
            .env("FAKE_SPRITE_ROOT", &self.root)
            .env("NO_COLOR", "1")
            // Unarmed pause points cost nothing; see src/sprite/testhook.rs.
            .env("GOODFLOWS_TEST_PAUSE_DIR", self.root.join("pause"))
            .env("GOODFLOWS_TEST_OWNER_WAIT_SECS", "5")
            // A short follow reconnect budget keeps lost-connection tests fast.
            .env("GOODFLOWS_TEST_FOLLOW_BUDGET_SECS", "3")
            // A short post-delete listing budget keeps failed-destroy tests fast.
            .env("GOODFLOWS_TEST_DESTROY_POLL_SECS", "2")
            .stdin(Stdio::null());
        for name in [
            "SPRITE_TOKEN",
            "SPRITES_TOKEN",
            "SPRITE_ORG",
            "SPRITES_API_URL",
            "GOODFLOWS_URL",
            "GOODFLOWS_API_KEY",
            "GOODFLOWS_SPRITE_STATE_DIR",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn invoked(&self) -> bool {
        self.root.join("argv.log").exists()
    }

    fn argv_log(&self) -> String {
        std::fs::read_to_string(self.root.join("argv.log")).unwrap_or_default()
    }

    /// The single fake Sprite's HOME.
    fn home(&self) -> PathBuf {
        let mut homes: Vec<_> = std::fs::read_dir(self.root.join("s"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(homes.len(), 1, "{homes:?}");
        homes.pop().unwrap()
    }

    fn homes(&self) -> Vec<PathBuf> {
        let mut homes: Vec<_> = std::fs::read_dir(self.root.join("s"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        homes.sort();
        homes
    }

    /// The run's directory on whichever fake Sprite accepted it.
    fn run_dir(&self, run: &str) -> PathBuf {
        let homes = self.homes();
        homes
            .iter()
            .map(|h| h.join(".goodflows/runs").join(run))
            .find(|d| d.exists())
            .unwrap_or_else(|| homes[0].join(".goodflows/runs").join(run))
    }

    fn runs(&self) -> usize {
        self.homes()
            .iter()
            .map(|h| {
                std::fs::read_dir(h.join(".goodflows/runs"))
                    .map(|e| e.count())
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Arm a pause point (src/sprite/testhook.rs).
    fn arm(&self, point: &str) {
        std::fs::write(self.root.join("pause").join(format!("{point}.pause")), "").unwrap();
    }

    fn wait_reached(&self, point: &str) {
        let reached = self.root.join("pause").join(format!("{point}.reached"));
        wait_for(point, Duration::from_secs(60), || reached.exists());
    }

    fn release(&self, point: &str) {
        std::fs::remove_file(self.root.join("pause").join(format!("{point}.reached"))).unwrap();
    }

    fn env_run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = self.command();
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(args).output().unwrap()
    }

    fn argv_lines(&self) -> usize {
        self.argv_log().lines().count()
    }

    fn record(&self, run: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.run_dir(run).join("record.json")).unwrap())
            .unwrap()
    }

    /// The run's record once submission has written it; polling loops may
    /// see a run directory before its record exists.
    fn try_record(&self, run: &str) -> Option<Value> {
        let bytes = std::fs::read(self.run_dir(run).join("record.json")).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.project.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

/// `STATE <run> phase=<phase> active=<bool> lock=<free|held>` for the run's
/// Sprite at the moment its hold release arrives.
fn release_state(root: &Path, run: &str) -> String {
    let mut phase = "missing".to_owned();
    let mut active = false;
    let mut lock = "free";
    for home in std::fs::read_dir(root.join("s"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let goodflows = home.path().join(".goodflows");
        let Ok(record) = std::fs::read(goodflows.join("runs").join(run).join("record.json")) else {
            continue;
        };
        if let Ok(record) = serde_json::from_slice::<Value>(&record) {
            phase = record["phase"].as_str().unwrap_or("unknown").to_owned();
        }
        for caller in std::fs::read_dir(goodflows.join("callers"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let pointer =
                std::fs::read_to_string(caller.path().join("active.json")).unwrap_or_default();
            active |= pointer.contains(run);
            if let Ok(file) = std::fs::File::open(caller.path().join("lock")) {
                // SAFETY: a nonblocking probe on a descriptor owned here.
                let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
                if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                    lock = "held";
                }
            }
        }
    }
    format!("STATE {run} phase={phase} active={active} lock={lock}")
}

impl Drop for Fake {
    fn drop(&mut self) {
        // Stop every process the fake Sprites started (supervisors, workers, jobs).
        // `<root>/` exactly: `/tmp/gfs-1-1` is a prefix of `/tmp/gfs-1-12`.
        let needle = format!("{}/", self.root.to_string_lossy());
        for _ in 0..3 {
            for entry in std::fs::read_dir("/proc").unwrap().flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<i32>().ok())
                else {
                    continue;
                };
                if pid as u32 == std::process::id() {
                    continue;
                }
                let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
                if String::from_utf8_lossy(&environ).contains(&needle) {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn events(output: &Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("non-JSON stdout line {line:?}\n{}", stdout(output)))
        })
        .collect()
}

fn of_type<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["type"] == kind).collect()
}

fn run_id(events: &[Value]) -> String {
    of_type(events, "sprite_run")[0]["run_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn wait_for(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

const BASIC: &str = r#"
[settings]
sprite_prefix = "gft"

[sprite]
setup = "echo setup-ran; mkdir -p \"$GOODFLOWS_SPRITE_STATE_DIR\" && echo cache > \"$GOODFLOWS_SPRITE_STATE_DIR/cache\""
resume = "echo resume-ran"
pass_env = ["FORWARDED", "EMPTY_VALUE", "MODE"]

[settings.env]
MODE = "configured"

[[jobs]]
name = "check"
command = "test -f \"$GOODFLOWS_SPRITE_STATE_DIR/cache\" && printf '%s|%s|%s' \"$FORWARDED\" \"${EMPTY_VALUE-unset}\" \"$MODE\" > out.txt && cat out.txt"
"#;

fn forwarded(command: &mut Command) -> &mut Command {
    command
        .env("FORWARDED", format!("{SENTINEL} \"quoted\" $(x) ;\nline2"))
        .env("EMPTY_VALUE", "")
        .env("MODE", "caller")
        .env("PRIVATE_TOKEN", "never-forwarded-0123")
}

#[test]
fn local_runs_and_offline_inspection_never_invoke_the_sprite_cli() {
    let fake = Fake::new(BASIC);
    let output = forwarded(&mut fake.command()).output().unwrap();
    // The default local run executes locally (the state directory is unset, so the job fails).
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let output = fake.run(&["run", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    // Inspection is offline even with allowlisted variables unset.
    for args in [&["validate", "--json"][..], &["list", "--json"]] {
        let output = fake.run(args);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(!stdout(&output).contains(SENTINEL));
    }
    let list = events(&fake.run(&["list", "--json"]));
    assert_eq!(list[0]["type"], "jobs");
    assert!(!fake.invoked(), "{}", fake.argv_log());
}

#[test]
fn destroy_help_states_the_confirmation_and_trust_boundaries() {
    let fake = Fake::new(BASIC);
    let help = stdout(&fake.run(&["sprite", "destroy", "--help"]));
    for text in [
        "--yes",
        "confirmation",
        "lost permanently",
        "do not isolate hostile jobs",
    ] {
        assert!(help.contains(text), "{help}");
    }
    assert!(!fake.invoked());
}

#[test]
fn sprite_help_hides_run_only_flags_that_sprite_commands_reject() {
    let fake = Fake::new(BASIC);
    let run_only = [
        "--env",
        "--parallelism",
        "--fail-fast",
        "--jobs",
        "--log-dir",
        "--history",
        "--history-dir",
        "--log-runs",
        "--history-runs",
        "--history-bytes",
    ];
    for command in [
        &["sprite"][..],
        &["sprite", "status"],
        &["sprite", "logs"],
        &["sprite", "collect"],
        &["sprite", "cancel"],
        &["sprite", "destroy"],
    ] {
        let mut args = command.to_vec();
        args.push("--help");
        let output = fake.run(&args);
        assert!(output.status.success(), "{args:?}");
        let help = stdout(&output);
        for flag in run_only {
            assert!(!help.contains(flag), "{args:?} shows {flag}:\n{help}");
        }
        assert!(
            help.contains("--config") && help.contains("--json"),
            "{help}"
        );
    }
    // `run` still documents them, and a hidden flag is still rejected.
    let help = stdout(&fake.run(&["run", "--help"]));
    for flag in run_only {
        assert!(help.contains(flag), "run help lacks {flag}");
    }
    let output = fake.run(&["sprite", "status", "T-1", "--jobs", "x", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("--jobs cannot be used with sprite commands"));
    assert!(!fake.invoked());
}

#[test]
fn argument_and_preflight_errors_have_no_remote_side_effects() {
    let fake = Fake::new(BASIC);
    for (args, code) in [
        (&["run", "--sprite"][..], 2),
        (&["sprite", "logs", "T-1"], 2),
        (&["sprite", "logs", "T-1", "--run", "not-a-uuid"], 2),
        (&["sprite", "status"], 2),
        (&["sprite", "status", "T-1", "--jobs", "x"], 1),
        (&["sprite", "status", "T-1", "--env", "x"], 1),
    ] {
        let output = fake.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?} {}",
            stderr(&output)
        );
    }
    // Unset allowlisted variable: named, never valued.
    let output = fake.run(&["run", "--sprite", "T-1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("pass_env variable FORWARDED is not set"));
    // Denied credential forwarding and configured overrides, even unselected.
    // A reserved variable fails config validation (exit 2); Sprite's own
    // preflight checks still exit 1.
    for (config, name, code) in [
        (BASIC.replace("\"MODE\"]", "\"MODE\", \"SPRITE_TOKEN\"]"), "SPRITE_TOKEN", 1),
        (BASIC.replace("\"MODE\"]", "\"MODE\", \"SPRITES_TOKEN\"]"), "SPRITES_TOKEN", 1),
        (
            format!("{BASIC}\n[environments.unused.settings.env]\nGOODFLOWS_SPRITE_STATE_DIR = \"x\"\n"),
            "GOODFLOWS_SPRITE_STATE_DIR",
            2,
        ),
        (
            format!("{BASIC}\n[workflows.other.settings.env]\nSPRITE_TOKEN = \"{SENTINEL}\"\n[[workflows.other.jobs]]\nname = \"x\"\ncommand = \"true\"\n"),
            "SPRITE_TOKEN",
            1,
        ),
    ] {
        fake.write("goodflows.toml", &config);
        let output = forwarded(&mut fake.command())
            .env("SPRITE_TOKEN", SENTINEL)
            .args(["run", "--sprite", "T-1", "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code), "{}", stdout(&output));
        let text = format!("{}{}", stdout(&output), stderr(&output));
        assert!(text.contains(name), "{text}");
        assert!(!text.contains(SENTINEL), "{text}");
        events(&output);
    }
    // No prefix, or no explicit hooks (a discoverable .agents/setup is ignored).
    fake.write(".agents/setup", "#!/bin/sh\necho discovered\n");
    for config in [
        BASIC.replace("sprite_prefix = \"gft\"", ""),
        BASIC.replace("setup = ", "# setup = "),
        BASIC.replace("resume = ", "# resume = "),
    ] {
        fake.write("goodflows.toml", &config);
        let output = forwarded(&mut fake.command())
            .args(["run", "--sprite", "T-1"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    }
    // A workflow/profile prefix override is a config error everywhere.
    fake.write(
        "goodflows.toml",
        &format!("{BASIC}\n[environments.e.settings]\nsprite_prefix = \"other\"\n"),
    );
    let output = fake.run(&["validate"]);
    assert!(stderr(&output).contains("sprite_prefix is project-wide"));
    assert!(!fake.invoked(), "{}", fake.argv_log());
}

#[test]
fn snapshot_preflight_rejects_unsafe_sources_without_remote_calls() {
    let fake = Fake::new(BASIC);
    std::os::unix::fs::symlink("/etc/hostname", fake.project.join("outside-link")).unwrap();
    let output = forwarded(&mut fake.command())
        .args(["run", "--sprite", "T-1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("points outside the source root"),
        "{}",
        stderr(&output)
    );
    std::fs::remove_file(fake.project.join("outside-link")).unwrap();
    // An outside-root workdir in an unselected profile fails before side effects.
    let outside = fake.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    fake.write(
        "goodflows.toml",
        &format!(
            "{BASIC}\n[environments.far.settings]\nworkdir = \"{}\"\n",
            outside.display()
        ),
    );
    let output = forwarded(&mut fake.command())
        .args(["run", "--sprite", "T-1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("outside the Git root"),
        "{}",
        stderr(&output)
    );
    assert!(!fake.invoked());
}

#[test]
fn remote_run_snapshots_prepares_executes_and_collects() {
    let fake = Fake::new(BASIC);
    // Working-tree state: an edited tracked file, a deleted one, a new
    // untracked file, an ignored output, a credential file and an odd name.
    fake.write("tracked.txt", "head\n");
    fake.write("deleted.txt", "gone\n");
    fake.write(".env", &format!("TOKEN={SENTINEL}\n"));
    git(&fake.project, &["add", "-f", "."]);
    git(&fake.project, &["commit", "-qm", "files"]);
    fake.write("tracked.txt", "edited\n");
    std::fs::remove_file(fake.project.join("deleted.txt")).unwrap();
    fake.write("new dir/untracked file.txt", "new\n");
    fake.write("target/ignored.bin", "ignored\n");
    let output = forwarded(&mut fake.command())
        .args([
            "run",
            "--sprite",
            "T-Mixed.Case/1",
            "--json",
            "--log-dir",
            "logs",
        ])
        .output()
        .unwrap();
    let all = events(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let run = run_id(&all);
    // Summary last, lifecycle events separate, hook output structured.
    assert_eq!(all.last().unwrap()["type"], "summary");
    assert_eq!(all.last().unwrap()["run_id"], run.as_str());
    let hooks = of_type(&all, "sprite_hook_output");
    assert!(hooks
        .iter()
        .any(|e| e["hook"] == "setup" && e["data"] == "setup-ran\n"));
    assert!(!hooks.iter().any(|e| e["hook"] == "resume"));
    let excluded = &of_type(&all, "sprite_snapshot")[0]["excluded"];
    assert!(excluded.to_string().contains(".env"), "{excluded}");
    // Forwarded values are exact; configured env overrides forwarding for jobs.
    let job_output: String = of_type(&all, "job_output")
        .iter()
        .map(|e| e["data"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        job_output,
        format!("{SENTINEL} \"quoted\" $(x) ;\nline2||configured")
    );
    // The remote source is exactly the working tree.
    let source = fake.run_dir(&run).join("source");
    assert_eq!(
        std::fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "edited\n"
    );
    assert!(source.join("new dir/untracked file.txt").exists());
    assert!(!source.join("deleted.txt").exists());
    assert!(!source.join("target").exists());
    assert!(!source.join(".env").exists());
    assert!(!source.join(".git").exists());
    // Collected bundle, with remote managed paths left untouched in events.
    let summary = all.last().unwrap();
    assert!(summary["log_dir"]
        .as_str()
        .unwrap()
        .contains(".goodflows/runs/"));
    let collected = &of_type(&all, "sprite_collected")[0];
    assert_eq!(collected["complete"], true);
    let bundle = PathBuf::from(collected["bundle"].as_str().unwrap());
    assert!(bundle.starts_with(fake.project.join(".goodflows/sprites")));
    assert!(bundle.join("record.json").exists() && bundle.join("hooks/setup.stdout").exists());
    // The collection manifest maps the summary's remote log_dir (left as is
    // in the saved events) to bundle-relative paths.
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(bundle.join("bundle.json")).unwrap()).unwrap();
    let remote_root = manifest["remote_root"].as_str().unwrap();
    assert_eq!(Path::new(remote_root), fake.run_dir(&run));
    let log_dir = Path::new(summary["log_dir"].as_str().unwrap());
    let relative = log_dir.strip_prefix(remote_root).unwrap();
    assert!(relative.starts_with("results/logs"), "{relative:?}");
    let logs: Vec<&Value> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| Path::new(f["remote_path"].as_str().unwrap()).starts_with(log_dir))
        .collect();
    assert!(!logs.is_empty(), "{manifest}");
    for file in logs {
        let local = file["path"].as_str().unwrap();
        assert!(Path::new(local).starts_with(relative), "{local}");
        assert_eq!(
            Path::new(remote_root).join(local),
            Path::new(file["remote_path"].as_str().unwrap())
        );
        assert_eq!(
            sha256(&std::fs::read(bundle.join(local)).unwrap()),
            file["sha256"].as_str().unwrap()
        );
    }
    // No secret in transport argv, durable metadata or the bundle.
    assert!(!fake.argv_log().contains(SENTINEL));
    for name in ["record.json", "header.json", "events.jsonl"] {
        let text = std::fs::read_to_string(fake.run_dir(&run).join(name)).unwrap();
        assert!(!text.contains(SENTINEL), "{name}");
    }
    // Hold renewed and released after the terminal record.
    let release = format!("DELETE /v1/tasks/gf-{run}");
    wait_for("hold release", Duration::from_secs(10), || {
        fake.api
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with(&release))
    });
    let api = fake.api.lock().unwrap().clone();
    let put = api
        .iter()
        .position(|l| l.starts_with(&format!("PUT /v1/tasks/gf-{run}")))
        .unwrap();
    let delete = api.iter().position(|l| l.starts_with(&release)).unwrap();
    assert!(put < delete && api[delete + 1..].iter().all(|l| !l.contains(&run)));

    // A second run resumes against fresh source and keeps the state directory.
    fake.write("tracked.txt", "second\n");
    std::fs::remove_file(fake.project.join("new dir/untracked file.txt")).unwrap();
    let output = forwarded(&mut fake.command())
        .args(["run", "--sprite", "T-Mixed.Case/1", "--json"])
        .output()
        .unwrap();
    let second = events(&output);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let hooks = of_type(&second, "sprite_hook_output");
    assert!(hooks.iter().any(|e| e["hook"] == "resume"));
    assert!(!hooks.iter().any(|e| e["hook"] == "setup"));
    let second_source = fake.run_dir(&run_id(&second)).join("source");
    assert!(!second_source.join("new dir/untracked file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(second_source.join("tracked.txt")).unwrap(),
        "second\n"
    );
    // The first run's source is unchanged.
    assert_eq!(
        std::fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "edited\n"
    );
}

#[test]
fn failed_jobs_keep_the_sprite_in_text_rerun_hints() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "bad"
command = "exit 3"
"#,
    );
    let output = fake.run(&["run", "--sprite", "T example"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stdout(&output)
            .ends_with("Rerun failed: goodflows run default --sprite='T example' --jobs=bad\n"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn setup_failure_starts_no_jobs_and_is_retried() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "test -f ok || { echo nope >&2; exit 4; }"
resume = "echo resumed"
[[jobs]]
name = "job"
command = "touch job-ran"
"#,
    );
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let all = events(&output);
    let run = run_id(&all);
    assert!(of_type(&all, "summary").is_empty());
    assert_eq!(fake.record(&run)["phase"], "preparation_failed");
    assert!(!fake.run_dir(&run).join("source/job-ran").exists());
    assert!(!fake.home().join(".goodflows/prepared.json").exists());
    fake.write("ok", "");
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let all = events(&output);
    assert!(of_type(&all, "sprite_hook_output")
        .iter()
        .all(|e| e["hook"] == "setup"));
    assert!(fake.home().join(".goodflows/prepared.json").exists());
}

const SLOW: &str = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "slow"
command = "echo started >> \"$GOODFLOWS_SPRITE_STATE_DIR/../state-count\"; sleep 4; echo done"

[[workflows.other.jobs]]
name = "quick"
command = "true"
"#;

fn spawn_run(fake: &Fake, identity: &str) -> std::process::Child {
    fake.command()
        .args(["run", "--sprite", identity, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_running(fake: &Fake) -> String {
    let mut found = String::new();
    wait_for("a running remote run", Duration::from_secs(60), || {
        let Ok(entries) = std::fs::read_dir(fake.root.join("s")) else {
            return false;
        };
        for home in entries.flatten() {
            let runs = home.path().join(".goodflows/runs");
            for run in std::fs::read_dir(runs).into_iter().flatten().flatten() {
                let record = std::fs::read(run.path().join("record.json")).unwrap_or_default();
                if let Ok(record) = serde_json::from_slice::<Value>(&record) {
                    if record["phase"] == "running" {
                        found = record["run_id"].as_str().unwrap().to_owned();
                        return true;
                    }
                }
            }
        }
        false
    });
    found
}

#[test]
fn same_identity_contention_is_rejected_and_other_identities_proceed() {
    // The first run stays active until the test releases it, so contention
    // never depends on how quickly the contender reaches admission.
    let fake = Fake::new(&SLOW.replace(
        "sleep 4",
        r#"while [ ! -e \"$GOODFLOWS_SPRITE_STATE_DIR/../release-slow\" ]; do sleep 0.1; done"#,
    ));
    let first = spawn_run(&fake, "T-1");
    let active = wait_running(&fake);
    let runs_before = std::fs::read_dir(fake.home().join(".goodflows/runs"))
        .unwrap()
        .count();
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(stdout(&output).contains(&active), "{}", stdout(&output));
    assert_eq!(
        std::fs::read_dir(fake.home().join(".goodflows/runs"))
            .unwrap()
            .count(),
        runs_before,
        "the rejected run created no record"
    );
    // A different identity under the same prefix is independent.
    let output = fake.run(&["run", "other", "--sprite", "T-2", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let home = fake
        .homes()
        .into_iter()
        .find(|h| h.join(".goodflows/runs").join(&active).exists())
        .unwrap();
    std::fs::write(home.join(".goodflows/release-slow"), "").unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    // After durable release a fresh UUID runs.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

#[test]
fn controller_loss_does_not_stop_or_rerun_remote_work() {
    let fake = Fake::new(SLOW);
    let mut first = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    first.kill().unwrap();
    let _ = first.wait();
    // The identity stays busy while the worker lives.
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let status = &events(&output)[0];
    assert_eq!(status["busy"], true, "{status}");
    assert_eq!(status["active_run_id"], run.as_str());
    wait_for("remote completion", Duration::from_secs(30), || {
        fake.record(&run)["phase"] == "completed"
    });
    let output = fake.run(&["sprite", "logs", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success());
    assert!(events(&output).iter().any(|e| e["type"] == "summary"));
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let collected = &events(&output)[0];
    assert_eq!(collected["complete"], true);
    // Repeated identical final collection succeeds.
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success());
    assert_eq!(events(&output)[0]["already_collected"], true);
    // Exactly one execution of the job.
    let count = std::fs::read_to_string(fake.home().join(".goodflows/state-count")).unwrap();
    assert_eq!(count.lines().count(), 1);
}

#[test]
fn a_cancel_acknowledged_before_preparation_starts_no_hook_or_job() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "echo ran >> \"$GOODFLOWS_SPRITE_STATE_DIR/../setup-runs\""
resume = "echo ran >> \"$GOODFLOWS_SPRITE_STATE_DIR/../resume-runs\""
[[jobs]]
name = "check"
command = "echo ran >> \"$GOODFLOWS_SPRITE_STATE_DIR/../job-runs\""
"#,
    );
    // Cancel while the source is still being delivered.
    fake.arm("uploading");
    let attached = spawn_run(&fake, "T-1");
    fake.wait_reached("uploading");
    let runs = fake.home().join(".goodflows/runs");
    let entry = std::fs::read_dir(&runs).unwrap().next().unwrap().unwrap();
    let run = entry.file_name().into_string().unwrap();
    let output = fake.run(&["sprite", "cancel", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(events(&output)[0]["status"], "requested");
    fake.release("uploading");
    let attached = attached.wait_with_output().unwrap();
    assert_ne!(attached.status.code(), Some(0), "{}", text(&attached));
    let record = fake.record(&run);
    assert_eq!(record["phase"], "cancelled", "{record}");
    // Nothing started: no hook phase, no registered process, no output.
    assert!(record["hook"].is_null(), "{record}");
    assert_eq!(record["children"], serde_json::json!([]), "{record}");
    let phases: Vec<_> = of_type(&events(&attached), "sprite_phase")
        .iter()
        .map(|e| e["phase"].clone())
        .collect();
    assert!(
        !phases.iter().any(|p| p == "setup" || p == "running"),
        "{phases:?}"
    );
    let goodflows = fake.home().join(".goodflows");
    for file in ["setup-runs", "resume-runs", "job-runs", "prepared.json"] {
        assert!(!goodflows.join(file).exists(), "{file}");
    }
    // The Sprite is still unprepared: the next run runs setup, then the job.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(goodflows.join("setup-runs")).unwrap(),
        "ran\n"
    );
    assert!(!goodflows.join("resume-runs").exists());
    assert_eq!(
        std::fs::read_to_string(goodflows.join("job-runs")).unwrap(),
        "ran\n"
    );
}

#[test]
fn explicit_cancel_targets_only_the_named_run() {
    let fake = Fake::new(SLOW);
    let first = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    // An unrelated (unknown) run id is refused; the active run is untouched.
    let output = fake.run(&[
        "sprite",
        "cancel",
        "T-1",
        "--run",
        "00000000-0000-4000-8000-000000000000",
    ]);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(fake.record(&run)["phase"], "running");
    let output = fake.run(&["sprite", "cancel", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let first = first.wait_with_output().unwrap();
    assert_eq!(fake.record(&run)["phase"], "cancelled");
    assert_ne!(first.status.code(), Some(0));
    // Cancelling the finished run again signals nothing.
    let output = fake.run(&["sprite", "cancel", "T-1", "--run", &run, "--json"]);
    assert_eq!(events(&output)[0]["status"], "already_terminal");
    // Ctrl-C while the cancel call is in flight: exit 130, and the message
    // names the run without claiming the request was or was not delivered.
    std::fs::write(fake.root.join("hang-ops"), "cancel").unwrap();
    let before = fake.argv_log().matches(" cancel ").count();
    let child = fake
        .command()
        .args(["sprite", "cancel", "T-1", "--run", &run, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_call(&fake, " cancel ", before);
    interrupt(&child);
    let output = child.wait_with_output().unwrap();
    std::fs::remove_file(fake.root.join("hang-ops")).unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output));
    assert!(
        text(&output).contains(&format!(
            "the cancellation of run {run} may or may not have been delivered (check `goodflows sprite status T-1`)"
        )),
        "{}",
        text(&output)
    );
}

#[test]
fn confirmed_worker_loss_is_reconciled_without_replay() {
    let fake = Fake::new(SLOW);
    let mut first = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    // The job itself (not only the runner) must be running.
    wait_for("job start", Duration::from_secs(30), || {
        fake.home().join(".goodflows/state-count").exists()
    });
    first.kill().unwrap();
    let _ = first.wait();
    // Kill the worker and every recorded process group (simulated process loss).
    let record = fake.record(&run);
    let mut pids = vec![record["worker"]["pid"].as_i64().unwrap() as i32];
    for child in record["children"].as_array().unwrap() {
        pids.push(-(child["pgid"].as_i64().unwrap() as i32));
    }
    for pid in pids {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let worker = record["worker"]["pid"].as_i64().unwrap() as i32;
    wait_for("worker exit", Duration::from_secs(20), || {
        process_gone(worker)
    });
    // A surviving job descendant (its own process group, same session) keeps
    // the identity blocked: nothing is inferred from the worker's death alone.
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(events(&output)[0]["busy"], true);
    assert_eq!(fake.record(&run)["phase"], "running");
    // The worker leads the run's session; job descendants remain in it.
    let session = record["worker"]["pid"].as_i64().unwrap().to_string();
    let _ = Command::new("pkill")
        .args(["-KILL", "-s", &session])
        .status();
    wait_run_gone(&record);
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(events(&output)[0]["busy"], false);
    assert_eq!(fake.record(&run)["phase"], "interrupted");
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success());
    // A fresh UUID may now run; the interrupted UUID is never replayed.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fake.record(&run)["phase"], "interrupted");
}

#[test]
fn ownership_and_destruction_are_explicit() {
    let fake = Fake::new(SLOW.replace("sleep 4", "true").as_str());
    // An unowned resource with the computed name is never adopted.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_file(fake.home().join(".goodflows/owner.json")).unwrap();
    let output = fake.run(&["run", "other", "--sprite", "T-1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("refusing to adopt"),
        "{}",
        stderr(&output)
    );
    let output = fake.run(&["sprite", "destroy", "T-1", "--yes"]);
    assert_ne!(output.status.code(), Some(0));
    assert!(fake.root.join("names").join(&name).exists());
    // Restore ownership; confirmation is required without a terminal.
    let output = fake.run(&["sprite", "destroy", "T-2"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--yes"));
    let output = fake.run(&["run", "other", "--sprite", "T-2", "--json"]);
    assert!(output.status.success());
    let output = fake.run(&["sprite", "destroy", "T-2", "--yes", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(events(&output)[0]["type"], "sprite_destroyed");
    let output = fake.run(&["sprite", "status", "T-2", "--json"]);
    assert_eq!(events(&output)[0]["state"], "absent");
    // The unowned resource for T-1 is still there.
    assert!(fake.root.join("names").join(&name).exists());
}

#[test]
fn workflow_calls_run_on_the_sprite_exactly_as_locally() {
    use common::mock::Mock;
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
history = true
[sprite]
setup = "true"
resume = "true"
[reporting]
project = "demo"

[[workflows.ci.jobs]]
name = "prepare"
command = "true"
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
depends_on = ["prepare"]
[[workflows.ci.jobs]]
name = "publish"
command = "true"
depends_on = ["integration"]

[workflows.suite.settings]
workdir = "suite"
[[workflows.suite.jobs]]
name = "deeper"
workflow = "leaf"
[[workflows.suite.jobs]]
name = "test"
command = 'printf "%s|%s" "$(basename "$PWD")" "${GOODFLOWS_SPRITE_STATE_DIR:+state}"; cp report.jsonl "$GOODFLOWS_REPORT_DIR/r1.jsonl"; exit 1'
reports = ["test-timings"]
depends_on = ["deeper"]

[[workflows.leaf.jobs]]
name = "unit"
command = 'cp report.jsonl "$GOODFLOWS_REPORT_DIR/r2.jsonl"'
reports = ["test-timings"]
"#,
    );
    fake.write("suite/report.jsonl", &common::timing_report("r1", None));
    fake.write("report.jsonl", &common::timing_report("r2", None));
    let mock = Mock::store();
    let output = fake
        .command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SENTINEL)
        .args(["run", "ci", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let all = events(&output);
    let run = run_id(&all);
    // The child ran its complete graph in its own mapped workdir, with the
    // runner-owned state directory, under its own identity.
    let child_summary = of_type(&all, "summary")
        .into_iter()
        .find(|e| e["workflow"] == "suite")
        .unwrap_or_else(|| {
            panic!(
                "the child summary is forwarded: {all:#?}\n{}",
                stderr(&output)
            )
        })
        .clone();
    let child = child_summary["run_id"].as_str().unwrap().to_owned();
    assert_ne!(child, run);
    assert_eq!(child_summary["parent_run_id"], run.as_str());
    assert_eq!(child_summary["caller_job"], "integration");
    let job_output: String = of_type(&all, "job_output")
        .iter()
        .filter(|e| e["invocation_id"] == child.as_str())
        .map(|e| e["data"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(job_output, "suite|state");
    // Relayed child events keep their own workflow, not the root's.
    let owned: Vec<_> = all
        .iter()
        .filter(|e| e["invocation_id"] == child.as_str())
        .collect();
    assert!(
        owned.len() > 2 && owned.iter().all(|e| e["workflow"] == "suite"),
        "{owned:?}"
    );
    // The root summary is last and counts the call once, linked to its child.
    let root = all.last().unwrap();
    assert_eq!(root["type"], "summary");
    assert_eq!(root["run_id"], run.as_str());
    assert!(root["parent_run_id"].is_null());
    let results = root["results"].as_array().unwrap();
    let call = results.iter().find(|r| r["name"] == "integration").unwrap();
    assert_eq!(
        (&call["status"], &call["workflow"], &call["child_run_id"]),
        (&"failed".into(), &"suite".into(), &child.as_str().into())
    );
    let publish = results.iter().find(|r| r["name"] == "publish").unwrap();
    assert_eq!(publish["status"], "skipped");
    // The nested call: root -> suite -> leaf, each with its own summary.
    let leaf_summary = of_type(&all, "summary")
        .into_iter()
        .find(|e| e["workflow"] == "leaf")
        .unwrap()
        .clone();
    let leaf = leaf_summary["run_id"].as_str().unwrap().to_owned();
    assert_eq!(leaf_summary["parent_run_id"], child.as_str());
    assert_eq!(leaf_summary["caller_job"], "deeper");
    assert_eq!(of_type(&all, "summary").len(), 3);
    assert_eq!(
        of_type(&all, "summary")
            .iter()
            .filter(|e| e["run_id"] == run.as_str())
            .count(),
        1
    );
    // Each invocation keeps its own history; the child's report is published
    // by the controller under the child's identity, before the root summary.
    let history: Value = serde_json::from_slice(
        &std::fs::read(child_summary["history_file"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(history["parent_run_id"], run.as_str(), "{history}");
    // Both child spools are published, in summary arrival order (the leaf
    // finishes first), each under its own identity.
    let uploaded: Vec<(&str, &str)> = of_type(&all, "report_uploaded")
        .iter()
        .map(|e| (e["run_id"].as_str().unwrap(), e["job"].as_str().unwrap()))
        .collect();
    assert_eq!(
        uploaded,
        [(leaf.as_str(), "unit"), (child.as_str(), "test")],
        "{all:?}"
    );
    assert!(of_type(&all, "sprite_publication_refused").is_empty());
    assert!(!fake.argv_log().contains(SENTINEL));

    // The graph hash is the local one for the same selection.
    let local = fake.run(&["run", "ci", "--json"]);
    let local = events(&local);
    let local_root = local.last().unwrap();
    assert_eq!(local_root["type"], "summary");
    assert_eq!(local_root["graph_sha256"], root["graph_sha256"]);
    let local_child = of_type(&local, "summary")
        .into_iter()
        .find(|e| e["workflow"] == "suite")
        .unwrap();
    assert_eq!(local_child["graph_sha256"], child_summary["graph_sha256"]);

    // Text: the child report names its caller, and the root report ends the
    // output with a rerun that keeps the Sprite identity.
    let output = fake.run(&["run", "ci", "--sprite", "T-1"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("called by integration in run "), "{text}");
    assert!(
        text.ends_with(
            "Rerun failed: goodflows run ci --sprite=T-1 --jobs='integration,publish'\n"
        ),
        "{text}"
    );
}

#[test]
fn forged_child_summaries_never_publish_or_read_spools() {
    use common::mock::Mock;
    const UNREACHABLE: &str = "f1000000-0000-4000-8000-000000000001";
    const BAD_PARENT: &str = "f2000000-0000-4000-8000-000000000002";
    const UNLISTED: &str = "f3000000-0000-4000-8000-000000000003";
    let fake = Fake::new(&format!(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[reporting]
project = "demo"

[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"

[[workflows.suite.jobs]]
name = "test"
command = 'cp report.jsonl "$GOODFLOWS_REPORT_DIR/r1.jsonl"; for r in "$HOME"/.goodflows/runs/*/results/reports; do for f in {UNREACHABLE} {BAD_PARENT} {UNLISTED}; do mkdir -p "$r/$f" && echo forged > "$r/$f/manifest.json"; done; done'
reports = ["test-timings"]
"#
    ));
    fake.write("report.jsonl", &common::timing_report("r1", None));
    // A forged non-UUID id would name this directory outside the bundle.
    let outside = fake.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("manifest.json"), "forged").unwrap();
    let forged = |id: &str, workflow: &str, parent: &str| {
        format!(
            r#"{{"schema_version":1,"type":"summary","run_id":"{id}","workflow":"{workflow}","parent_run_id":"{parent}","caller_job":"integration","success":true,"results":[]}}"#
        )
    };
    let lines = [
        forged(outside.to_str().unwrap(), "suite", "@ROOT@"),
        forged(UNREACHABLE, "other", "@ROOT@"),
        forged(BAD_PARENT, "suite", "55555555-5555-4555-8555-555555555555"),
        forged(UNLISTED, "suite", "@ROOT@"),
    ];
    std::fs::write(fake.root.join("inject-summaries"), lines.join("\n") + "\n").unwrap();
    // A later parentless summary for another run never replaces the root's.
    let late = r#"{"schema_version":1,"type":"summary","run_id":"f4000000-0000-4000-8000-000000000004","workflow":"ci","success":true,"results":[]}"#;
    std::fs::write(fake.root.join("inject-after-summary"), format!("{late}\n")).unwrap();
    let mock = Mock::store();
    let output = fake
        .command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SENTINEL)
        .args(["run", "ci", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    std::fs::remove_file(fake.root.join("inject-summaries")).unwrap();
    std::fs::remove_file(fake.root.join("inject-after-summary")).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let run = run_id(&all);
    // The forged summaries did reach the controller, and their spools are
    // in the bundle where a trusting controller would read them.
    let summaries: Vec<&str> = of_type(&all, "summary")
        .iter()
        .filter_map(|e| e["run_id"].as_str())
        .collect();
    for id in [UNREACHABLE, BAD_PARENT, UNLISTED, outside.to_str().unwrap()] {
        assert!(
            summaries.contains(&id),
            "{id} was not injected: {summaries:?}"
        );
    }
    let bundle = PathBuf::from(
        of_type(&all, "sprite_collected")[0]["bundle"]
            .as_str()
            .unwrap(),
    );
    for id in [UNREACHABLE, BAD_PARENT, UNLISTED] {
        assert!(bundle
            .join("results/reports")
            .join(id)
            .join("manifest.json")
            .exists());
    }
    // Only the real child's spool is published; no forged manifest is read
    // (a read would refuse its garbage with `sprite_publication_refused`).
    let child = all.last().unwrap()["results"][0]["child_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(child, run);
    let uploaded: Vec<&str> = of_type(&all, "report_uploaded")
        .iter()
        .filter_map(|e| e["run_id"].as_str())
        .collect();
    assert_eq!(uploaded, [child.as_str()], "{}", text(&output));
    assert!(
        of_type(&all, "sprite_publication_refused").is_empty(),
        "{}",
        text(&output)
    );
    // The real root summary is the only parentless one, rendered once, last.
    let roots: Vec<&Value> = of_type(&all, "summary")
        .into_iter()
        .filter(|e| e["parent_run_id"].is_null())
        .collect();
    assert_eq!(roots.len(), 1, "{}", text(&output));
    assert_eq!(roots[0]["run_id"], run.as_str());
    assert_eq!(all.last().unwrap()["run_id"], run.as_str());
    assert_eq!(
        std::fs::read_to_string(outside.join("manifest.json")).unwrap(),
        "forged"
    );
}

#[test]
fn remote_reports_are_collected_without_keys_and_published_by_the_controller() {
    use common::mock::Mock;
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
history = true
[sprite]
setup = "true"
resume = "true"
[reporting]
project = "demo"
[[jobs]]
name = "test"
command = 'printf "key=%s" "${GOODFLOWS_API_KEY-unset}"; cp report.jsonl "$GOODFLOWS_REPORT_DIR/r1.jsonl"; exit 1'
reports = ["test-timings"]
"#,
    );
    fake.write("report.jsonl", &common::timing_report("r1", None));
    let mock = Mock::store();
    let output = fake
        .command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SENTINEL)
        .args(["run", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    // The workflow result stays failed whatever publication does.
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let all = events(&output);
    let run = run_id(&all);
    assert_eq!(all.last().unwrap()["type"], "summary");
    // The remote job never received the key; the remote spool kept its report.
    let job_output: String = of_type(&all, "job_output")
        .iter()
        .map(|e| e["data"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(job_output, "key=unset");
    assert_eq!(of_type(&all, "reports_collected")[0]["reports"], 1);
    // Controller publication happened after collection and before the summary.
    let collected = all
        .iter()
        .position(|e| e["type"] == "sprite_collected")
        .unwrap();
    let uploaded = all
        .iter()
        .position(|e| e["type"] == "report_uploaded")
        .unwrap();
    assert!(collected < uploaded && uploaded < all.len() - 1);
    assert!(!mock.requests.lock().unwrap().is_empty());
    assert!(!fake.argv_log().contains(SENTINEL));
    // Remote history carries the snapshot's provenance, as the control record
    // does; the untracked report fixture shipped, so the archive is dirty.
    let history = all.last().unwrap()["history_file"]
        .as_str()
        .unwrap()
        .to_owned();
    let history: Value = serde_json::from_slice(&std::fs::read(&history).unwrap()).unwrap();
    let provenance = fake.record(&run)["provenance"].clone();
    assert!(provenance["commit"].is_string());
    assert_eq!(
        history["source"],
        serde_json::json!({"commit": provenance["commit"], "dirty": true, "branch": provenance["branch"]}),
        "{history}"
    );

    // Without controller credentials publication is skipped; the result is unchanged.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let again = events(&output);
    let collected = again
        .iter()
        .position(|e| e["type"] == "sprite_collected")
        .unwrap();
    assert!(again[collected..]
        .iter()
        .any(|e| e["type"] == "reporting_skipped" && e["reason"] == "credentials_unavailable"));
    assert!(of_type(&again, "report_uploaded").is_empty());

    // Explicit collection never publishes; the collected spool replays
    // through `reports upload` without any checkout or remote secret.
    let target = fake.root.join("collected");
    let before = mock.requests.lock().unwrap().len();
    let output = fake.run(&[
        "sprite",
        "collect",
        "T-1",
        "--run",
        &run,
        "--output",
        target.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(mock.requests.lock().unwrap().len(), before);
    let spool = target.join("result/results/reports").join(&run);
    let raw_events = std::fs::read(target.join("result/workflow.jsonl")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_goodflows"))
        .current_dir(outside.path())
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SENTINEL)
        .args(["reports", "upload", "--json"])
        .arg(&spool)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        stdout(&output),
        stderr(&output)
    );
    let summary = &events(&output).last().unwrap().clone();
    assert_eq!(summary["type"], "reports_upload_summary");
    assert_eq!(summary["pending"], 0, "{summary}");
    // Saved native event bytes are unchanged by collection and replay.
    assert_eq!(
        std::fs::read(target.join("result/workflow.jsonl")).unwrap(),
        raw_events
    );
    assert_eq!(
        std::fs::read(fake.run_dir(&run).join("workflow.jsonl")).unwrap(),
        raw_events
    );
}

const PLAIN: &str = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "ok"
command = "true"
"#;

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn snapshot_covers_git_states_odd_paths_and_an_independent_manifest() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::new(PLAIN);
    fake.write("staged.txt", "v1\n");
    fake.write("old-name.txt", "renamed content\n");
    fake.write("tool.sh", "#!/bin/sh\necho hi\n");
    std::fs::set_permissions(
        fake.project.join("tool.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git(&fake.project, &["add", "."]);
    git(&fake.project, &["commit", "-qm", "files"]);
    fake.write("staged.txt", "v2 staged\n");
    git(&fake.project, &["add", "staged.txt"]);
    git(&fake.project, &["mv", "old-name.txt", "new name.txt"]);
    let odd = fake
        .project
        .join(std::ffi::OsStr::from_bytes(b"bad\xffname.txt"));
    std::fs::write(&odd, "odd\n").unwrap();
    std::os::unix::fs::symlink("tool.sh", fake.project.join("tool-link")).unwrap();
    fake.write("notes/has-secret.txt", &format!("token: {SENTINEL}\n"));
    let output = fake
        .command()
        .env("SPRITES_TOKEN", SENTINEL)
        .args(["run", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let all = events(&output);
    let run = run_id(&all);
    let excluded = of_type(&all, "sprite_snapshot")[0]["excluded"].to_string();
    assert!(excluded.contains("notes/has-secret.txt"), "{excluded}");
    assert!(!excluded.contains(SENTINEL));
    let source = fake.run_dir(&run).join("source");
    assert_eq!(
        std::fs::read_to_string(source.join("staged.txt")).unwrap(),
        "v2 staged\n"
    );
    assert!(source.join("new name.txt").exists() && !source.join("old-name.txt").exists());
    assert_eq!(
        std::fs::read(source.join(std::ffi::OsStr::from_bytes(b"bad\xffname.txt"))).unwrap(),
        b"odd\n"
    );
    // The internal link is a regular copy of its target.
    let link = std::fs::symlink_metadata(source.join("tool-link")).unwrap();
    assert!(link.is_file());
    assert_eq!(link.permissions().mode() & 0o777, 0o755);
    assert!(!source.join("notes/has-secret.txt").exists());
    // The manifest matches content and modes computed here from the working tree.
    let header: Value =
        serde_json::from_slice(&std::fs::read(fake.run_dir(&run).join("header.json")).unwrap())
            .unwrap();
    let manifest = header["manifest"].as_array().unwrap();
    for entry in manifest {
        let path = entry["path"].as_str().unwrap();
        let local = if path == "bad%FFname.txt" {
            odd.clone()
        } else {
            fake.project.join(path)
        };
        let bytes = std::fs::read(&local).unwrap();
        assert_eq!(entry["sha256"], sha256(&bytes), "{path}");
        assert_eq!(entry["size"], bytes.len(), "{path}");
        let executable = std::fs::metadata(&local).unwrap().permissions().mode() & 0o111 != 0;
        assert_eq!(entry["executable"], executable, "{path}");
    }
    let mut names: Vec<_> = manifest
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".gitignore",
            "bad%FFname.txt",
            "goodflows.toml",
            "new name.txt",
            "staged.txt",
            "tool-link",
            "tool.sh"
        ]
    );
    assert!(!fake.argv_log().contains(SENTINEL));
}

#[test]
fn a_detached_snapshot_records_no_branch() {
    let fake = Fake::new(PLAIN);
    git(&fake.project, &["checkout", "-q", "--detach"]);
    let output = fake.run(&["run", "--sprite", "T-1", "--json", "--history"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let all = events(&output);
    let provenance = fake.record(&run_id(&all))["provenance"].clone();
    assert!(provenance["branch"].is_null(), "{provenance}");
    let path = all.last().unwrap()["history_file"].as_str().unwrap();
    let history: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    // No branch at all, never a fallback such as `main`.
    assert_eq!(
        history["source"],
        serde_json::json!({"commit": provenance["commit"], "dirty": false}),
        "{history}"
    );
}

#[test]
fn unsupported_source_entries_fail_before_any_remote_call() {
    type Prepare = Box<dyn Fn(&Fake)>;
    let cases: Vec<(&str, Prepare, &str)> = vec![
        (
            "cyclic link",
            Box::new(|f: &Fake| {
                std::os::unix::fs::symlink("loop-b", f.project.join("loop-a")).unwrap();
                std::os::unix::fs::symlink("loop-a", f.project.join("loop-b")).unwrap();
            }),
            "unresolved or cyclic",
        ),
        (
            "fifo",
            Box::new(|f: &Fake| {
                // Git lists no untracked special files; a tracked path replaced by one is caught.
                f.write("pipe", "was a file\n");
                git(&f.project, &["add", "pipe"]);
                git(&f.project, &["commit", "-qm", "pipe"]);
                std::fs::remove_file(f.project.join("pipe")).unwrap();
                let path =
                    std::ffi::CString::new(f.project.join("pipe").to_str().unwrap()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }),
            "special file",
        ),
        (
            "submodule",
            Box::new(|f: &Fake| {
                let sub = f.root.join("sub");
                std::fs::create_dir_all(&sub).unwrap();
                git(&sub, &["init", "-q"]);
                std::fs::write(sub.join("x"), "x").unwrap();
                git(&sub, &["add", "."]);
                git(&sub, &["commit", "-qm", "x"]);
                git(
                    &f.project,
                    &[
                        "-c",
                        "protocol.file.allow=always",
                        "submodule",
                        "add",
                        "-q",
                        sub.to_str().unwrap(),
                        "vendored",
                    ],
                );
            }),
            "submodule vendored is not supported",
        ),
        (
            "excluded hook script",
            Box::new(|f: &Fake| {
                f.write(
                    "goodflows.toml",
                    &PLAIN.replace("setup = \"true\"", "setup = \"bash scripts/secrets.sh\""),
                );
                f.write("scripts/secrets.sh", "true\n");
            }),
            "is not part of the snapshot",
        ),
        (
            "hook script outside the source root",
            Box::new(|f: &Fake| {
                f.write(
                    "goodflows.toml",
                    &PLAIN.replace("setup = \"true\"", "setup = \"bash ./x/../../outside.sh\""),
                );
                std::fs::write(f.root.join("outside.sh"), "true\n").unwrap();
            }),
            "uses ./x/../../outside.sh, which is outside the source root",
        ),
    ];
    for (name, prepare, message) in cases {
        let fake = Fake::new(PLAIN);
        prepare(&fake);
        let output = fake.run(&["run", "--sprite", "T-1"]);
        assert_eq!(output.status.code(), Some(1), "{name}: {}", stderr(&output));
        assert!(
            stderr(&output).contains(message),
            "{name}: {}",
            stderr(&output)
        );
        assert!(!fake.invoked(), "{name}");
    }
}

#[test]
fn hook_paths_are_normalized_before_snapshot_membership() {
    // The hook runs in the nested workdir `sub`: `./` and `../` paths name
    // snapshot files once normalized against it.
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
workdir = "sub"
[sprite]
setup = "bash ./setup.sh && bash ../scripts/shared.sh"
resume = "true"
[[jobs]]
name = "check"
command = "test -f \"$GOODFLOWS_SPRITE_STATE_DIR/../local-ran\" && test -f \"$GOODFLOWS_SPRITE_STATE_DIR/../shared-ran\""
"#,
    );
    fake.write(
        "sub/setup.sh",
        "touch \"$GOODFLOWS_SPRITE_STATE_DIR/../local-ran\"\n",
    );
    fake.write(
        "scripts/shared.sh",
        "touch \"$GOODFLOWS_SPRITE_STATE_DIR/../shared-ran\"\n",
    );
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let goodflows = fake.home().join(".goodflows");
    assert!(goodflows.join("local-ran").exists());
    assert!(goodflows.join("shared-ran").exists());
}

#[test]
fn missing_sprite_cli_and_step_level_credentials_fail_preflight() {
    let fake = Fake::new(PLAIN);
    let output = fake
        .command()
        .env("PATH", "/usr/bin:/bin")
        .args(["run", "--sprite", "T-1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("Sprite CLI (`sprite`) is not installed"),
        "{}",
        stderr(&output)
    );
    fake.write(
        "goodflows.toml",
        &format!("{PLAIN}\n[[jobs]]\nname = \"s\"\n[[jobs.steps]]\nid = \"x\"\ncommand = \"true\"\nenv = {{ SPRITES_TOKEN = \"{SENTINEL}\" }}\n"),
    );
    let output = fake.run(&["run", "--sprite", "T-1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("step 'x' env must not set SPRITES_TOKEN"),
        "{}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains(SENTINEL));
    // The same config still runs locally (local rules are unchanged).
    assert!(fake.run(&["run", "--json"]).status.success());
    assert!(!fake.invoked());
}

#[test]
fn identities_and_prefixes_select_distinct_owned_sprites() {
    let fake = Fake::new(PLAIN);
    let mut names = Vec::new();
    for identity in ["T-a", "t-a", "T_a", "T a"] {
        let output = fake.run(&["run", "--sprite", identity, "--json"]);
        assert!(output.status.success(), "{}", stderr(&output));
        names.push(
            of_type(&events(&output), "sprite_run")[0]["sprite"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    fake.write("goodflows.toml", &PLAIN.replace("\"gft\"", "\"other\""));
    let output = fake.run(&["run", "--sprite", "T-a", "--json"]);
    names.push(
        of_type(&events(&output), "sprite_run")[0]["sprite"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
    assert!(names[4].starts_with("other-") && names[0].starts_with("gft-"));
    // A record for a different identity at this name is never adopted.
    fake.write("goodflows.toml", PLAIN);
    let home = fake.root.join("s").join(&names[0][names[0].len() - 8..]);
    let owner = home.join(".goodflows/owner.json");
    let text = std::fs::read_to_string(&owner)
        .unwrap()
        .replace("\"T-a\"", "\"T-b\"");
    std::fs::write(&owner, text).unwrap();
    for args in [
        &["run", "--sprite", "T-a"][..],
        &["sprite", "destroy", "T-a", "--yes"],
    ] {
        let output = fake.run(args);
        assert_ne!(output.status.code(), Some(0), "{args:?}");
        assert!(
            stderr(&output).contains("different GoodFlows scope"),
            "{}",
            stderr(&output)
        );
    }
    assert!(fake.root.join("names").join(&names[0]).exists());
}

#[test]
fn documented_status_example_matches_the_status_event() {
    let fake = Fake::new(PLAIN);
    assert!(fake.run(&["run", "--sprite", "T-1"]).status.success());
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success());
    let event = &events(&output)[0];
    let docs =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/sprites.md"))
            .unwrap();
    let example = docs
        .split("```json\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .unwrap()
        .replace("[...]", "[]");
    let example: Value = serde_json::from_str(&example).unwrap();
    let keys = |v: &Value| {
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(keys(&example), keys(event));
    for field in ["owner", "supervisor", "prepared"] {
        assert_eq!(keys(&example[field]), keys(&event[field]), "{field}");
    }
    assert_eq!(keys(&example["runs"][0]), keys(&event["runs"][0]));
    // Text mode is readable and names the identity.
    let text = stdout(&fake.run(&["sprite", "status", "T-1"]));
    assert!(
        text.contains("identity T-1") && text.contains("prepared: yes"),
        "{text}"
    );
}

/// Text-mode status puts each field on its own line, while control
/// characters in values read from the Sprite stay escaped.
#[test]
fn status_text_breaks_lines_and_escapes_sprite_values() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    let run = run_id(&events(&output));
    let mut record = fake.record(&run);
    record["workflow"] = Value::from("wf\x1b[2J\r\n\0\u{9b}x");
    let path = fake.run_dir(&run).join("record.json");
    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    let output = fake.run(&["sprite", "status", "T-1"]);
    assert!(output.status.success(), "{}", text(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("Sprite gft-") && lines[0].ends_with(" (identity T-1)"),
        "{text}"
    );
    let phase = record["phase"].as_str().unwrap();
    assert_eq!(
        lines[1..],
        [
            "  prepared: yes",
            "  active run: none",
            &format!(r"  {run}  {phase}  wf\u{{1b}}[2J\r\n\u{{0}}\u{{9b}}x"),
        ],
        "{text}"
    );
    assert!(!text.contains(['\x1b', '\r', '\0', '\u{9b}']), "{text:?}");
}

#[test]
fn nested_config_and_profile_workdirs_map_into_the_snapshot() {
    let fake = Fake::new(PLAIN);
    let config = r#"
[settings]
sprite_prefix = "gft"
workdir = ".."
[sprite]
setup = "true"
resume = "true"
[environments.app.settings]
workdir = "../app"
[[jobs]]
name = "where"
command = "printf '%s' \"${PWD##*/}\""
"#;
    fake.write("ci/goodflows.toml", config);
    fake.write("app/README", "app\n");
    let output = fake.run(&[
        "run",
        "--config",
        "ci/goodflows.toml",
        "--env",
        "app",
        "--sprite",
        "T-1",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "job_output")[0]["data"], "app");
    let source = fake.run_dir(&run_id(&all)).join("source");
    // The original source config is delivered unchanged.
    assert_eq!(
        std::fs::read_to_string(source.join("ci/goodflows.toml")).unwrap(),
        config
    );
    let output = fake.run(&[
        "run",
        "--config",
        "ci/goodflows.toml",
        "--sprite",
        "T-1",
        "--json",
    ]);
    assert_eq!(of_type(&events(&output), "job_output")[0]["data"], "source");
}

#[test]
fn a_workflows_only_config_with_a_relative_top_level_workdir_runs_remotely() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
workdir = "sub"
[sprite]
setup = "true"
resume = "true"
[workflows.check]
[[workflows.check.jobs]]
name = "where"
command = "printf '%s' \"${PWD##*/}\""
"#,
    );
    fake.write("sub/README", "sub\n");
    let local = fake.run(&["run", "check", "--json"]);
    assert_eq!(local.status.code(), Some(0), "{}", stderr(&local));
    assert_eq!(of_type(&events(&local), "job_output")[0]["data"], "sub");
    let output = fake.run(&["run", "check", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    // The workflow inherits the shared workdir, mapped into the snapshot.
    assert_eq!(of_type(&events(&output), "job_output")[0]["data"], "sub");
}

#[test]
fn remote_execution_keeps_graph_semantics_and_one_run_identity() {
    let config = r#"
[settings]
sprite_prefix = "gft"
history = true
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "detect"
outputs = { go = "steps.scan.outputs.go" }
[[jobs.steps]]
id = "scan"
command = "printf '{\"go\":true}' > \"$GOODFLOWS_OUTPUT\""
[[jobs]]
name = "part"
depends_on = ["detect"]
condition = { expression = "needs.detect.outputs.go" }
command = "echo \"p$GOODFLOWS_PARTITION/$GOODFLOWS_PARTITION_COUNT\""
partitions = 2
[[jobs]]
name = "skipme"
depends_on = ["detect"]
condition = { expression = "!needs.detect.outputs.go" }
command = "true"
[[jobs]]
name = "slow"
timeout_secs = 1
command = "sleep 5"
[[jobs]]
name = "after"
depends_on = ["slow"]
command = "true"
"#;
    let fake = Fake::new(config);
    let local = fake.run(&["run", "--json", "--no-fail-fast", "--no-history"]);
    let remote = fake.run(&[
        "run",
        "--sprite",
        "T-1",
        "--json",
        "--no-fail-fast",
        "--log-dir",
        "logs",
    ]);
    assert_eq!(local.status.code(), Some(1));
    assert_eq!(remote.status.code(), Some(1), "{}", stderr(&remote));
    let statuses = |output: &Output| {
        let summary = events(output).last().unwrap().clone();
        assert_eq!(summary["type"], "summary");
        let mut rows: Vec<String> = summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                format!(
                    "{} {} {}",
                    r["name"].as_str().unwrap(),
                    r["partition"],
                    r["status"].as_str().unwrap()
                )
            })
            .collect();
        rows.sort();
        (rows, summary)
    };
    let (local_rows, local_summary) = statuses(&local);
    let (remote_rows, summary) = statuses(&remote);
    assert_eq!(local_rows, remote_rows);
    assert_eq!(local_summary["graph_sha256"], summary["graph_sha256"]);
    assert!(
        remote_rows.contains(&"slow null timed_out".to_owned()),
        "{remote_rows:?}"
    );
    // One UUID: control record, summary, history and log directory.
    let all = events(&remote);
    let run = run_id(&all);
    assert_eq!(summary["run_id"], run.as_str());
    let history = summary["history_file"].as_str().unwrap();
    assert!(history.contains(&run) && Path::new(history).exists());
    assert!(summary["log_dir"].as_str().unwrap().contains(&run));
    // The saved native summary is relayed verbatim, except that the rerun
    // command keeps the caller's Sprite identity.
    let saved = std::fs::read_to_string(fake.run_dir(&run).join("workflow.jsonl")).unwrap();
    let saved: Value = serde_json::from_str(saved.lines().last().unwrap()).unwrap();
    let mut relayed = summary.clone();
    assert_eq!(relayed["workflow"], saved["workflow"]);
    assert_eq!(
        relayed["rerun_command"],
        "goodflows run default --sprite=T-1 --jobs='slow,after'"
    );
    assert_eq!(
        saved["rerun_command"],
        "goodflows run default --jobs='slow,after'"
    );
    for key in ["workflow", "rerun_command"] {
        relayed.as_object_mut().unwrap().remove(key);
    }
    let mut saved = saved;
    for key in ["workflow", "rerun_command"] {
        saved.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(relayed, saved);
    // Fail-fast (the default) stops launching after the timeout.
    let output = fake.run(&["run", "--sprite", "T-1", "--json", "--jobs", "slow,after"]);
    let (rows, _) = statuses(&output);
    assert_eq!(rows, ["after null skipped", "slow null timed_out"]);
}

#[test]
fn transport_failure_exits_3_with_pure_json_and_a_recovery_reference() {
    let fake = Fake::new(PLAIN);
    // A Sprite CLI that fails every call after printing noise.
    let broken = fake.root.join("cli/sprite");
    std::fs::write(
        &broken,
        "#!/bin/sh\necho noisy stdout\necho 'Error: connection reset' >&2\nexit 1\n",
    )
    .unwrap();
    // Without an organization the CLI cannot authenticate: a preflight error.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let output = fake
        .command()
        .env("SPRITE_ORG", "testorg")
        .args(["run", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let all = events(&output);
    assert!(of_type(&all, "summary").is_empty());
    let unresolved = of_type(&all, "sprite_unresolved");
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0]["recover"], "goodflows sprite status T-1");
    assert!(!stdout(&output).contains("noisy stdout"));
}

// ---------------------------------------------------------------------------
// Fault windows driven by pause points and fake-CLI fault injection.

fn spawn_args(fake: &Fake, args: &[&str], env: &[(&str, &str)]) -> std::process::Child {
    let mut command = fake.command();
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The first run (on any fake Sprite) whose record reaches `phase`.
fn wait_phase(fake: &Fake, phase: &str) -> String {
    let mut found = String::new();
    wait_for(phase, Duration::from_secs(60), || {
        for home in fake.homes() {
            for run in std::fs::read_dir(home.join(".goodflows/runs"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let record = std::fs::read(run.path().join("record.json")).unwrap_or_default();
                if let Ok(record) = serde_json::from_slice::<Value>(&record) {
                    if record["phase"] == phase {
                        found = record["run_id"].as_str().unwrap().to_owned();
                        return true;
                    }
                }
            }
        }
        false
    });
    found
}

/// The run id in a Sprite's (single) caller active pointer.
fn active_pointer(goodflows: &Path) -> Option<String> {
    for caller in std::fs::read_dir(goodflows.join("callers")).ok()?.flatten() {
        if let Ok(bytes) = std::fs::read(caller.path().join("active.json")) {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            return Some(value["run_id"].as_str().unwrap().to_owned());
        }
    }
    None
}

fn scope_hex(identity: &str) -> String {
    let scope =
        serde_json::json!({"organization": "testorg", "prefix": "gft", "identity": identity});
    serde_json::to_vec(&scope)
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Call the in-Sprite agent's `submit` directly with crafted frames, as the
/// fake transport's stdin would deliver them.
fn agent_submit(fake: &Fake, home: &Path, identity: &str, header: &Value, archive: &[u8]) -> Value {
    let mut input = Vec::new();
    for frame in [serde_json::to_vec(header).unwrap(), b"{}".to_vec()] {
        input.extend_from_slice(&(frame.len() as u64).to_be_bytes());
        input.extend_from_slice(&frame);
    }
    input.extend_from_slice(archive);
    let mut child = Command::new(env!("CARGO_BIN_EXE_goodflows"))
        .args(["__sprite-agent", "submit", &scope_hex(identity)])
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("GOODFLOWS_SPRITE_API_SOCKET", fake.root.join("api.sock"))
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(&input);
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let line = stdout(&output)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("{}", text(&output)))
}

fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    for (path, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn process_gone(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with(['Z', 'X'])),
    }
}

/// Wait until no live process remains in the run's worker or recorded
/// sessions/groups: the precondition of a confirmed loss.
fn wait_run_gone(record: &Value) {
    let worker = record["worker"]["pid"].as_i64().unwrap() as i32;
    let groups: Vec<String> = record["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pgid"].as_i64().unwrap().to_string())
        .chain([worker.to_string()])
        .collect();
    wait_for("killed processes to exit", Duration::from_secs(20), || {
        process_gone(worker)
            && std::fs::read_dir("/proc").unwrap().flatten().all(|entry| {
                let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
                let Some((_, rest)) = stat.rsplit_once(')') else {
                    return true;
                };
                let fields: Vec<&str> = rest.split_whitespace().collect();
                fields.first() == Some(&"Z")
                    || fields.len() < 4
                    || !groups.iter().any(|g| g == fields[2] || g == fields[3])
            })
    });
}

fn kill_group_from(pidfile: &Path) {
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[test]
fn a_file_changing_during_the_copy_fails_before_any_remote_call() {
    let fake = Fake::new(PLAIN);
    fake.write("busy.txt", "before\n");
    fake.arm("snapshot-copy-busy.txt");
    let child = spawn_args(&fake, &["run", "--sprite", "T-1", "--json"], &[]);
    fake.wait_reached("snapshot-copy-busy.txt");
    std::fs::write(
        fake.project.join("busy.txt"),
        "changed while it was copied\n",
    )
    .unwrap();
    fake.release("snapshot-copy-busy.txt");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("changed while it was being copied")
            && text(&output).contains("busy.txt"),
        "{}",
        text(&output)
    );
    assert!(!fake.invoked(), "{}", fake.argv_log());
    // Once the tree is quiet the same run succeeds with the new content.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    assert_eq!(
        std::fs::read_to_string(fake.run_dir(&run).join("source/busy.txt")).unwrap(),
        "changed while it was copied\n"
    );
}

#[test]
fn first_creation_races_admit_one_owner_and_never_adopt_unmarked_sprites() {
    let fake = Fake::new(SLOW);
    let a = spawn_run(&fake, "T-1");
    let b = spawn_run(&fake, "T-1");
    let outputs = [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()];
    let names: Vec<_> = std::fs::read_dir(fake.root.join("names"))
        .unwrap()
        .collect();
    assert_eq!(names.len(), 1, "exactly one Sprite was created");
    let home = fake.home();
    assert!(home.join(".goodflows/owner.json").exists());
    let successes = outputs
        .iter()
        .filter(|o| o.status.code() == Some(0))
        .count();
    assert!(
        successes >= 1,
        "{}",
        outputs.iter().map(text).collect::<String>()
    );
    for output in &outputs {
        match output.status.code() {
            Some(0) => {}
            Some(1) => assert!(
                text(output).contains("is busy with run"),
                "{}",
                text(output)
            ),
            other => panic!("unexpected exit {other:?}: {}", text(output)),
        }
    }
    // Only admitted runs created records and executed the job.
    assert_eq!(fake.runs(), successes);
    let count = std::fs::read_to_string(home.join(".goodflows/state-count")).unwrap();
    assert_eq!(count.lines().count(), successes);
    // Explicit targeting; the CLI's active context is never changed.
    for line in fake.argv_log().lines() {
        assert!(!line.starts_with("use"), "{line}");
        // (Multi-line shell arguments are logged as continuation lines.)
        let command = line.split_whitespace().next().unwrap_or_default();
        if ["exec", "create", "list", "destroy"].contains(&command) && !line.contains('$') {
            assert!(line.contains("-o testorg"), "{line}");
        }
    }

    // A lost create acknowledgement leaves an unmarked resource that is never adopted.
    let output = fake.env_run(
        &["run", "--sprite", "T-lost", "--json"],
        &[("FAKE_SPRITE_LOSE_CREATE_ACK", "1")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("refusing to adopt"),
        "{}",
        text(&output)
    );
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let lost = fake.root.join("s").join(&name[name.len() - 8..]);
    assert!(fake.root.join("names").join(&name).exists());
    assert!(!lost.join(".goodflows").exists(), "nothing was initialized");

    // A name collision: someone else's contents stay exactly as they were.
    std::fs::write(lost.join("precious.txt"), "user data\n").unwrap();
    let listing = |dir: &Path| {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.file_name(), std::fs::read(e.path()).unwrap_or_default())
            })
            .collect();
        entries.sort();
        entries
    };
    let before = listing(&lost);
    let output = fake.run(&["run", "--sprite", "T-lost", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("refusing to adopt"),
        "{}",
        text(&output)
    );
    let output = fake.run(&["sprite", "destroy", "T-lost", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(listing(&lost), before);
    assert!(fake.root.join("names").join(&name).exists());
}

const GATED: &str = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "while [ -e \"$GOODFLOWS_SPRITE_STATE_DIR/../hold-resume\" ]; do sleep 0.1; done"
[[jobs]]
name = "count"
command = "echo started >> \"$GOODFLOWS_SPRITE_STATE_DIR/../state-count\""
[[workflows.other.jobs]]
name = "quick"
command = "true"
[environments.p.settings]
parallelism = 1
"#;

#[test]
fn different_uuids_contend_through_upload_preparation_and_finalization() {
    let fake = Fake::new(GATED);
    fake.write("other.toml", &PLAIN.replace("\"gft\"", "\"gfu\""));
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let goodflows = fake.run_dir(&run_id(&events(&output))).join("../..");
    let contend = |fake: &Fake, active: &str| {
        let runs = fake.runs();
        for args in [
            &["run", "other", "--env", "p", "--sprite", "T-1", "--json"][..],
            &["run", "--sprite", "T-1", "--json"],
        ] {
            let output = fake.run(args);
            assert_eq!(output.status.code(), Some(1), "{}", text(&output));
            assert!(
                text(&output).contains(&format!("busy with run {active}")),
                "{}",
                text(&output)
            );
        }
        assert_eq!(
            fake.runs(),
            runs,
            "a rejected contender creates no work record"
        );
    };

    // Source delivery: the submitting process holds the identity.
    fake.arm("uploading");
    let first = spawn_run(&fake, "T-1");
    fake.wait_reached("uploading");
    let uploading = active_pointer(&goodflows).unwrap();
    assert_eq!(fake.record(&uploading)["phase"], "uploading");
    contend(&fake, &uploading);
    // Other identities and other project scopes are not serialized behind it.
    let output = fake.run(&["run", "other", "--sprite", "T-2", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let output = fake.run(&["run", "--config", "other.toml", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    fake.release("uploading");
    let first = first.wait_with_output().unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));

    // Preparation: the worker holds the identity while resume runs.
    std::fs::write(goodflows.join("hold-resume"), "").unwrap();
    let second = spawn_run(&fake, "T-1");
    let preparing = wait_phase(&fake, "resume");
    contend(&fake, &preparing);
    std::fs::remove_file(goodflows.join("hold-resume")).unwrap();
    let second = second.wait_with_output().unwrap();
    assert_eq!(second.status.code(), Some(0), "{}", text(&second));

    // Finalization: still held until the terminal record is durable.
    fake.arm("finalizing");
    let third = spawn_run(&fake, "T-1");
    fake.wait_reached("finalizing");
    let finalizing = active_pointer(&goodflows).unwrap();
    assert_eq!(fake.record(&finalizing)["phase"], "finalizing");
    contend(&fake, &finalizing);
    fake.release("finalizing");
    let third = third.wait_with_output().unwrap();
    assert_eq!(third.status.code(), Some(0), "{}", text(&third));
    assert_eq!(fake.record(&finalizing)["phase"], "completed");

    // Resubmitting a recorded UUID is only a lookup of the original record.
    let home = goodflows.join("..");
    let header: Value = serde_json::from_slice(
        &std::fs::read(fake.run_dir(&uploading).join("header.json")).unwrap(),
    )
    .unwrap();
    let before = std::fs::read(fake.run_dir(&uploading).join("record.json")).unwrap();
    let reply = agent_submit(&fake, &home, "T-1", &header, b"");
    assert_eq!(reply["status"], "existing", "{reply}");
    assert_eq!(reply["record"]["run_id"], uploading.as_str());
    assert_eq!(
        std::fs::read(fake.run_dir(&uploading).join("record.json")).unwrap(),
        before
    );
    // One execution per admitted UUID: setup run, then the three successors.
    let count = std::fs::read_to_string(goodflows.join("state-count")).unwrap();
    assert_eq!(count.lines().count(), 4);
    // A fresh UUID follows the durable release.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn a_contender_with_a_different_runner_build_gets_the_conflict_rejection() {
    let fake = Fake::new(&SLOW.replace(
        "sleep 4",
        r#"while [ ! -e \"$GOODFLOWS_SPRITE_STATE_DIR/../release-slow\" ]; do sleep 0.1; done"#,
    ));
    // Another build of the same version: same protocol, different runner digest
    // (as with a macOS controller's release runner versus a Linux dev build).
    let alt = fake.root.join("alt-goodflows");
    let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_goodflows")).unwrap();
    bytes.extend_from_slice(b"\0different-build");
    std::fs::write(&alt, bytes).unwrap();
    std::fs::set_permissions(&alt, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let first = spawn_run(&fake, "T-1");
    let active = wait_running(&fake);
    let runs = fake.runs();
    let output = fake
        .command_with(&alt)
        .args(["run", "other", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains(&format!("busy with run {active}"))
            && !text(&output).contains("a different runner version"),
        "{}",
        text(&output)
    );
    assert_eq!(
        fake.runs(),
        runs,
        "the rejected contender created no work record"
    );
    assert_eq!(fake.record(&active)["phase"], "running");
    let runners: Vec<_> = std::fs::read_dir(fake.home().join(".goodflows/runners"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(runners.len(), 1, "the rejected build installed no runner");

    // The accepted run is unchanged and finishes; then the other build runs.
    std::fs::write(fake.home().join(".goodflows/release-slow"), "").unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    let output = fake
        .command_with(&alt)
        .args(["run", "other", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let runners = std::fs::read_dir(fake.home().join(".goodflows/runners"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .count();
    assert_eq!(
        runners, 2,
        "the other build installs once the identity is free"
    );

    // With both runners installed, the contender passes provisioning and is
    // rejected by the supervisor check instead, with the same conflict.
    std::fs::remove_file(fake.home().join(".goodflows/release-slow")).unwrap();
    let first = spawn_run(&fake, "T-1");
    let mut active = String::new();
    wait_for("a new running run", Duration::from_secs(60), || {
        std::fs::read_dir(fake.home().join(".goodflows/runs"))
            .unwrap()
            .flatten()
            .any(|entry| {
                let id = entry.file_name().to_string_lossy().into_owned();
                let running = fake
                    .try_record(&id)
                    .is_some_and(|r| r["phase"] == "running");
                if running {
                    active = id;
                }
                running
            })
    });
    let runs = fake.runs();
    let output = fake
        .command_with(&alt)
        .args(["run", "other", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains(&format!("busy with run {active}"))
            && text(&output).contains("a different runner version"),
        "{}",
        text(&output)
    );
    assert_eq!(fake.runs(), runs);
    std::fs::write(fake.home().join(".goodflows/release-slow"), "").unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
}

/// Whether the (single) caller lock under `goodflows` can be taken right now.
/// A successful probe releases it at once.
fn caller_lock_free(goodflows: &Path) -> bool {
    let caller = std::fs::read_dir(goodflows.join("callers"))
        .unwrap()
        .flatten()
        .next()
        .unwrap();
    let file = std::fs::File::open(caller.path().join("lock")).unwrap();
    // SAFETY: a nonblocking probe on a descriptor owned here.
    unsafe {
        libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&file),
            libc::LOCK_EX | libc::LOCK_NB,
        ) == 0
    }
}

#[test]
fn the_caller_lock_is_never_free_from_admission_to_finalization() {
    let fake = Fake::new(PLAIN);
    // First use creates the Sprite and its caller directory.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let goodflows = fake.home().join(".goodflows");

    // Invariant probe: whenever the active pointer names a nonterminal run,
    // nobody else can take the caller lock. A probe that gets the lock while
    // the pointer and record still say "active" (re-read under the lock) has
    // found a gap.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probes = Arc::new(AtomicU64::new(0));
    let probe = {
        let (stop, probes, goodflows) = (stop.clone(), probes.clone(), goodflows.clone());
        std::thread::spawn(move || {
            let active = |goodflows: &Path| -> Option<String> {
                let run = active_pointer(goodflows)?;
                let record: Value = serde_json::from_slice(
                    &std::fs::read(goodflows.join("runs").join(&run).join("record.json")).ok()?,
                )
                .ok()?;
                let terminal = [
                    "completed",
                    "preparation_failed",
                    "transfer_failed",
                    "cancelled",
                    "interrupted",
                ];
                (!terminal.contains(&record["phase"].as_str()?))
                    .then(|| record["phase"].as_str().unwrap().to_owned())
            };
            let mut gaps = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                if active(&goodflows).is_some() {
                    probes.fetch_add(1, Ordering::SeqCst);
                    let caller = std::fs::read_dir(goodflows.join("callers"))
                        .unwrap()
                        .flatten()
                        .next()
                        .unwrap();
                    let file = std::fs::File::open(caller.path().join("lock")).unwrap();
                    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
                    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                        if let Some(phase) = active(&goodflows) {
                            gaps.push(phase);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            gaps
        })
    };

    // Every window from source delivery to durable finalization, including
    // the submit-to-worker handoff and the registered-before-GO window.
    let held = [
        "uploading",
        "handoff",
        "registered",
        "hold-created",
        "spawned-hook",
        "spawned-workflow",
        "finalizing",
        "terminal-written",
        "pointer-cleared",
    ];
    for point in held.iter().chain(&["unlocked"]) {
        fake.arm(point);
    }
    let run = spawn_run(&fake, "T-1");
    for point in held {
        fake.wait_reached(point);
        assert!(
            !caller_lock_free(&goodflows),
            "the caller lock was free at {point}"
        );
        fake.release(point);
    }
    // Only after the terminal record and pointer cleanup is it released.
    fake.wait_reached("unlocked");
    assert!(active_pointer(&goodflows).is_none());
    // The submitting process may still hold its reference while it reports
    // acceptance; it exits promptly.
    wait_for("caller lock release", Duration::from_secs(10), || {
        caller_lock_free(&goodflows)
    });
    fake.release("unlocked");
    let run = run.wait_with_output().unwrap();
    assert_eq!(run.status.code(), Some(0), "{}", text(&run));

    // Unpaused runs, back to back, under the same probe.
    for _ in 0..3 {
        let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    }
    stop.store(true, Ordering::SeqCst);
    let gaps = probe.join().unwrap();
    assert!(
        probes.load(Ordering::SeqCst) > 0,
        "the probe never observed a run"
    );
    assert!(
        gaps.is_empty(),
        "caller lock was free while active: {gaps:?}"
    );
}

#[test]
fn service_startup_reconciles_without_replay_and_reads_never_reconcile() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 300"));
    let mut controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    // The job itself (not only the runner) must be running.
    wait_for("job start", Duration::from_secs(30), || {
        fake.home().join(".goodflows/state-count").exists()
    });
    controller.kill().unwrap();
    let _ = controller.wait();
    let home = fake.home();
    let record = fake.record(&run);
    // Supervisor service loss plus worker loss; a job descendant survives.
    kill_group_from(&home.join(".service-goodflows-supervisor.pid"));
    let mut pids = vec![record["worker"]["pid"].as_i64().unwrap() as i32];
    for child in record["children"].as_array().unwrap() {
        pids.push(-(child["pgid"].as_i64().unwrap() as i32));
    }
    for pid in pids {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let worker = record["worker"]["pid"].as_i64().unwrap() as i32;
    wait_for("worker exit", Duration::from_secs(20), || {
        process_gone(worker)
    });
    let hooks: Vec<_> = std::fs::read_dir(fake.run_dir(&run).join("hooks"))
        .unwrap()
        .collect();
    let argv_before = fake.argv_lines();

    // Reads never reconcile.
    let output = fake.run(&["sprite", "logs", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(events(&output)[0]["complete"], false);
    assert_eq!(fake.record(&run)["phase"], "running");

    // An uncertain generation (a live descendant) blocks the service
    // replacement, so nothing new is admitted: the contender gets the
    // conflict rejection naming the active run.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains(&format!("busy with run {run}"))
            && text(&output).contains("not replaced under live work"),
        "{}",
        text(&output)
    );
    assert_eq!(fake.record(&run)["phase"], "running");
    assert_eq!(fake.runs(), 1);

    // The worker leads the run's session; job descendants remain in it.
    let session = record["worker"]["pid"].as_i64().unwrap().to_string();
    let _ = Command::new("pkill")
        .args(["-KILL", "-s", &session])
        .status();
    wait_run_gone(&record);
    // Confirmed loss: the replacement service's startup records the
    // interruption before it accepts the fresh UUID.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let fresh = run_id(&events(&output));
    let record = fake.record(&run);
    assert_eq!(record["phase"], "interrupted");
    // The replacement service's startup (not admission) recorded it.
    let reason = record["terminal"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("confirmed by supervisor startup reconciliation"),
        "{reason}"
    );
    let interrupted_at = record["timings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["phase"] == "interrupted")
        .unwrap()["started_at_ms"]
        .as_u64()
        .unwrap();
    assert!(interrupted_at <= fake.record(&fresh)["accepted_at_ms"].as_u64().unwrap());

    // Recovery transferred no source, ran no hook and replayed no job.
    let recovery = fake.argv_log();
    let recovery: Vec<_> = recovery.lines().skip(argv_before).collect();
    let reads: Vec<_> = recovery
        .iter()
        .take_while(|l| !l.contains("uname"))
        .collect();
    assert!(
        reads
            .iter()
            .all(|l| !l.contains("--stdin") && !l.contains("submit")),
        "{reads:?}"
    );
    assert_eq!(
        std::fs::read_dir(fake.run_dir(&run).join("hooks"))
            .unwrap()
            .count(),
        hooks.len()
    );
    let count = std::fs::read_to_string(home.join(".goodflows/state-count")).unwrap();
    assert_eq!(count.lines().count(), 1);
}

#[test]
fn large_payloads_stream_and_corrupt_or_partial_transfers_never_activate() {
    use std::os::unix::fs::PermissionsExt;
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "echo setup-ran"
resume = "echo resume-ran"
[[jobs]]
name = "ok"
command = "touch job-ran"
"#,
    );
    // Incompressible content: the archive exceeds the 10 MiB HTTP stdin cap.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let big: Vec<u8> = (0..11 * 1024 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    std::fs::write(fake.project.join("big.bin"), &big).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert!(
        of_type(&all, "sprite_snapshot")[0]["archive_bytes"]
            .as_u64()
            .unwrap()
            > 10 * 1024 * 1024
    );
    let earlier = run_id(&all);
    let source = fake.run_dir(&earlier).join("source");
    assert_eq!(
        sha256(&std::fs::read(source.join("big.bin")).unwrap()),
        sha256(&big)
    );
    assert!(fake.argv_log().contains("--stdin") && !fake.argv_log().contains("--http-post"));
    let earlier_record = std::fs::read(fake.run_dir(&earlier).join("record.json")).unwrap();
    let check_untouched = || {
        assert_eq!(
            std::fs::read(fake.run_dir(&earlier).join("record.json")).unwrap(),
            earlier_record
        );
        assert_eq!(
            sha256(&std::fs::read(source.join("big.bin")).unwrap()),
            sha256(&big)
        );
    };
    let check_failed = |run: &str| {
        let dir = fake.run_dir(run);
        let record = fake.record(run);
        assert_eq!(record["phase"], "transfer_failed", "{record}");
        assert!(record["hook"].is_null(), "no hook ran: {record}");
        for name in ["source", "source.incoming", "hooks", "workflow.jsonl"] {
            assert!(!dir.join(name).exists(), "{name}");
        }
    };

    // Injected truncation of the streamed stdin.
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_TRUNCATE_STDIN", "1000000")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("source transfer failed"),
        "{}",
        text(&output)
    );
    check_failed(&run_id(&events(&output)));
    check_untouched();

    // Crafted submissions: content hash, mode, archive checksum and size.
    let home = fake.home();
    let template: Value =
        serde_json::from_slice(&std::fs::read(fake.run_dir(&earlier).join("header.json")).unwrap())
            .unwrap();
    let good = sha256(b"hello\n");
    let manifest =
        |sha: &str| format!(r#"[{{"path":"a.txt","size":6,"sha256":"{sha}","executable":false}}]"#);
    let plain = tar_gz(&[("a.txt", b"hello\n", 0o644)]);
    let executable = tar_gz(&[("a.txt", b"hello\n", 0o755)]);
    // (case, manifest, archive, archive sha256, declared bytes, expected reason)
    type Case = (&'static str, String, Vec<u8>, String, u64, &'static str);
    let cases: Vec<Case> = vec![
        (
            "1",
            manifest(&sha256(b"other\n")),
            plain.clone(),
            sha256(&plain),
            plain.len() as u64,
            "does not match the staged manifest",
        ),
        (
            "2",
            manifest(&good),
            executable.clone(),
            sha256(&executable),
            executable.len() as u64,
            "mode",
        ),
        (
            "3",
            manifest(&good),
            plain.clone(),
            sha256(b"x"),
            plain.len() as u64,
            "checksum mismatch",
        ),
        (
            "4",
            manifest(&good),
            plain[..plain.len() / 2].to_vec(),
            sha256(&plain),
            plain.len() as u64,
            "",
        ),
    ];
    for (n, manifest, archive, archive_sha, bytes, reason) in cases {
        let run = format!("00000000-0000-4000-8000-00000000000{n}");
        let mut header = template.clone();
        header["run_id"] = Value::from(run.clone());
        header["manifest"] = serde_json::from_str(&manifest).unwrap();
        header["manifest_sha256"] = Value::from(sha256(manifest.as_bytes()));
        header["archive_sha256"] = Value::from(archive_sha);
        header["archive_bytes"] = Value::from(bytes);
        let reply = agent_submit(&fake, &home, "T-1", &header, &archive);
        assert_eq!(reply["status"], "transfer_failed", "{reply}");
        assert!(
            reply["reason"].as_str().unwrap().contains(reason),
            "{reply}"
        );
        check_failed(&run);
        check_untouched();
    }
    assert_eq!(
        std::fs::metadata(source.join("big.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    // Failed transfers released the identity; the next run resumes normally.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let hooks = of_type(&all, "sprite_hook_output");
    assert!(hooks.iter().all(|e| e["hook"] == "resume") && !hooks.is_empty());
}

type Routes = Arc<Mutex<Vec<(String, u16, Vec<u8>)>>>;

/// A local release server for runner provisioning tests.
fn release_server() -> (String, Routes, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (r, l) = (routes.clone(), log.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            let _ = reader.read_line(&mut first);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = first
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            l.lock().unwrap().push(path.clone());
            let (status, body) = r
                .lock()
                .unwrap()
                .iter()
                .find(|(p, ..)| *p == path)
                .map(|(_, s, b)| (*s, b.clone()))
                .unwrap_or((404, b"not found".to_vec()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    (url, routes, log)
}

#[test]
fn incompatible_or_unpublished_runners_fail_before_hooks_without_latest() {
    use std::os::unix::fs::PermissionsExt;
    let fake =
        Fake::new(&BASIC.replace("pass_env = [\"FORWARDED\", \"EMPTY_VALUE\", \"MODE\"]", ""));
    // The fake Sprite reports the architecture in <root>/arch.
    let uname = fake.root.join("bin/uname");
    std::fs::write(
        &uname,
        format!(
            "#!/bin/sh\nif [ \"$1\" = -m ] && [ -e '{0}/arch' ]; then cat '{0}/arch'; else PATH=/usr/bin:/bin exec uname \"$@\"; fi\n",
            fake.root.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&uname, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(fake.root.join("arch"), "aarch64\n").unwrap();
    let (url, routes, requests) = release_server();
    let version = env!("CARGO_PKG_VERSION");
    let archive_path = format!("/v{version}/goodflows-v{version}-aarch64-unknown-linux-gnu.tar.gz");
    let publish = |archive: Option<Vec<u8>>, checksum: Option<String>| {
        // A real release never changes; this test republishes the same
        // version, so the controller's verified-runner cache is cleared too.
        let _ = std::fs::remove_dir_all(fake.project.join(".goodflows/sprites/runners"));
        let mut routes = routes.lock().unwrap();
        routes.clear();
        if let Some(archive) = archive {
            let digest = checksum.unwrap_or_else(|| sha256(&archive));
            routes.push((
                format!("{archive_path}.sha256"),
                200,
                format!("{digest}  x\n").into_bytes(),
            ));
            routes.push((archive_path.clone(), 200, archive));
        }
    };
    let runner = |hello: &str| {
        let script = format!("#!/bin/sh\necho '{hello}'\n");
        tar_gz(&[(
            &format!("goodflows-v{version}-aarch64-unknown-linux-gnu/goodflows"),
            script.as_bytes(),
            0o755,
        )])
    };
    let hello =
        |v: &str, p: u32, a: &str| format!(r#"{{"version":"{v}","protocol":{p},"arch":"{a}"}}"#);
    let attempt = |expected: &str| {
        let output = fake.env_run(
            &["run", "--sprite", "T-1", "--json"],
            &[
                ("GOODFLOWS_TEST_RELEASE_URL", &url),
                ("NO_PROXY", "127.0.0.1"),
                ("no_proxy", "127.0.0.1"),
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{}", text(&output));
        assert!(
            text(&output).contains(expected),
            "{expected}: {}",
            text(&output)
        );
        assert!(of_type(&events(&output), "sprite_hook_output").is_empty());
        let goodflows = fake.home().join(".goodflows");
        assert!(!goodflows.join("runs").exists(), "no run was submitted");
        assert!(!goodflows.join("runners/current").exists());
    };
    // Missing unpublished platform artifact.
    publish(None, None);
    attempt(&format!(
        "no published Linux aarch64 runner for goodflows v{version}"
    ));
    // Checksum mismatch.
    publish(
        Some(runner(&hello(version, 1, "aarch64"))),
        Some(sha256(b"tampered")),
    );
    attempt("does not match its published checksum");
    // Version, protocol and architecture mismatches of the installed runner.
    for reply in [
        hello("0.0.1", 1, "aarch64"),
        hello(version, 99, "aarch64"),
        hello(version, 1, "x86_64"),
    ] {
        publish(Some(runner(&reply)), None);
        attempt("does not match this goodflows");
    }
    // A release archive without the executable.
    publish(Some(tar_gz(&[("README", b"no binary", 0o644)])), None);
    attempt("has no goodflows executable");
    // An unsupported architecture is refused without any download.
    let downloads = requests.lock().unwrap().len();
    std::fs::write(fake.root.join("arch"), "riscv64\n").unwrap();
    attempt("unsupported Sprite architecture riscv64");
    assert_eq!(requests.lock().unwrap().len(), downloads);
    // Only the exact version was ever requested; never `latest`.
    for path in requests.lock().unwrap().iter() {
        assert!(
            path.starts_with(&format!("/v{version}/")) && !path.contains("latest"),
            "{path}"
        );
    }
    assert!(!fake.argv_log().contains("latest"));
    // The matching local executable is used once the architecture matches.
    std::fs::remove_file(fake.root.join("arch")).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

fn kill_run_processes(record: &Value) {
    let mut pids = vec![record["worker"]["pid"].as_i64().unwrap() as i32];
    for child in record["children"].as_array().unwrap() {
        pids.push(-(child["pgid"].as_i64().unwrap() as i32));
    }
    for pid in pids {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    // The worker leads the run's session; job descendants remain in it.
    let session = record["worker"]["pid"].as_i64().unwrap().to_string();
    let _ = Command::new("pkill")
        .args(["-KILL", "-s", &session])
        .status();
    wait_run_gone(record);
}

fn hooks_of(events: &[Value]) -> std::collections::BTreeSet<String> {
    of_type(events, "sprite_hook_output")
        .iter()
        .map(|e| e["hook"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn setup_and_resume_lifecycle_survives_failures_changes_and_lost_acknowledgements() {
    let config = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "echo setup-ran; test -f hang && sleep 300; true"
resume = "echo resume-ran; test -f resume-ok || exit 5"
[[jobs]]
name = "job"
command = "echo started >> \"$GOODFLOWS_SPRITE_STATE_DIR/../state-count\""
"#;
    let fake = Fake::new(config);
    let count = |fake: &Fake| {
        std::fs::read_to_string(fake.home().join(".goodflows/state-count"))
            .map(|c| c.lines().count())
            .unwrap_or(0)
    };
    let prepared = |fake: &Fake| fake.home().join(".goodflows/prepared.json");

    // Setup's process is lost before durable success: never inferred.
    fake.write("hang", "");
    let mut controller = spawn_run(&fake, "T-1");
    let lost = wait_phase(&fake, "setup");
    controller.kill().unwrap();
    let _ = controller.wait();
    kill_run_processes(&fake.record(&lost));
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(events(&output)[0]["prepared"].is_null());
    assert_eq!(fake.record(&lost)["phase"], "interrupted");
    assert!(!prepared(&fake).exists());
    assert_eq!(count(&fake), 0);

    // The next run is treated as unprepared: setup only, then the job.
    std::fs::remove_file(fake.project.join("hang")).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(hooks_of(&events(&output)), ["setup".to_owned()].into());
    assert!(prepared(&fake).exists());
    let digest = std::fs::read(prepared(&fake)).unwrap();

    // The controller loses the completion acknowledgement (no follow):
    // the result stays remote and the next run resumes, never re-running setup.
    fake.write("resume-ok", "");
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_FAIL_OPS", "follow")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let unacknowledged = run_id(&events(&output));
    wait_for("remote completion", Duration::from_secs(30), || {
        fake.record(&unacknowledged)["phase"] == "completed"
    });
    assert_eq!(fake.record(&unacknowledged)["hook"], "resume");
    assert_eq!(count(&fake), 2);

    // Failed resume: no job, preparation kept, and resume is retried.
    std::fs::remove_file(fake.project.join("resume-ok")).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let failed = run_id(&events(&output));
    assert_eq!(fake.record(&failed)["phase"], "preparation_failed");
    assert_eq!(hooks_of(&events(&output)), ["resume".to_owned()].into());
    assert_eq!(count(&fake), 2);
    assert_eq!(std::fs::read(prepared(&fake)).unwrap(), digest);
    fake.write("resume-ok", "");
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(hooks_of(&events(&output)), ["resume".to_owned()].into());
    assert_eq!(count(&fake), 3);

    // Changed setup text does not silently reclassify the prepared Sprite.
    std::fs::write(
        fake.project.join("goodflows.toml"),
        config.replace("echo setup-ran", "echo setup-v2"),
    )
    .unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(hooks_of(&all), ["resume".to_owned()].into());
    assert_eq!(of_type(&all, "sprite_setup_changed").len(), 1);
    assert_eq!(std::fs::read(prepared(&fake)).unwrap(), digest);
    assert_eq!(count(&fake), 4);

    // No run ever executed both hooks.
    for run in std::fs::read_dir(fake.home().join(".goodflows/runs"))
        .unwrap()
        .flatten()
    {
        let hooks: std::collections::BTreeSet<String> = std::fs::read_dir(run.path().join("hooks"))
            .unwrap()
            .flatten()
            .map(|f| {
                f.file_name()
                    .to_string_lossy()
                    .split('.')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        // The run killed in setup can die after its record entered setup but
        // before the hook's output files were opened; every other run ran
        // exactly one hook.
        if run.file_name() == std::ffi::OsStr::new(&lost) {
            assert!(hooks.len() <= 1, "{:?}: {hooks:?}", run.path());
        } else {
            assert_eq!(hooks.len(), 1, "{:?}: {hooks:?}", run.path());
        }
    }
}

const LAYERS: &str = r#"
[settings]
sprite_prefix = "gft"
[settings.env]
L1 = "settings"
L2 = "settings"
L3 = "settings"
L4 = "settings"
L5 = "settings"
[sprite]
setup = "printf '%s|' \"$L0\" \"$L1\" \"$L2\" \"$L3\" \"$L4\" \"$L5\" \"${EMPTY-unset}\" \"$MULTI\" \"${PRIVATE_TOKEN-absent}\" > \"$GOODFLOWS_SPRITE_STATE_DIR/../hook-env\""
resume = "true"
pass_env = ["L0", "L1", "L2", "L3", "L4", "L5", "EMPTY", "MULTI"]
[workflows.w.settings.env]
L2 = "workflow"
L3 = "workflow"
L4 = "workflow"
L5 = "workflow"
[environments.p.settings.env]
L3 = "profile"
L4 = "profile"
L5 = "profile"
[[jobs]]
name = "default"
command = "true"
[[workflows.w.jobs]]
name = "j"
env = { L4 = "job", L5 = "job" }
[[workflows.w.jobs.steps]]
id = "s"
env = { L5 = "step" }
command = "printf '%s|' \"$L0\" \"$L1\" \"$L2\" \"$L3\" \"$L4\" \"$L5\" \"${EMPTY-unset}\" \"$MULTI\" \"${PRIVATE_TOKEN-absent}\" > \"$GOODFLOWS_SPRITE_STATE_DIR/../job-env\""
"#;

fn files_under(dir: &Path, skip: &[&str], out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if skip.iter().any(|s| entry.file_name() == *s) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => files_under(&path, skip, out),
            Ok(kind) if kind.is_file() => out.push(path),
            _ => {}
        }
    }
}

#[test]
fn a_forwarded_home_reaches_hooks_and_jobs_without_moving_runner_state() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "printf '%s' \"$HOME\" > \"$GOODFLOWS_SPRITE_STATE_DIR/../hook-home\""
resume = "true"
pass_env = ["HOME"]
[[jobs]]
name = "check"
command = "printf '%s' \"$HOME\" > \"$GOODFLOWS_SPRITE_STATE_DIR/../job-home\""
"#,
    );
    // The caller's HOME differs from the Sprite's.
    let caller_home = fake.root.join("caller-home");
    std::fs::create_dir_all(&caller_home).unwrap();
    let output = fake
        .command()
        .env("HOME", &caller_home)
        .args(["run", "--sprite", "T-1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(all.last().unwrap()["type"], "summary");
    let goodflows = fake.home().join(".goodflows");
    let caller_home = caller_home.to_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(goodflows.join("hook-home")).unwrap(),
        caller_home
    );
    assert_eq!(
        std::fs::read_to_string(goodflows.join("job-home")).unwrap(),
        caller_home
    );
}

#[test]
fn forwarded_values_are_exact_lowest_precedence_and_never_recorded() {
    let fake = Fake::new(LAYERS);
    let multi = format!("{SENTINEL} \"q\" 'x' $(touch pwned) `touch pwned2` ;|& *\n line2\n");
    let mut command = fake.command();
    for name in ["L0", "L1", "L2", "L3", "L4", "L5"] {
        command.env(name, "caller");
    }
    let output = command
        .env("EMPTY", "")
        .env("MULTI", &multi)
        .env("PRIVATE_TOKEN", SENTINEL)
        .env("FAKE_SPRITE_CPUS", "0")
        .args([
            "run",
            "w",
            "--env",
            "p",
            "--sprite",
            "T-1",
            "--json",
            "--history",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    let goodflows = fake.home().join(".goodflows");
    // Hooks receive the exact forwarded values; configured env is for jobs.
    assert_eq!(
        std::fs::read_to_string(goodflows.join("hook-env")).unwrap(),
        format!("caller|caller|caller|caller|caller|caller||{multi}|absent|")
    );
    // Jobs: forwarding < settings < workflow < profile < job < step.
    assert_eq!(
        std::fs::read_to_string(goodflows.join("job-env")).unwrap(),
        format!("caller|settings|workflow|profile|job|step||{multi}|absent|")
    );
    let source = fake.run_dir(&run).join("source");
    assert!(!source.join("pwned").exists() && !source.join("pwned2").exists());
    // The sentinel (forwarded and ambient) is absent from transport argv,
    // controller output, every GoodFlows-written file on the Sprite and the
    // local bundle/receipt. Only the user's own hook/job files contain it.
    assert!(!fake.argv_log().contains(SENTINEL));
    assert!(!text(&output).contains(SENTINEL));
    assert!(!fake
        .api
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains(SENTINEL)));
    let mut files = Vec::new();
    files_under(
        &fake.home(),
        &["runners", "hook-env", "job-env"],
        &mut files,
    );
    files_under(&fake.project.join(".goodflows"), &[], &mut files);
    assert!(files.iter().any(|f| f.ends_with("header.json")));
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(SENTINEL),
            "{}",
            file.display()
        );
    }
    // No remote process carries a forwarded value in its argv or environment.
    let needle = format!("{}/", fake.root.to_string_lossy());
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
        if String::from_utf8_lossy(&environ).contains(&needle) {
            let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
            assert!(!String::from_utf8_lossy(&cmdline).contains(SENTINEL));
            assert!(!String::from_utf8_lossy(&environ).contains(SENTINEL));
        }
    }
    // Host measurements in the collected history were taken by the remote runner.
    let mut history = Vec::new();
    files_under(
        &fake.run_dir(&run).join("results/history"),
        &[],
        &mut history,
    );
    assert_eq!(history.len(), 1, "{history:?}");
    // The fake Sprite is pinned to one CPU, unlike this controller.
    let saved: Value = serde_json::from_slice(&std::fs::read(&history[0]).unwrap()).unwrap();
    assert_eq!(saved["host"]["logical_cpus"], 1, "{saved}");
    let controller_cpus = std::thread::available_parallelism().unwrap().get();
    if controller_cpus > 1 {
        assert_ne!(saved["host"]["logical_cpus"], controller_cpus);
    }
}

#[test]
fn state_directory_is_remote_only_and_caches_outlive_fresh_sources() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "mkdir -p \"$GOODFLOWS_SPRITE_STATE_DIR/cache\" && echo built > \"$GOODFLOWS_SPRITE_STATE_DIR/cache/artifact\" && ln -s \"$GOODFLOWS_SPRITE_STATE_DIR/cache\" cache"
resume = "ln -s \"$GOODFLOWS_SPRITE_STATE_DIR/cache\" cache"
[[jobs]]
name = "job"
command = "printf '%s\\n' \"${GOODFLOWS_SPRITE_STATE_DIR-unset}\"; if [ -n \"${GOODFLOWS_SPRITE_STATE_DIR-}\" ]; then cat cache/artifact; if [ -e first-only.txt ]; then echo first; else echo fresh; fi; fi"
"#,
    );
    let job_output = |output: &Output| -> String {
        of_type(&events(output), "job_output")
            .iter()
            .map(|e| e["data"].as_str().unwrap().to_owned())
            .collect()
    };
    // An ordinary local run never inherits a remote-state identity.
    let output = fake.env_run(
        &["run", "--json"],
        &[("GOODFLOWS_SPRITE_STATE_DIR", "/misleading/state")],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(job_output(&output), "unset\n");
    assert!(!fake.invoked());

    fake.write("first-only.txt", "only in the first source\n");
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let first = run_id(&events(&output));
    let state = fake.home().join(".goodflows/state");
    let printed = job_output(&output);
    let mut lines = printed.lines();
    assert_eq!(Path::new(lines.next().unwrap()), state);
    assert_eq!(lines.next(), Some("built"));
    assert_eq!(lines.next(), Some("first"));
    std::fs::remove_file(fake.project.join("first-only.txt")).unwrap();
    // The second run gets a fresh source and reuses the script-managed cache.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let second = run_id(&events(&output));
    assert_eq!(
        job_output(&output),
        format!("{}\nbuilt\nfresh\n", state.display())
    );
    assert_ne!(fake.run_dir(&first), fake.run_dir(&second));
    assert!(fake.run_dir(&first).join("source/first-only.txt").exists());
    assert_eq!(
        std::fs::read_link(fake.run_dir(&second).join("source/cache")).unwrap(),
        state.join("cache")
    );
    assert!(!state.starts_with(fake.run_dir(&second)));
}

#[test]
fn a_stalled_follower_cannot_stop_disk_capture_and_release_is_last() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "noisy"
command = "seq 1 30000 | sed 's/^/line-/'"
"#,
    );
    // The controller's stdout is never read, so it stops consuming the stream.
    let mut controller = spawn_args(&fake, &["run", "--sprite", "T-1", "--json"], &[]);
    let run = wait_phase(&fake, "completed");
    let workflow = std::fs::read_to_string(fake.run_dir(&run).join("workflow.jsonl")).unwrap();
    assert!(workflow.contains("line-30000"), "disk capture is complete");
    let summary: Value = serde_json::from_str(
        workflow
            .lines()
            .rfind(|l| l.contains("\"summary\""))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(summary["success"], true);
    // Lifecycle timings are separate from the native job durations.
    let record = fake.record(&run);
    let phases: Vec<&str> = record["timings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["phase"].as_str().unwrap())
        .collect();
    for phase in [
        "accepted",
        "uploading",
        "verifying",
        "setup",
        "running",
        "finalizing",
        "completed",
    ] {
        assert!(phases.contains(&phase), "{phases:?}");
    }
    assert!(summary["results"][0]["duration_ms"].is_u64(), "{summary}");
    // The terminal record, active pointer and lock were settled before the
    // final hold DELETE; nothing about the run reaches the API afterwards.
    let release = format!("DELETE /v1/tasks/gf-{run}");
    wait_for("hold release", Duration::from_secs(10), || {
        fake.api
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with(&release))
    });
    let api = fake.api.lock().unwrap().clone();
    let delete = api.iter().position(|l| l.starts_with(&release)).unwrap();
    assert_eq!(
        api[delete - 1],
        format!("STATE {run} phase=completed active=false lock=free")
    );
    assert!(api[delete + 1..].iter().all(|l| !l.contains(&run)));
    controller.kill().unwrap();
    let _ = controller.wait();
    let output = fake.run(&["sprite", "logs", "T-1", "--run", &run, "--json"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("line-30000"));
}

#[test]
fn renewal_failure_blocks_admission_and_stops_work_without_releasing_early() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 300"));
    let controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    let goodflows = fake.home().join(".goodflows");
    // Hold the worker after it stopped the work but before its terminal
    // record, so the admission checks below always meet a live run.
    fake.arm("finalizing");
    std::fs::write(fake.root.join("api.fail"), "").unwrap();
    let failed_at = Instant::now();
    wait_for("degraded marker", Duration::from_secs(10), || {
        goodflows.join("control/hold-degraded").exists()
    });
    // Bounded cleanup: the work is stopped before the last good renewal's
    // hold (12 s) expires, plus the short kill grace.
    fake.wait_reached("finalizing");
    assert!(
        failed_at.elapsed() < Duration::from_secs(20),
        "{:?}",
        failed_at.elapsed()
    );
    // Admission is blocked and the identity is still held: hold expiry is
    // never taken as proof that the worker stopped.
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    let status = &events(&output)[0];
    assert_eq!(status["degraded"], true, "{status}");
    assert_eq!(status["busy"], true, "{status}");
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(fake.runs(), 1);
    fake.release("finalizing");
    wait_for("terminal record", Duration::from_secs(30), || {
        fake.record(&run)["terminal"].is_object()
    });
    let record = fake.record(&run);
    assert_eq!(record["phase"], "cancelled", "{record}");
    assert!(record["terminal"]["reason"]
        .as_str()
        .unwrap()
        .contains("wake hold renewal failed"));
    // Progress stayed durable.
    let saved = std::fs::read_to_string(fake.run_dir(&run).join("events.jsonl")).unwrap();
    assert!(saved.contains("\"running\"") && saved.contains("\"cancelled\""));
    let controller = controller.wait_with_output().unwrap();
    assert_ne!(controller.status.code(), Some(0));
    let _ = events(&controller);
    let release = format!("DELETE /v1/tasks/gf-{run}");
    wait_for("hold release", Duration::from_secs(10), || {
        fake.api
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with(&release))
    });
    let api = fake.api.lock().unwrap().clone();
    let delete = api.iter().position(|l| l.starts_with(&release)).unwrap();
    assert_eq!(
        api[delete - 1],
        format!("STATE {run} phase=cancelled active=false lock=free")
    );
    assert!(!goodflows.join("control/hold-degraded").exists());
    // Once the API recovers, admission resumes.
    std::fs::remove_file(fake.root.join("api.fail")).unwrap();
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn recovery_reports_every_state_from_an_independent_controller() {
    let config = SLOW.replace(
        "resume = \"true\"",
        "resume = \"true\"\npass_env = [\"FORWARDED\"]",
    );
    let fake = Fake::new(&config);
    // Independent controller: another directory, no source, run-only values unset.
    let elsewhere = fake.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("goodflows.toml"), &config).unwrap();
    let recover = |args: &[&str], env: &[(&str, &str)]| {
        let mut command = fake.command();
        command.current_dir(&elsewhere);
        for (key, value) in env {
            command.env(key, value);
        }
        let before = fake.argv_lines();
        let output = command.args(args).output().unwrap();
        // Recovery never streams source and never submits or replays.
        for line in fake.argv_log().lines().skip(before) {
            assert!(!line.contains("--stdin"), "{args:?}: {line}");
            assert!(
                !line.split_whitespace().any(|w| w == "submit"),
                "{args:?}: {line}"
            );
        }
        output
    };
    let status = |env: &[(&str, &str)]| {
        let output = recover(&["sprite", "status", "T-1", "--json"], env);
        (output.status.code(), events(&output)[0].clone())
    };
    // Missing.
    let (code, missing) = status(&[]);
    assert_eq!((code, missing["state"].as_str()), (Some(0), Some("absent")));
    // Active.
    let controller = spawn_args(
        &fake,
        &["run", "--sprite", "T-1", "--json"],
        &[("FORWARDED", "run-only")],
    );
    let run = wait_running(&fake);
    let (code, active) = status(&[]);
    assert_eq!(code, Some(0));
    assert_eq!(
        (active["state"].as_str(), active["active_run_id"].as_str()),
        (Some("active"), Some(run.as_str()))
    );
    let output = recover(&["sprite", "collect", "T-1", "--run", &run, "--json"], &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        events(&output)[0]["complete"],
        false,
        "partial while active"
    );
    // Completed.
    let controller = controller.wait_with_output().unwrap();
    assert_eq!(controller.status.code(), Some(0), "{}", text(&controller));
    let (_, done) = status(&[]);
    assert_eq!(done["state"], "idle");
    assert_eq!(done["runs"][0]["phase"], "completed");
    let output = recover(&["sprite", "logs", "T-1", "--run", &run, "--json"], &[]);
    assert!(events(&output).iter().any(|e| e["type"] == "summary"));
    let output = recover(&["sprite", "collect", "T-1", "--run", &run, "--json"], &[]);
    assert_eq!(events(&output)[0]["complete"], true);
    // Interrupted (confirmed loss).
    let mut controller = spawn_args(
        &fake,
        &["run", "--sprite", "T-1", "--json"],
        &[("FORWARDED", "run-only")],
    );
    let lost = wait_running(&fake);
    // The second job itself must have started before its loss.
    wait_for("job start", Duration::from_secs(30), || {
        std::fs::read_to_string(fake.home().join(".goodflows/state-count"))
            .is_ok_and(|c| c.lines().count() == 2)
    });
    controller.kill().unwrap();
    let _ = controller.wait();
    kill_run_processes(&fake.record(&lost));
    let (_, interrupted) = status(&[]);
    assert_eq!(interrupted["state"], "idle");
    assert_eq!(interrupted["runs"][0]["run_id"], lost.as_str());
    assert_eq!(interrupted["runs"][0]["phase"], "interrupted");
    // Unreachable: the agent call fails, or the platform does.
    let (code, unreachable) = status(&[("FAKE_SPRITE_FAIL_OPS", "status")]);
    assert_eq!(
        (code, unreachable["state"].as_str()),
        (Some(3), Some("unreachable"))
    );
    for args in [
        &["sprite", "status", "T-1", "--json"][..],
        &["sprite", "logs", "T-1", "--run", &run, "--json"],
        &["sprite", "collect", "T-1", "--run", &run, "--json"],
        &["sprite", "cancel", "T-1", "--run", &run, "--json"],
    ] {
        // With and without an explicit organization (no `org list` call).
        for org in [None, Some("testorg")] {
            let mut env = vec![("FAKE_SPRITE_UNREACHABLE", "1")];
            env.extend(org.map(|o| ("SPRITE_ORG", o)));
            let output = recover(args, &env);
            assert_eq!(output.status.code(), Some(3), "{args:?}: {}", text(&output));
            assert_eq!(events(&output).last().unwrap()["type"], "sprite_unresolved");
        }
    }
    // Recovery never transferred source, ran hooks or workflow commands.
    let submits = fake
        .argv_log()
        .lines()
        .filter(|l| l.split_whitespace().any(|w| w == "submit"))
        .count();
    assert_eq!(submits, 2, "only the two runs submitted");
    let count = std::fs::read_to_string(fake.home().join(".goodflows/state-count")).unwrap();
    assert_eq!(count.lines().count(), 2);
    assert_eq!(fake.runs(), 2);
}

const CANCELLABLE: &str = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[[jobs]]
name = "slow"
command = "sleep 3001 & echo $! > \"$GOODFLOWS_SPRITE_STATE_DIR/../grandchild\"; sleep 300"
[[workflows.mid.jobs]]
name = "mid"
command = "sleep 3; echo done"
"#;

#[test]
fn cancellation_is_uuid_bound_forwarded_on_interrupt_and_never_inferred_from_disconnect() {
    let fake = Fake::new(CANCELLABLE);
    let grandchild = |fake: &Fake| -> i32 {
        let path = fake.home().join(".goodflows/grandchild");
        wait_for("grandchild", Duration::from_secs(30), || {
            std::fs::read_to_string(&path).is_ok_and(|p| p.trim().parse::<i32>().is_ok())
        });
        let pid = std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::fs::remove_file(path).unwrap();
        pid
    };
    // Attached interrupt: forwarded as cancellation of exactly this run.
    let controller = spawn_run(&fake, "T-1");
    let first = wait_running(&fake);
    let descendant = grandchild(&fake);
    unsafe {
        libc::kill(controller.id() as i32, libc::SIGINT);
    }
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "sprite_cancel")[0]["acknowledged"], true);
    assert_eq!(fake.record(&first)["phase"], "cancelled");
    wait_for("descendant cleanup", Duration::from_secs(10), || {
        process_gone(descendant)
    });

    // A lost acknowledgement after the interrupt: exit 3, and a stale
    // cancellation of the older UUID signals nothing.
    let controller = spawn_args(
        &fake,
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_LOSE_ACK_OPS", "cancel")],
    );
    let second = wait_phase(&fake, "running");
    let descendant = grandchild(&fake);
    let output = fake.run(&["sprite", "cancel", "T-1", "--run", &first, "--json"]);
    assert_eq!(events(&output)[0]["status"], "already_terminal");
    assert_eq!(fake.record(&second)["phase"], "running");
    assert_eq!(fake.record(&second)["cancel_requested"], false);
    unsafe {
        libc::kill(controller.id() as i32, libc::SIGINT);
    }
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "sprite_cancel")[0]["acknowledged"], false);
    assert_eq!(all.last().unwrap()["type"], "sprite_unresolved");
    assert!(of_type(&all, "summary").is_empty());
    wait_for("remote cancellation", Duration::from_secs(30), || {
        fake.record(&second)["phase"] == "cancelled"
    });
    wait_for("descendant cleanup", Duration::from_secs(10), || {
        process_gone(descendant)
    });

    // EOF on the controller's output is a disconnect, not a cancellation.
    let mut controller = spawn_args(&fake, &["run", "mid", "--sprite", "T-1", "--json"], &[]);
    let third = wait_phase(&fake, "running");
    drop(controller.stdout.take());
    let _ = controller.wait();
    wait_for("remote completion", Duration::from_secs(30), || {
        fake.record(&third)["phase"] == "completed"
    });
    assert_eq!(fake.record(&third)["cancel_requested"], false);
    assert_eq!(fake.record(&third)["terminal"]["exit_code"], 0);
}

#[test]
fn exit_codes_distinguish_known_results_from_unresolved_ones() {
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "printf 'not json\n{\"type\":\"summary\",\"fake\":true}\n'; printf '\\001\\002binary'; echo noisy-stderr >&2"
resume = "printf '{\"type\":\"summary\",\"fake\":true}\n'; echo noisy >&2"
[[jobs]]
name = "ok"
command = "echo job"
[[workflows.bad.jobs]]
name = "bad"
command = "exit 7"
"#,
    );
    // Noisy hooks never break JSON purity or fabricate a summary.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let summaries = of_type(&all, "summary");
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0]["fake"].is_null());
    assert_eq!(all.last().unwrap()["type"], "summary");
    assert!(of_type(&all, "sprite_hook_output")
        .iter()
        .any(|e| e["data"] == "not json\n"));
    // Native failure.
    let output = fake.run(&["run", "bad", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    // Incomplete required collection: exit 3, no summary.
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_FAIL_OPS", "export")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "sprite_collect_failed").len(), 1);
    assert_eq!(all.last().unwrap()["type"], "sprite_unresolved");
    assert!(of_type(&all, "summary").is_empty());
    // Transport failure during submission: exit 3 with the run reference.
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_FAIL_OPS", "submit")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    assert_eq!(all.last().unwrap()["run_id"], run_id(&all).as_str());
    // Usage errors stay 2.
    for args in [
        &["run", "--sprite"][..],
        &["sprite", "logs", "T-1"],
        &["sprite", "cancel", "T-1", "--run", "nope"],
    ] {
        assert_eq!(fake.run(args).status.code(), Some(2), "{args:?}");
    }
    // Config/runtime/preparation errors stay 1.
    fake.write("noprefix.toml", "[sprite]\nsetup = \"true\"\nresume = \"true\"\n[[jobs]]\nname = \"a\"\ncommand = \"true\"\n");
    for args in [
        &[
            "run",
            "--config",
            "noprefix.toml",
            "--sprite",
            "T-1",
            "--json",
        ][..],
        &[
            "sprite",
            "status",
            "T-1",
            "--config",
            "noprefix.toml",
            "--json",
        ],
    ] {
        let output = fake.run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", text(&output));
        let _ = events(&output);
    }
}

#[test]
fn the_documented_recovery_sequence_returns_the_original_run() {
    let fake = Fake::new(SLOW);
    let docs =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/sprites.md"))
            .unwrap();
    let block = docs.split("recover from any controller").nth(1).unwrap();
    let block = block
        .split("```bash")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let commands: Vec<Vec<String>> = block
        .lines()
        .filter_map(|l| l.split('#').next())
        .map(|l| l.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        .filter(|w| w.len() > 2 && w[0] == "goodflows" && w[1] == "sprite" && w[2] != "cancel")
        .collect();
    let verbs: Vec<&str> = commands.iter().map(|c| c[2].as_str()).collect();
    assert_eq!(verbs, ["status", "logs", "collect"]);

    let mut controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    controller.kill().unwrap();
    let _ = controller.wait();
    wait_for("remote completion", Duration::from_secs(30), || {
        fake.record(&run)["phase"] == "completed"
    });
    let mut outputs = Vec::new();
    for command in &commands {
        let mut args: Vec<String> = command[1..]
            .iter()
            .map(|a| {
                if a == "<uuid>" {
                    run.clone()
                } else {
                    a.clone()
                }
            })
            .collect();
        args.push("--json".into());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = fake.run(&args);
        assert!(output.status.success(), "{args:?}: {}", text(&output));
        outputs.push(events(&output));
    }
    assert_eq!(outputs[0][0]["runs"][0]["run_id"], run.as_str());
    let summary = outputs[1].iter().find(|e| e["type"] == "summary").unwrap();
    assert_eq!(summary["run_id"], run.as_str());
    let bundle = PathBuf::from(outputs[2][0]["bundle"].as_str().unwrap());
    let record: Value =
        serde_json::from_slice(&std::fs::read(bundle.join("record.json")).unwrap()).unwrap();
    assert_eq!(record["run_id"], run.as_str());
    assert_eq!(outputs[2][0]["complete"], true);
    // No second run: one record, one execution of the counter.
    assert_eq!(fake.runs(), 1);
    let count = std::fs::read_to_string(fake.home().join(".goodflows/state-count")).unwrap();
    assert_eq!(count.lines().count(), 1);
}

#[test]
fn destruction_is_serialized_and_ambiguous_acknowledgements_are_reconciled() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 3"));
    let name_of = |output: &Output| {
        of_type(&events(output), "sprite_run")[0]["sprite"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let create = |fake: &Fake| {
        let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        name_of(&output)
    };
    let destroy = |fake: &Fake, env: &[(&str, &str)]| {
        fake.env_run(&["sprite", "destroy", "T-1", "--yes", "--json"], env)
    };
    // Refused while a run is active.
    let active = spawn_run(&fake, "T-1");
    let _ = wait_running(&fake);
    let output = destroy(&fake, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("is active or unresolved"),
        "{}",
        text(&output)
    );
    let name = name_of(&active.wait_with_output().unwrap());
    assert!(fake.root.join("names").join(&name).exists());

    // A run racing confirmed destruction fails rather than recreating state.
    fake.arm("destroy-prepared");
    let destroyer = spawn_args(&fake, &["sprite", "destroy", "T-1", "--yes", "--json"], &[]);
    fake.wait_reached("destroy-prepared");
    let runs = fake.runs();
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("destruction of this Sprite is in progress"),
        "{}",
        text(&output)
    );
    assert_eq!(fake.runs(), runs);
    fake.release("destroy-prepared");
    let output = destroyer.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fake.root.join("names").join(&name).exists());
    assert!(fake.homes().is_empty());

    // Without stable ids the owned resource cannot be proven: fail closed,
    // and the aborted destruction leaves the Sprite usable.
    create(&fake);
    let output = destroy(&fake, &[("FAKE_SPRITE_LIST_NO_ID", "1")]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("no stable resource id"),
        "{}",
        text(&output)
    );
    assert!(fake.root.join("names").join(&name).exists());
    // Ambiguous listings (two resources with the name) are never guessed.
    let output = destroy(&fake, &[("FAKE_SPRITE_LIST_DUP", "1")]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(fake.root.join("names").join(&name).exists());
    create(&fake);

    // A lost delete acknowledgement is reconciled by lookup, not retried.
    let before = fake.argv_lines();
    let output = destroy(&fake, &[("FAKE_SPRITE_DESTROY", "lose-ack")]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fake.root.join("names").join(&name).exists());
    let destroys = |from: usize| {
        fake.argv_log()
            .lines()
            .skip(from)
            .filter(|l| l.starts_with("destroy "))
            .count()
    };
    assert_eq!(destroys(before), 1);

    // A replacement that appears under the name is never deleted.
    create(&fake);
    let before = fake.argv_lines();
    let output = destroy(&fake, &[("FAKE_SPRITE_DESTROY", "replace")]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("was replaced by a new resource"),
        "{}",
        text(&output)
    );
    assert_eq!(destroys(before), 1);
    assert!(fake.home().join("replacement-marker").exists());
    assert!(fake.root.join("names").join(&name).exists());
}

#[test]
fn an_attached_run_rides_out_a_follow_outage_longer_than_three_retries() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let first = run_id(&events(&output));
    // Every follow connection fails until the toggle is removed.
    std::fs::write(fake.root.join("fail-ops"), "follow").unwrap();
    let follows = |fake: &Fake| {
        fake.argv_log()
            .lines()
            .filter(|l| l.split(' ').any(|a| a == "follow"))
            .count()
    };
    let before = follows(&fake);
    let controller = spawn_args(
        &fake,
        &["run", "--sprite", "T-1", "--json"],
        &[("GOODFLOWS_TEST_FOLLOW_BUDGET_SECS", "60")],
    );
    // More failed reconnects than the old fixed three attempts, with backoff.
    wait_for(
        "four failed follow attempts",
        Duration::from_secs(30),
        || follows(&fake) >= before + 4,
    );
    let mut run = String::new();
    wait_for(
        "the second run to complete",
        Duration::from_secs(60),
        || {
            let runs = fake.home().join(".goodflows/runs");
            std::fs::read_dir(runs).unwrap().flatten().any(|entry| {
                let id = entry.file_name().to_string_lossy().into_owned();
                let done = id != first
                    && fake
                        .try_record(&id)
                        .is_some_and(|r| r["phase"] == "completed");
                if done {
                    run = id;
                }
                done
            })
        },
    );
    std::fs::remove_file(fake.root.join("fail-ops")).unwrap();
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(run_id(&all), run);
    assert_eq!(all.last().unwrap()["type"], "summary");
    assert_eq!(of_type(&all, "summary").len(), 1);
    assert_eq!(of_type(&all, "sprite_collected")[0]["complete"], true);
    // No reconnect replayed a printed event.
    let phases: Vec<_> = of_type(&all, "sprite_phase")
        .iter()
        .map(|e| e["phase"].as_str().unwrap().to_owned())
        .collect();
    let unique: std::collections::BTreeSet<_> = phases.iter().collect();
    assert_eq!(unique.len(), phases.len(), "{phases:?}");

    // Without new output for the whole budget it reports unresolved (exit 3).
    std::fs::write(fake.root.join("fail-ops"), "follow").unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    std::fs::remove_file(fake.root.join("fail-ops")).unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("reconnecting for 3s"),
        "{}",
        text(&output)
    );
    assert!(
        text(&output).contains("abnormal closure"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_different_runner_at_the_content_addressed_path_is_refused_not_replaced() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // Corrupt the installed runner under its content-addressed id.
    let runners = fake.home().join(".goodflows/runners");
    let id = std::fs::read_to_string(runners.join("current")).unwrap();
    let path = runners.join(id.trim()).join("goodflows");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let runs = fake.runs();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("refusing to replace it"),
        "{}",
        text(&output)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"#!/bin/sh\nexit 1\n");
    assert_eq!(fake.runs(), runs, "no run was submitted");
    let staged: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(staged.len(), 1, "no staging copy is left: {staged:?}");
}

#[test]
fn a_capability_drop_failure_is_a_definite_launch_failure() {
    let fake = Fake::new(SLOW);
    // Debug fault hook: the workflow runner's capability drop fails.
    std::fs::write(fake.root.join("pause/caps.fail"), "").unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("capabilities could not be dropped")
            && text(&output).contains("failed before any job started")
            && !text(&output).contains("preparation failed"),
        "{}",
        text(&output)
    );
    let all = events(&output);
    let run = run_id(&all);
    assert!(of_type(&all, "sprite_unresolved").is_empty());
    let record = fake.record(&run);
    assert_eq!(record["phase"], "preparation_failed", "{record}");
    assert!(
        !fake.home().join(".goodflows/state-count").exists(),
        "no job ran"
    );
    // The identity is free again and a later launch succeeds.
    std::fs::remove_file(fake.root.join("pause/caps.fail")).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

/// The live `follow` agent processes of the fake Sprites under `fake`.
fn follow_pids(fake: &Fake) -> Vec<i32> {
    let needle = format!("{}/", fake.root.to_string_lossy());
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
            let follow = args
                .windows(2)
                .any(|w| w == [&b"__sprite-agent"[..], b"follow"]);
            let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
            (follow && !process_gone(pid) && String::from_utf8_lossy(&environ).contains(&needle))
                .then_some(pid)
        })
        .collect()
}

#[test]
fn a_quiet_run_that_loses_its_stream_after_the_budget_still_reconnects() {
    // The job stays silent until the test releases it.
    let fake = Fake::new(&PLAIN.replace(
        "command = \"true\"",
        r#"command = "while [ ! -e \"$GOODFLOWS_SPRITE_STATE_DIR/../go\" ]; do sleep 0.1; done; echo done""#,
    ));
    // Follow budget 3 s (the harness default).
    let controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    let drop_stream = |fake: &Fake| {
        let mut pids = Vec::new();
        wait_for("a follow connection", Duration::from_secs(30), || {
            pids = follow_pids(fake);
            !pids.is_empty()
        });
        for pid in &pids {
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
        pids
    };
    // Let the first stream catch up so the reconnected one prints nothing
    // new. First drop, then a reconnected stream that stays up and quiet for
    // longer than the budget before it drops too.
    std::thread::sleep(Duration::from_secs(2));
    let first = drop_stream(&fake);
    wait_for("a reconnect", Duration::from_secs(30), || {
        follow_pids(&fake).iter().any(|p| !first.contains(p))
    });
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(fake.record(&run)["phase"], "running");
    let second = drop_stream(&fake);
    wait_for(
        "a reconnect after the quiet period",
        Duration::from_secs(30),
        || {
            follow_pids(&fake)
                .iter()
                .any(|p| !first.contains(p) && !second.contains(p))
        },
    );
    std::fs::write(fake.home().join(".goodflows/go"), "").unwrap();
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(all.last().unwrap()["type"], "summary");
    assert!(of_type(&all, "sprite_unresolved").is_empty());
}

#[test]
fn a_stream_that_only_replays_and_drops_ends_unresolved_within_the_budget() {
    // The job stays silent until the test releases it.
    let fake = Fake::new(&PLAIN.replace(
        "command = \"true\"",
        r#"command = "while [ ! -e \"$GOODFLOWS_SPRITE_STATE_DIR/../go\" ]; do sleep 0.1; done""#,
    ));
    // Every follow session is cut after 1 s with an error, after replaying
    // the saved stream: no connection delivers anything new once caught up.
    std::fs::write(fake.root.join("flap-ops"), "follow").unwrap();
    let follows = |fake: &Fake| {
        fake.argv_log()
            .lines()
            .filter(|l| l.split(' ').any(|a| a == "follow"))
            .count()
    };
    let started = Instant::now();
    let mut controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    let mut status = None;
    // Follow budget 3 s (the harness default): bounded, not a reconnect loop.
    wait_for("the controller to give up", Duration::from_secs(60), || {
        status = controller.try_wait().unwrap();
        status.is_some()
    });
    let output = controller.wait_with_output().unwrap();
    assert_eq!(status.unwrap().code(), Some(3), "{}", text(&output));
    assert!(started.elapsed() < Duration::from_secs(60));
    assert!(follows(&fake) >= 2, "it did reconnect");
    assert!(
        text(&output).contains("reconnecting for 3s brought no new output"),
        "{}",
        text(&output)
    );
    let all = events(&output);
    assert_eq!(all.last().unwrap()["type"], "sprite_unresolved");
    assert_eq!(all.last().unwrap()["run_id"], run.as_str());
    // The run itself was never affected and finishes remotely.
    assert_eq!(fake.record(&run)["phase"], "running");
    std::fs::remove_file(fake.root.join("flap-ops")).unwrap();
    std::fs::write(fake.home().join(".goodflows/go"), "").unwrap();
    wait_for("remote completion", Duration::from_secs(30), || {
        fake.record(&run)["phase"] == "completed"
    });
}

#[test]
fn hooks_and_jobs_never_inherit_the_caller_lock() {
    let fds = r#"for f in /proc/$$/fd/*; do echo \"fd $(readlink $f)\"; done"#;
    let fake = Fake::new(&format!(
        r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "{fds}"
resume = "{fds}"
[[jobs]]
name = "fds"
command = "{fds}"
"#
    ));
    for hook in ["setup", "resume"] {
        let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        let all = events(&output);
        let hook_lines: String = of_type(&all, "sprite_hook_output")
            .iter()
            .filter(|e| e["hook"] == hook)
            .map(|e| e["data"].as_str().unwrap().to_owned())
            .collect();
        let job_lines: String = of_type(&all, "job_output")
            .iter()
            .map(|e| e["data"].as_str().unwrap().to_owned())
            .collect();
        for (what, lines) in [(hook, &hook_lines), ("job", &job_lines)] {
            assert!(
                lines.contains("fd "),
                "{what} listed no descriptors: {lines}"
            );
            assert!(
                !lines
                    .lines()
                    .any(|l| l.contains("/callers/") && l.ends_with("/lock")),
                "{what} inherited the caller lock:\n{lines}"
            );
        }
    }
}

#[test]
fn an_accepted_delete_that_is_still_listed_keeps_refusing_runs() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let destroy = ["sprite", "destroy", "T-1", "--yes", "--json"];
    let refused = |fake: &Fake| {
        let runs = fake.runs();
        let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
        assert_eq!(output.status.code(), Some(1), "{}", text(&output));
        assert!(
            text(&output).contains("destruction of this Sprite is in progress"),
            "{}",
            text(&output)
        );
        assert_eq!(fake.runs(), runs, "no run record");
    };

    // The delete call succeeded, the platform still lists the Sprite.
    fake.arm("destroy-issued");
    let destroyer = spawn_args(
        &fake,
        &destroy,
        &[
            ("FAKE_SPRITE_DESTROY", "accept-linger"),
            ("GOODFLOWS_TEST_DESTROY_POLL_SECS", "60"),
        ],
    );
    fake.wait_reached("destroy-issued");
    assert!(fake.root.join("names").join(&name).exists());
    refused(&fake);
    fake.release("destroy-issued");
    // Still listed while the destroyer polls: still refused.
    std::thread::sleep(Duration::from_secs(3));
    refused(&fake);
    std::fs::write(fake.root.join("finish-delete"), "").unwrap();
    let output = destroyer.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let destroyed = of_type(&all, "sprite_destroyed")[0];
    assert!(destroyed["listings"].as_u64().unwrap() >= 2, "{destroyed}");
    assert!(!fake.root.join("names").join(&name).exists());
    std::fs::remove_file(fake.root.join("finish-delete")).unwrap();

    // Still listed after the polling budget: unresolved, and runs stay refused.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let output = fake.env_run(&destroy, &[("FAKE_SPRITE_DESTROY", "accept-linger")]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("was accepted but the Sprite is still listed")
            && text(&output).contains("may still be in progress"),
        "{}",
        text(&output)
    );
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["destroying"], true, "{}", text(&output));
    refused(&fake);
    std::fs::write(fake.root.join("finish-delete"), "").unwrap();
    wait_for("the delete to finish", Duration::from_secs(10), || {
        !fake.root.join("names").join(&name).exists()
    });
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["state"], "absent", "{}", text(&output));
}

#[test]
fn concurrent_destroys_never_clear_each_others_marker() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let destroy = ["sprite", "destroy", "T-1", "--yes", "--json"];
    // Destroy B holds its claim; destroy A fails and abandons only its own.
    fake.arm("destroy-prepared");
    let b = spawn_args(&fake, &destroy, &[]);
    fake.wait_reached("destroy-prepared");
    let a = fake.env_run(&destroy, &[("FAKE_SPRITE_DESTROY", "fail")]);
    assert_eq!(a.status.code(), Some(3), "{}", text(&a));
    assert!(text(&a).contains("nothing was deleted"), "{}", text(&a));
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["destroying"], true, "{}", text(&output));
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("destruction of this Sprite is in progress"));
    fake.release("destroy-prepared");
    let b = b.wait_with_output().unwrap();
    assert_eq!(b.status.code(), Some(0), "{}", text(&b));
    assert!(!fake.root.join("names").join(&name).exists());
}

#[test]
fn a_killed_destroys_claim_is_swept_only_once_stale() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let destroy = ["sprite", "destroy", "T-1", "--yes", "--json"];
    let admitted = |fake: &Fake| {
        fake.run(&["run", "--sprite", "T-1", "--json"])
            .status
            .code()
    };
    // A destroy whose controller dies inside the window leaves its claim.
    fake.arm("destroy-prepared");
    let mut killed = spawn_args(&fake, &destroy, &[]);
    fake.wait_reached("destroy-prepared");
    killed.kill().unwrap();
    let _ = killed.wait();
    let _ = std::fs::remove_file(fake.root.join("pause/destroy-prepared.reached"));
    let claims = fake.home().join(".goodflows/control/destroying");
    assert_eq!(std::fs::read_dir(&claims).unwrap().count(), 1);
    assert_eq!(admitted(&fake), Some(1));
    // A later failing destroy keeps the fresh claim of the killed one.
    let output = fake.env_run(&destroy, &[("FAKE_SPRITE_DESTROY", "fail")]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert_eq!(admitted(&fake), Some(1));
    // Once that claim is older than the stale age, the next destroy sweeps it.
    for claim in std::fs::read_dir(&claims).unwrap().flatten() {
        std::fs::write(claim.path(), "1").unwrap();
    }
    let output = fake.env_run(&destroy, &[("FAKE_SPRITE_DESTROY", "fail")]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(!claims.exists());
    assert_eq!(admitted(&fake), Some(0));
}

#[test]
fn a_destroy_whose_claim_was_swept_never_issues_its_delete() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let destroy = ["sprite", "destroy", "T-1", "--yes", "--json"];
    let destroys = |fake: &Fake| {
        fake.argv_log()
            .lines()
            .filter(|l| l.starts_with("destroy "))
            .count()
    };
    // Destroy A stalls after its ownership check (a suspended controller);
    // its claim ages past the stale limit.
    fake.arm("destroy-prepared");
    let a = spawn_args(&fake, &destroy, &[]);
    fake.wait_reached("destroy-prepared");
    let claims = fake.home().join(".goodflows/control/destroying");
    for claim in std::fs::read_dir(&claims).unwrap().flatten() {
        std::fs::write(claim.path(), "1").unwrap();
    }
    // Destroy B sweeps it, is refused by the platform and readmits runs.
    let b = fake.env_run(&destroy, &[("FAKE_SPRITE_DESTROY", "fail")]);
    assert_eq!(b.status.code(), Some(3), "{}", text(&b));
    assert!(!claims.exists());
    assert_eq!(destroys(&fake), 1);
    // A wakes up: its claim is gone, so it deletes nothing.
    fake.release("destroy-prepared");
    let a = a.wait_with_output().unwrap();
    assert_eq!(a.status.code(), Some(1), "{}", text(&a));
    assert!(
        text(&a).contains("claim was removed as stale"),
        "{}",
        text(&a)
    );
    assert_eq!(destroys(&fake), 1, "A issued no delete call");
    assert!(fake.root.join("names").join(&name).exists());
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn status_reconciliation_removes_an_empty_destruction_marker() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // An abort died after removing its claim but before the directory.
    let marker = fake.home().join(".goodflows/control/destroying");
    std::fs::create_dir(&marker).unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("destruction of this Sprite is in progress"));
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(events(&output)[0]["destroying"], false, "{}", text(&output));
    assert!(!marker.exists());
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // A marker that still holds a claim is never removed by status.
    std::fs::create_dir(&marker).unwrap();
    std::fs::write(marker.join("00000000-0000-4000-8000-000000000000"), "1").unwrap();
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["destroying"], true, "{}", text(&output));
}

#[test]
fn a_failed_or_unconfirmed_destroy_never_leaves_the_identity_refusing_runs() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let admitted_after = |fake: &Fake, what: &str| {
        let output = fake.run(&["sprite", "status", "T-1", "--json"]);
        assert!(output.status.success(), "{what}: {}", text(&output));
        assert_eq!(events(&output)[0]["destroying"], false, "{what}");
        let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
        assert_eq!(output.status.code(), Some(0), "{what}: {}", text(&output));
    };

    // The platform listing fails between the ownership check (which wrote
    // the destruction marker) and the delete.
    fake.arm("destroy-prepared");
    let destroyer = spawn_args(&fake, &["sprite", "destroy", "T-1", "--yes", "--json"], &[]);
    fake.wait_reached("destroy-prepared");
    assert_eq!(
        events(&fake.run(&["sprite", "status", "T-1", "--json"]))[0]["destroying"],
        true
    );
    std::fs::write(fake.root.join("fail-list"), "").unwrap();
    fake.release("destroy-prepared");
    let output = destroyer.wait_with_output().unwrap();
    std::fs::remove_file(fake.root.join("fail-list")).unwrap();
    assert_ne!(output.status.code(), Some(0), "{}", text(&output));
    assert!(
        text(&output).contains("could not be re-checked"),
        "{}",
        text(&output)
    );
    assert!(fake.root.join("names").join(&name).exists());
    admitted_after(&fake, "failed re-check");

    // The platform refuses the delete call and the Sprite still exists.
    let output = fake.env_run(
        &["sprite", "destroy", "T-1", "--yes", "--json"],
        &[("FAKE_SPRITE_DESTROY", "fail")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("refused by the platform")
            && text(&output).contains("nothing was deleted"),
        "{}",
        text(&output)
    );
    assert!(fake.root.join("names").join(&name).exists());
    admitted_after(&fake, "refused deletion");

    // A server error is no proof that the delete was not accepted: the
    // marker stays and runs stay refused, though the Sprite is still listed.
    let output = fake.env_run(
        &["sprite", "destroy", "T-1", "--yes", "--json"],
        &[("FAKE_SPRITE_DESTROY", "fail-unknown")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("was not confirmed")
            && text(&output).contains("may still be in progress"),
        "{}",
        text(&output)
    );
    assert!(fake.root.join("names").join(&name).exists());
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["destroying"], true, "{}", text(&output));
    let runs = fake.runs();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("destruction of this Sprite is in progress"));
    assert_eq!(fake.runs(), runs);

    // A confirmed destroy still completes.
    let output = fake.run(&["sprite", "destroy", "T-1", "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fake.root.join("names").join(&name).exists());
}

#[test]
fn an_unregistered_descendant_keeps_the_identity_blocked() {
    // The worker dies after spawning the hook but before recording it.
    let fake = Fake::new(&SLOW.replace("resume = \"true\"", "resume = \"sleep 300\""));
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    fake.arm("spawned-hook");
    let mut controller = spawn_run(&fake, "T-1");
    fake.wait_reached("spawned-hook");
    let run = wait_phase(&fake, "resume");
    let record = fake.record(&run);
    assert!(
        record["children"].as_array().unwrap().is_empty(),
        "{record}"
    );
    let worker = record["worker"]["pid"].as_i64().unwrap() as i32;
    unsafe {
        libc::kill(worker, libc::SIGKILL);
    }
    wait_for("worker exit", Duration::from_secs(20), || {
        process_gone(worker)
    });
    controller.kill().unwrap();
    let _ = controller.wait();
    // The unrecorded hook is still in the worker's session: fail closed.
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], true, "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "resume");
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains(&format!("busy with run {run}")));
    // Once the session is gone the loss is confirmed and recorded.
    let _ = Command::new("pkill")
        .args(["-KILL", "-s", &worker.to_string()])
        .status();
    wait_run_gone(&record);
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], false, "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "interrupted");
}

#[test]
fn a_submission_killed_before_its_active_pointer_is_reconciled_without_work() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    fake.arm("accept-record");
    let controller = spawn_run(&fake, "T-1");
    fake.wait_reached("accept-record");
    let reached = fake.root.join("pause/accept-record.reached");
    let mut pid = 0;
    wait_for("paused pid", Duration::from_secs(10), || {
        pid = std::fs::read_to_string(&reached)
            .ok()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(0);
        pid > 0
    });
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    wait_for("submit exit", Duration::from_secs(20), || process_gone(pid));
    let _ = std::fs::remove_file(&reached);
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let run = run_id(&events(&output));
    assert_eq!(fake.record(&run)["phase"], "accepted");
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], false, "{}", text(&output));
    let record = fake.record(&run);
    assert_eq!(record["phase"], "interrupted", "{record}");
    assert!(record["terminal"]["reason"]
        .as_str()
        .unwrap()
        .contains("abandoned without an active pointer"));
    assert!(!fake.run_dir(&run).join("source").exists());
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn an_attached_controller_reports_a_dead_run_instead_of_waiting_forever() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // The worker dies after registering, before starting any hook.
    fake.arm("registered");
    let controller = spawn_run(&fake, "T-1");
    fake.wait_reached("registered");
    let reached = fake.root.join("pause/registered.reached");
    let mut pid = 0;
    wait_for("paused pid", Duration::from_secs(10), || {
        pid = std::fs::read_to_string(&reached)
            .ok()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(0);
        pid > 0
    });
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    wait_for("worker exit", Duration::from_secs(20), || process_gone(pid));
    let _ = std::fs::remove_file(&reached);
    let started = Instant::now();
    let output = controller.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    let last = all.last().unwrap();
    assert_eq!(last["type"], "sprite_unresolved");
    assert!(last["reason"]
        .as_str()
        .unwrap()
        .contains("gone without a terminal result"));
    assert!(of_type(&all, "summary").is_empty());
    let run = run_id(&all);
    assert!(fake.record(&run)["hook"].is_null(), "no hook started");
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], false, "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "interrupted");
}

/// Every `receipt.json` under the project's private Sprite run directory.
fn receipts(fake: &Fake) -> usize {
    let mut count = 0;
    let Ok(scopes) = std::fs::read_dir(fake.project.join(".goodflows/sprites")) else {
        return 0;
    };
    for scope in scopes {
        for run in std::fs::read_dir(scope.unwrap().path()).unwrap() {
            count += run.unwrap().path().join("receipt.json").exists() as usize;
        }
    }
    count
}

#[test]
fn a_platform_organization_other_than_the_requested_one_is_refused_before_any_mutation() {
    let fake = Fake::new(SLOW.replace("sleep 4", "true").as_str());
    // The token's organization, not `-o`, decides where the real CLI acts
    // (live evidence: `sprite list -o goodway --json` reported org.name
    // "thomas-brewer"). The credential value never appears in diagnostics.
    let token = format!("thomas-brewer/org-id/token-id/{SENTINEL}");
    let goodway = [("SPRITE_ORG", "goodway"), ("SPRITE_TOKEN", token.as_str())];
    // Matching platform organization: runs and recovery work as before.
    let output = fake.env_run(&["run", "other", "--sprite", "T-1", "--json"], &goodway);
    assert!(output.status.success(), "{}", text(&output));
    let run = run_id(&events(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    let output = fake.env_run(&["sprite", "status", "T-1", "--json"], &goodway);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(events(&output)[0]["type"], "sprite_status");
    let (runs, receipts_before) = (fake.runs(), receipts(&fake));
    let unknown = "00000000-0000-4000-8000-000000000000";
    // (fault, expected diagnostic, whether absent identities are refused too)
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a str, bool);
    let cases: [Case; 3] = [
        // The listing reports another organization.
        (
            &[("FAKE_SPRITE_REPORTED_ORG", "thomas-brewer")],
            "thomas-brewer",
            true,
        ),
        // The listing names no organization at all.
        (&[("FAKE_SPRITE_LIST_NO_ORG", "1")], "did not report", true),
        // The organization matches but the listed Sprite belongs elsewhere
        // (an absent identity lists nothing that could contradict it).
        (
            &[("FAKE_SPRITE_ENTRY_ORG", "thomas-brewer")],
            "thomas-brewer",
            false,
        ),
    ];
    for (fault, reported, absent) in cases {
        let env: Vec<_> = goodway.iter().chain(fault).copied().collect();
        let commands: [&[&str]; 8] = [
            &["run", "other", "--sprite", "T-1", "--json"],
            // An absent identity would otherwise be created in the wrong org.
            &["run", "other", "--sprite", "T-NEW", "--json"],
            &["sprite", "status", "T-1", "--json"],
            &["sprite", "logs", "T-1", "--run", &run, "--json"],
            &["sprite", "collect", "T-1", "--run", &run, "--json"],
            &["sprite", "cancel", "T-1", "--run", unknown, "--json"],
            &["sprite", "destroy", "T-1", "--yes", "--json"],
            &["sprite", "destroy", "T-NEW", "--yes", "--json"],
        ];
        for args in commands {
            if !absent && args.contains(&"T-NEW") {
                continue;
            }
            let before = fake.argv_lines();
            let output = fake.env_run(args, &env);
            let all = text(&output);
            assert_eq!(output.status.code(), Some(1), "{fault:?} {args:?}: {all}");
            assert!(
                all.contains("goodway") && all.contains(reported),
                "{fault:?} {args:?}: {all}"
            );
            assert!(!all.contains(SENTINEL), "{all}");
            // Only listings reached the platform: no create, exec or destroy.
            for line in fake.argv_log().lines().skip(before) {
                assert!(line.starts_with("list "), "{fault:?} {args:?}: {line}");
            }
            // JSON output stays pure.
            events(&output);
        }
        assert_eq!(fake.runs(), runs, "{fault:?}");
        assert_eq!(receipts(&fake), receipts_before, "{fault:?}");
        assert!(fake.root.join("names").join(&name).exists());
        assert_eq!(fake.homes().len(), 1, "{fault:?}");
    }
    // Once the platform reports the requested organization again, the same
    // Sprite is reused and destroyed normally.
    let output = fake.env_run(&["run", "other", "--sprite", "T-1", "--json"], &goodway);
    assert!(output.status.success(), "{}", text(&output));
    let output = fake.env_run(&["sprite", "destroy", "T-1", "--yes", "--json"], &goodway);
    assert!(output.status.success(), "{}", text(&output));
    assert!(!fake.root.join("names").join(&name).exists());
}

#[test]
fn a_worker_error_after_a_spawn_kills_descendants_before_freeing_the_identity() {
    // The resume hook has descendants still running when the worker errs.
    let fake = Fake::new(&SLOW.replace(
        "resume = \"true\"",
        "resume = \"sh -c 'sleep 300 & wait' & sleep 300\"",
    ));
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    fake.arm("spawned-hook");
    let controller = spawn_run(&fake, "T-1");
    fake.wait_reached("spawned-hook");
    let run = wait_phase(&fake, "resume");
    let worker = fake.record(&run)["worker"]["pid"].as_i64().unwrap();
    let session = || {
        let out = Command::new("pgrep")
            .args(["-s", &worker.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter(|pid| *pid != worker.to_string())
            .count()
    };
    wait_for("hook descendants", Duration::from_secs(20), || {
        session() >= 4
    });
    std::fs::write(fake.root.join("pause/worker-error.fail"), "").unwrap();
    fake.release("spawned-hook");
    let output = controller.wait_with_output().unwrap();
    std::fs::remove_file(fake.root.join("pause/worker-error.fail")).unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let record = fake.record(&run);
    assert_eq!(record["phase"], "interrupted", "{record}");
    assert!(
        record["terminal"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("injected worker error"),
        "{record}"
    );
    // Nothing the hook started survives the terminal record.
    assert_eq!(session(), 0);
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], false, "{}", text(&output));
}

#[test]
fn export_streams_large_files_and_refuses_oversized_results_before_reading_them() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    let results = fake.run_dir(&run).join("results");
    // Larger than the in-memory threshold: hashed and streamed, not buffered.
    let large: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(results.join("large.bin"), &large).unwrap();
    // The attached run collected its result already; collect again afresh.
    std::fs::remove_dir_all(fake.project.join(".goodflows/sprites")).unwrap();
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let bundle = PathBuf::from(events(&output)[0]["bundle"].as_str().unwrap());
    assert_eq!(
        std::fs::read(bundle.join("results/large.bin")).unwrap(),
        large
    );
    std::fs::remove_dir_all(&bundle).unwrap();
    // A (sparse) file over the bundle limit is refused from its metadata.
    std::fs::File::create(results.join("huge.bin"))
        .unwrap()
        .set_len(600 * 1024 * 1024)
        .unwrap();
    let output = fake.run(&["sprite", "collect", "T-1", "--run", &run, "--json"]);
    assert_ne!(output.status.code(), Some(0), "{}", text(&output));
    assert!(
        text(&output).contains("exceed the collection limits"),
        "{}",
        text(&output)
    );
    assert!(!bundle.exists());
}

#[test]
fn a_local_runner_that_cannot_run_on_the_sprite_falls_back_to_the_exact_release() {
    let fake = Fake::new(PLAIN);
    let version = env!("CARGO_PKG_VERSION");
    let arch = std::env::consts::ARCH;
    let exe = std::fs::read(env!("CARGO_BIN_EXE_goodflows")).unwrap();
    let local_id = format!("{version}-{}", &sha256(&exe)[..16]);
    // The published build: same version, a different digest, and it runs.
    let mut release = exe;
    release.extend_from_slice(b"\0release build");
    let release_id = format!("{version}-{}", &sha256(&release)[..16]);
    let archive = {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(release.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("goodflows-v{version}-{arch}-unknown-linux-gnu/goodflows"),
                &release[..],
            )
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    };
    drop(release);
    let (url, routes, requests) = release_server();
    let archive_path = format!("/v{version}/goodflows-v{version}-{arch}-unknown-linux-gnu.tar.gz");
    let env = [
        ("GOODFLOWS_TEST_RELEASE_URL", url.as_str()),
        ("NO_PROXY", "127.0.0.1"),
        ("no_proxy", "127.0.0.1"),
        ("FAKE_SPRITE_BROKEN_RUNNER", local_id.as_str()),
    ];
    // Without a published release the failure is a refusal before any hook.
    let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("does not run on the Sprite")
            && text(&output).contains(&format!("no published Linux {arch} runner")),
        "{}",
        text(&output)
    );
    assert!(of_type(&events(&output), "sprite_hook_output").is_empty());
    assert!(!fake.home().join(".goodflows/runs").exists());
    {
        let mut routes = routes.lock().unwrap();
        routes.push((
            format!("{archive_path}.sha256"),
            200,
            format!("{}  x\n", sha256(&archive)).into_bytes(),
        ));
        routes.push((archive_path.clone(), 200, archive));
    }
    let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let fallback = of_type(&all, "sprite_runner_fallback");
    assert_eq!(fallback.len(), 1, "{}", text(&output));
    assert!(
        fallback[0]["reason"]
            .as_str()
            .unwrap()
            .contains("GLIBC_2.99"),
        "{}",
        fallback[0]
    );
    assert_eq!(fallback[0]["release_runner"], release_id.as_str());
    assert_eq!(fallback[0]["cached"], false);
    let current = std::fs::read_to_string(fake.home().join(".goodflows/runners/current")).unwrap();
    assert_eq!(current.trim(), release_id);
    // The verified release is cached on the controller by version and arch.
    let cache = fake
        .project
        .join(format!(".goodflows/sprites/runners/v{version}-{arch}"));
    assert_eq!(
        std::fs::read_to_string(cache.join("goodflows.sha256"))
            .unwrap()
            .trim(),
        sha256(&std::fs::read(cache.join("goodflows")).unwrap())
    );
    // The next run downloads nothing and uploads nothing: the broken local
    // build is already installed and the cached release is already there.
    let downloads = requests.lock().unwrap().len();
    let uploads = |fake: &Fake| fake.argv_log().matches("install-runner").count();
    let installs = uploads(&fake);
    let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "sprite_runner_fallback")[0]["cached"], true);
    assert_eq!(requests.lock().unwrap().len(), downloads);
    assert_eq!(uploads(&fake), installs, "{}", fake.argv_log());
    // A damaged cache entry is never used: it is downloaded and verified again.
    std::fs::write(cache.join("goodflows"), b"damaged").unwrap();
    let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        of_type(&events(&output), "sprite_runner_fallback")[0]["cached"],
        false
    );
    assert!(requests.lock().unwrap().len() > downloads);
    let cached_run = |fake: &Fake| {
        let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        of_type(&events(&output), "sprite_runner_fallback")[0]["cached"] == true
    };
    assert!(cached_run(&fake), "the re-downloaded copy is cached again");
    // A cache directory others can write to is neither used nor rewritten.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o770)).unwrap();
    assert!(!cached_run(&fake));
    assert!(!cached_run(&fake));
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(cached_run(&fake));
    // A runner tracked by Git is never taken from the cache.
    git(&fake.project, &["add", "-f", ".goodflows/sprites/runners"]);
    assert!(!cached_run(&fake));
    git(
        &fake.project,
        &["rm", "-q", "--cached", "-r", ".goodflows/sprites/runners"],
    );
    assert!(cached_run(&fake));
    // The cache belongs to the config's Git root, not the current directory.
    let sub = fake.project.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let downloads = requests.lock().unwrap().len();
    let mut command = fake.command();
    command.current_dir(&sub).args([
        "--config",
        "../goodflows.toml",
        "run",
        "--sprite",
        "T-1",
        "--json",
    ]);
    for (key, value) in &env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        of_type(&events(&output), "sprite_runner_fallback")[0]["cached"],
        true
    );
    assert_eq!(requests.lock().unwrap().len(), downloads);
    assert!(!sub.join(".goodflows/sprites/runners").exists());
    // Only the exact version was requested.
    assert!(requests.lock().unwrap().contains(&archive_path));
    for path in requests.lock().unwrap().iter() {
        assert!(path.starts_with(&format!("/v{version}/")), "{path}");
    }
}

fn interrupt(child: &std::process::Child) {
    // SAFETY: signalling a child this test spawned and has not reaped.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
}

/// Wait until the fake CLI has been asked for `needle` once more than `before`.
fn wait_call(fake: &Fake, needle: &str, before: usize) {
    wait_for(needle, Duration::from_secs(60), || {
        fake.argv_log().matches(needle).count() > before
    });
}

#[test]
fn stalled_calls_are_bounded_and_ctrl_c_stops_recovery_commands() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    std::fs::write(fake.root.join("hang-ops"), "status").unwrap();
    // A stalled call ends as unresolved (exit 3) once its bound expires.
    let started = Instant::now();
    let output = fake.env_run(
        &["sprite", "status", "T-1", "--json"],
        &[("GOODFLOWS_TEST_CALL_TIMEOUT_SECS", "2")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(
        text(&output).contains("did not finish within 2s"),
        "{}",
        text(&output)
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    // Ctrl-C stops status, logs and collect at once with 128+SIGINT.
    for (op, needle, args) in [
        ("status", " status ", vec!["sprite", "status", "T-1"]),
        (
            "logs",
            " logs ",
            vec!["sprite", "logs", "T-1", "--run", &run],
        ),
        (
            "export",
            " export ",
            vec!["sprite", "collect", "T-1", "--run", &run],
        ),
    ] {
        std::fs::write(fake.root.join("hang-ops"), op).unwrap();
        let before = fake.argv_log().matches(needle).count();
        let child = fake
            .command()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_call(&fake, needle, before);
        let started = Instant::now();
        interrupt(&child);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(130), "{op}: {}", text(&output));
        assert!(started.elapsed() < Duration::from_secs(20), "{op}");
    }
    std::fs::remove_file(fake.root.join("hang-ops")).unwrap();
}

#[test]
fn ctrl_c_during_destroy_removes_its_claim_only_before_the_delete_call() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let destroying = |fake: &Fake| {
        let output = fake.run(&["sprite", "status", "T-1", "--json"]);
        events(&output)[0]["destroying"] == true
    };
    // Before the delete call: the claim is removed and nothing is deleted.
    std::fs::write(fake.root.join("hang-ops"), "check-destroy").unwrap();
    let child = fake
        .command()
        .args(["sprite", "destroy", "T-1", "--yes", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_call(&fake, " check-destroy ", 0);
    assert!(destroying(&fake));
    interrupt(&child);
    let output = child.wait_with_output().unwrap();
    std::fs::remove_file(fake.root.join("hang-ops")).unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output));
    assert!(
        text(&output).contains("claim was removed"),
        "{}",
        text(&output)
    );
    assert!(!fake.argv_log().contains("destroy -o"));
    assert!(!destroying(&fake));
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // After the delete call: the claim stays, so runs stay refused.
    let child = fake
        .command()
        .args(["sprite", "destroy", "T-1", "--yes", "--json"])
        .env("FAKE_SPRITE_DESTROY", "accept-linger")
        .env("GOODFLOWS_TEST_DESTROY_POLL_SECS", "60")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_call(&fake, "destroy -o", 0);
    interrupt(&child);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output));
    assert!(text(&output).contains("claim stays"), "{}", text(&output));
    assert!(destroying(&fake));
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("destruction of this Sprite is in progress"));
    std::fs::write(fake.root.join("finish-delete"), "").unwrap();
}

#[test]
fn ctrl_c_before_submission_submits_nothing() {
    let fake = Fake::new(PLAIN);
    fake.arm("before-submit");
    let child = spawn_run(&fake, "T-1");
    fake.wait_reached("before-submit");
    interrupt(&child);
    // Let the signal land before the pause is released.
    std::thread::sleep(Duration::from_millis(500));
    fake.release("before-submit");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output));
    assert!(
        text(&output).contains("nothing was submitted"),
        "{}",
        text(&output)
    );
    assert!(!fake.argv_log().contains(" submit "));
    assert_eq!(fake.runs(), 0);
}

#[test]
fn a_silent_follow_connection_is_reconnected_without_duplicate_output() {
    // A current runner honours `--skip`; one from before that option ignores
    // it and replays everything, and the controller then skips by count.
    for ignore_skip in [false, true] {
        let fake = Fake::new(&PLAIN.replace(
            "command = \"true\"",
            "command = \"echo first; sleep 5; echo second\"",
        ));
        let mut env = vec![
            ("GOODFLOWS_TEST_FOLLOW_IDLE_SECS", "1"),
            ("GOODFLOWS_TEST_FOLLOW_BUDGET_SECS", "30"),
        ];
        if ignore_skip {
            env.push(("FAKE_SPRITE_IGNORE_SKIP", "1"));
        }
        let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        let all = events(&output);
        let lines: Vec<String> = of_type(&all, "job_output")
            .iter()
            .map(|e| e["data"].as_str().unwrap().trim().to_owned())
            .collect();
        assert_eq!(lines, ["first", "second"], "ignore_skip={ignore_skip}");
        let phases: Vec<&str> = of_type(&all, "sprite_phase")
            .iter()
            .map(|e| e["phase"].as_str().unwrap())
            .collect();
        let unique: std::collections::BTreeSet<_> = phases.iter().collect();
        assert_eq!(phases.len(), unique.len(), "duplicated phases: {phases:?}");
        // The acknowledgement is protocol, never an output event.
        assert!(of_type(&all, "sprite_follow").is_empty());
        let log = fake.argv_log();
        assert!(log.matches(" follow ").count() >= 3, "{log}");
        assert!(log.contains(" --skip "), "{log}");
    }
}

#[test]
fn an_attached_run_whose_follow_connections_all_stall_ends_unresolved() {
    // Every follow session hangs without delivering a byte. Before the fix a
    // connection cut by the idle bound counted as working (it stayed up), so
    // the controller reconnected forever.
    let fake = Fake::new(PLAIN);
    std::fs::write(fake.root.join("hang-ops"), "follow").unwrap();
    let started = Instant::now();
    let mut child = fake
        .command()
        .args(["run", "--sprite", "T-1", "--json"])
        .env("GOODFLOWS_TEST_FOLLOW_IDLE_SECS", "4")
        .env("GOODFLOWS_TEST_FOLLOW_BUDGET_SECS", "3")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let finished = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    let output = child.wait_with_output().unwrap();
    std::fs::remove_file(fake.root.join("hang-ops")).unwrap();
    assert!(
        finished,
        "the controller kept reconnecting: {}",
        text(&output)
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    let unresolved = of_type(&all, "sprite_unresolved");
    assert_eq!(unresolved.len(), 1, "{}", text(&output));
    assert!(unresolved[0]["run_id"].is_string());
    // Budget (3 s) + one idle-bounded connection (4 s) + start-up slack.
    assert!(started.elapsed() < Duration::from_secs(40));
}

#[test]
fn the_remote_config_is_the_snapshot_copy_even_if_the_file_changes_later() {
    let config = PLAIN.replace("command = \"true\"", "command = \"echo original\"");
    let fake = Fake::new(&config);
    fake.arm("snapshot-taken");
    let child = spawn_run(&fake, "T-1");
    fake.wait_reached("snapshot-taken");
    fake.write(
        "goodflows.toml",
        &PLAIN.replace("command = \"true\"", "command = \"echo changed\""),
    );
    fake.release("snapshot-taken");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    assert_eq!(of_type(&all, "job_output")[0]["data"], "original\n");
    let source = fake.run_dir(&run_id(&all)).join("source");
    assert_eq!(
        std::fs::read_to_string(source.join("goodflows.toml")).unwrap(),
        config
    );
}

#[test]
fn an_unreadable_boot_identity_never_proves_a_run_gone() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 300"));
    let mut controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    controller.kill().unwrap();
    let _ = controller.wait();
    let record = fake.record(&run);
    std::fs::write(fake.root.join("pause/boot-id-unreadable.fail"), "").unwrap();
    // Every process of the run is gone, but this boot's identity is unknown.
    // Kill the session until it is empty: a process forked during one kill
    // pass (the runner may still be starting its job) is caught by the next.
    let session = record["worker"]["pid"].as_i64().unwrap().to_string();
    wait_for("an empty run session", Duration::from_secs(20), || {
        let _ = Command::new("pkill")
            .args(["-KILL", "-s", &session])
            .status();
        std::thread::sleep(Duration::from_millis(100));
        // Only zombies (or nothing) left in the session.
        Command::new("ps")
            .args(["-o", "stat=", "-s", &session])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .all(|stat| stat.trim_start().starts_with('Z'))
            })
    });
    wait_run_gone(&record);
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], true, "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "running");
    // Once it is readable again the loss is proven and recorded.
    std::fs::remove_file(fake.root.join("pause/boot-id-unreadable.fail")).unwrap();
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["busy"], false, "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "interrupted");
}

#[test]
fn a_new_sprite_is_initialized_only_once_it_is_listed_with_its_id() {
    let fake = Fake::new(PLAIN);
    // Listed only on the fourth listing after its create: retried, then owned.
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[("FAKE_SPRITE_LIST_LAG", "3")],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let owner: Value =
        serde_json::from_slice(&std::fs::read(fake.home().join(".goodflows/owner.json")).unwrap())
            .unwrap();
    assert!(
        owner["sprite_id"].as_str().unwrap().starts_with("id-"),
        "{owner}"
    );
    // Never listed with an id: refused before ownership is recorded.
    let output = fake.env_run(
        &["run", "--sprite", "T-2", "--json"],
        &[("FAKE_SPRITE_LIST_NO_ID", "1")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("did not list its resource id"),
        "{}",
        text(&output)
    );
    let owned = fake
        .homes()
        .iter()
        .filter(|h| h.join(".goodflows/owner.json").exists())
        .count();
    assert_eq!((fake.homes().len(), owned), (2, 1));
}

#[test]
fn a_lost_ownership_check_acknowledgement_is_unresolved_not_a_refusal() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let name = of_type(&events(&output), "sprite_run")[0]["sprite"]
        .as_str()
        .unwrap()
        .to_owned();
    // prepare-destroy runs on the Sprite (its claim is written), but the
    // controller never sees the reply.
    let output = fake.env_run(
        &["sprite", "destroy", "T-1", "--yes", "--json"],
        &[("FAKE_SPRITE_LOSE_ACK_OPS", "prepare-destroy")],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let all = events(&output);
    let unresolved = of_type(&all, "sprite_unresolved");
    assert!(
        unresolved[0]["reason"]
            .as_str()
            .unwrap()
            .contains("ownership check of"),
        "{}",
        text(&output)
    );
    assert!(!fake.argv_log().contains("destroy -o"));
    assert!(fake.root.join("names").join(&name).exists());
    // The claim it may have left is reported and keeps refusing runs.
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["destroying"], true, "{}", text(&output));
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("destruction of this Sprite is in progress"));
    // A later destroy still completes.
    let output = fake.run(&["sprite", "destroy", "T-1", "--yes", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn an_unused_absolute_top_level_workdir_never_blocks_a_workflows_only_config() {
    // The shared workdir is outside the Git root, but no top-level job uses
    // it and the only workflow sets its own.
    let fake = Fake::new(
        r#"
[settings]
sprite_prefix = "gft"
workdir = "/"
[sprite]
setup = "true"
resume = "true"
[workflows.check.settings]
workdir = "sub"
[[workflows.check.jobs]]
name = "where"
command = "printf '%s' \"${PWD##*/}\""
"#,
    );
    fake.write("sub/README", "sub\n");
    let local = fake.run(&["run", "check", "--json"]);
    assert_eq!(local.status.code(), Some(0), "{}", text(&local));
    assert_eq!(of_type(&events(&local), "job_output")[0]["data"], "sub");
    let output = fake.run(&["run", "check", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(of_type(&events(&output), "job_output")[0]["data"], "sub");
}

/// Children of `parent` that have exited but were never reaped.
fn zombie_children(parent: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let Some((_, rest)) = stat.rsplit_once(')') else {
                return false;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            fields.first() == Some(&"Z") && fields.get(1) == Some(&parent.to_string().as_str())
        })
        .collect()
}

#[test]
fn a_failed_worker_handshake_kills_and_reaps_the_worker() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let supervisor: Value = serde_json::from_slice(
        &std::fs::read(fake.home().join(".goodflows/control/supervisor.json")).unwrap(),
    )
    .unwrap();
    let supervisor = supervisor["pid"].as_u64().unwrap() as u32;
    std::fs::write(fake.root.join("pause/worker-handshake.fail"), "").unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    std::fs::remove_file(fake.root.join("pause/worker-handshake.fail")).unwrap();
    assert_ne!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    // The spawned worker was killed and reaped by the supervisor.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        zombie_children(supervisor).is_empty(),
        "{:?}",
        zombie_children(supervisor)
    );
    let worker_alive = std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .is_ok_and(|c| c.windows(run.len()).any(|w| w == run.as_bytes()))
    });
    assert!(!worker_alive, "a worker for {run} is still running");
    // The identity is usable again.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
}

#[test]
fn a_failed_release_diagnostic_never_skips_the_hold_release() {
    let fake = Fake::new(PLAIN);
    std::fs::write(fake.root.join("pause/release-diagnostics.fail"), "").unwrap();
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    std::fs::remove_file(fake.root.join("pause/release-diagnostics.fail")).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let run = run_id(&events(&output));
    let release = format!("DELETE /v1/tasks/gf-{run}");
    wait_for("the hold release", Duration::from_secs(20), || {
        fake.api
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with(&release))
    });
    let log = std::fs::read_to_string(fake.run_dir(&run).join("hold.jsonl")).unwrap_or_default();
    assert!(
        log.lines()
            .any(|l| l.contains("\"op\":\"release\"") && l.contains("\"active_cleared\":null")),
        "{log}"
    );
}

#[test]
fn a_line_cut_at_a_dropped_connection_is_never_counted_as_printed() {
    // The stream drops halfway through the native summary line. The fragment
    // must not count as printed, or the reconnect (`--skip`, or count-based
    // skipping of a full replay) would leave the summary out.
    for ignore_skip in [false, true] {
        let fake = Fake::new(&PLAIN.replace(
            "command = \"true\"",
            "command = \"echo before; sleep 1; echo after\"",
        ));
        std::fs::write(fake.root.join("cut-summary"), "").unwrap();
        let mut env = vec![];
        if ignore_skip {
            env.push(("FAKE_SPRITE_IGNORE_SKIP", "1"));
        }
        let output = fake.env_run(&["run", "--sprite", "T-1", "--json"], &env);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        assert!(
            !fake.root.join("cut-summary").exists(),
            "the cut was never applied"
        );
        let all = events(&output);
        assert_eq!(
            of_type(&all, "summary").len(),
            1,
            "ignore_skip={ignore_skip}: {}",
            text(&output)
        );
        assert_eq!(all.last().unwrap()["type"], "summary");
        let lines: Vec<&str> = of_type(&all, "job_output")
            .iter()
            .map(|e| e["data"].as_str().unwrap().trim())
            .collect();
        assert_eq!(lines, ["before", "after"]);
        assert!(fake.argv_log().matches(" follow ").count() >= 2);
    }
}

#[test]
fn a_transient_hello_failure_never_triggers_the_release_fallback() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // No release is published: a fallback would fail the run.
    let (url, _routes, requests) = release_server();
    std::fs::write(fake.root.join("fail-once-ops"), "hello").unwrap();
    let output = fake.env_run(
        &["run", "--sprite", "T-1", "--json"],
        &[
            ("GOODFLOWS_TEST_RELEASE_URL", &url),
            ("NO_PROXY", "127.0.0.1"),
            ("no_proxy", "127.0.0.1"),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fake.root.join("fail-once-ops").exists());
    assert!(of_type(&events(&output), "sprite_runner_fallback").is_empty());
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn a_dead_runs_renewal_failure_marker_is_cleared_by_reconciliation() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 300"));
    let mut controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    controller.kill().unwrap();
    let _ = controller.wait();
    let goodflows = fake.home().join(".goodflows");
    std::fs::write(fake.root.join("api.fail"), "").unwrap();
    wait_for("degraded marker", Duration::from_secs(10), || {
        goodflows.join("control/hold-degraded").exists()
    });
    // The worker dies before it can finish (and clear its marker).
    let record = fake.record(&run);
    let session = record["worker"]["pid"].as_i64().unwrap().to_string();
    wait_for("an empty run session", Duration::from_secs(20), || {
        let _ = Command::new("pkill")
            .args(["-KILL", "-s", &session])
            .status();
        std::thread::sleep(Duration::from_millis(100));
        Command::new("ps")
            .args(["-o", "stat=", "-s", &session])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .all(|stat| stat.trim_start().starts_with('Z'))
            })
    });
    wait_run_gone(&record);
    std::fs::remove_file(fake.root.join("api.fail")).unwrap();
    assert!(goodflows.join("control/hold-degraded").exists());
    assert!(!fake.record(&run)["terminal"].is_object());
    // The next admission reconciles the dead run and drops only its marker.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(fake.record(&run)["phase"], "interrupted");
    assert!(!goodflows.join("control/hold-degraded").exists());
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["degraded"], false, "{}", text(&output));
}

#[test]
fn a_renewal_failure_during_finalization_never_leaves_admission_blocked() {
    let fake = Fake::new(PLAIN);
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    // The next run pauses after its terminal record and pointer cleanup,
    // while its hold renewal keeps running; renewal then fails.
    fake.arm("pointer-cleared");
    let controller = spawn_run(&fake, "T-1");
    fake.wait_reached("pointer-cleared");
    let run = std::fs::read_dir(fake.home().join(".goodflows/runs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|id| {
            fake.try_record(id)
                .is_some_and(|r| r["phase"] == "completed" && r["submitter"].is_null())
                && !std::fs::read_to_string(fake.run_dir(id).join("hold.jsonl"))
                    .unwrap_or_default()
                    .contains("\"op\":\"release\"")
        })
        .expect("the paused run");
    let hold_log = fake.run_dir(&run).join("hold.jsonl");
    let failed_renewals = || {
        std::fs::read_to_string(&hold_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["op"] == "renew" && e["ok"] == false)
            .count()
    };
    let renewals = || {
        std::fs::read_to_string(&hold_log)
            .unwrap_or_default()
            .matches("\"renew\"")
            .count()
    };
    let renewals_at_pause = renewals();
    std::fs::write(fake.root.join("api.fail"), "").unwrap();
    // Two renewal intervals (3 s each): a renewal that was still running
    // after the terminal record would have failed by now.
    std::thread::sleep(Duration::from_secs(7));
    let failed_while_paused = failed_renewals();
    fake.release("pointer-cleared");
    let release = format!("DELETE /v1/tasks/gf-{run}");
    wait_for("the hold release", Duration::from_secs(20), || {
        fake.api
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with(&release))
    });
    std::fs::remove_file(fake.root.join("api.fail")).unwrap();
    let controller = controller.wait_with_output().unwrap();
    assert_eq!(controller.status.code(), Some(0), "{}", text(&controller));
    // The finished run blocks nothing: a new run is admitted.
    let output = fake.run(&["run", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!fake
        .home()
        .join(".goodflows/control/hold-degraded")
        .exists());
    // Renewal stopped once the terminal record was durable.
    assert_eq!(failed_while_paused, 0);
    assert_eq!(renewals(), renewals_at_pause);
}

#[test]
fn a_marker_left_by_a_worker_killed_after_its_terminal_record_is_cleared_on_admission() {
    let fake = Fake::new(&SLOW.replace("sleep 4", "sleep 300"));
    fake.arm("terminal-written");
    let controller = spawn_run(&fake, "T-1");
    let run = wait_running(&fake);
    let goodflows = fake.home().join(".goodflows");
    let marker = goodflows.join("control/hold-degraded");
    // Renewal fails: the marker is written and the run is stopped (bounded
    // cleanup), which finalizes it as cancelled.
    std::fs::write(fake.root.join("api.fail"), "").unwrap();
    let hold_log = fake.run_dir(&run).join("hold.jsonl");
    wait_for("a failed renewal", Duration::from_secs(20), || {
        std::fs::read_to_string(&hold_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|e| e["op"] == "renew" && e["ok"] == false)
    });
    // The renewal thread logs the failure before it writes the marker.
    wait_for("the degraded marker", Duration::from_secs(10), || {
        marker.exists()
    });
    fake.wait_reached("terminal-written");
    // The worker dies with its terminal record durable but before it clears
    // the marker, the active pointer or its hold.
    let reached = fake.root.join("pause/terminal-written.reached");
    let worker: i32 = std::fs::read_to_string(&reached)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        fake.record(&run)["worker"]["pid"].as_i64(),
        Some(worker as i64)
    );
    unsafe {
        libc::kill(worker, libc::SIGKILL);
    }
    wait_for("worker exit", Duration::from_secs(20), || {
        process_gone(worker)
    });
    fake.release("terminal-written");
    std::fs::remove_file(fake.root.join("api.fail")).unwrap();
    let _ = controller.wait_with_output().unwrap();
    assert_eq!(fake.record(&run)["phase"], "cancelled");
    assert!(
        marker.exists(),
        "the killed worker never cleared its marker"
    );
    assert_eq!(active_pointer(&goodflows).as_deref(), Some(run.as_str()));
    // The next admission reconciles the finished run, drops its marker and
    // admits the new run.
    let output = fake.run(&["run", "other", "--sprite", "T-1", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(!marker.exists());
    assert_eq!(fake.record(&run)["phase"], "cancelled");
    let output = fake.run(&["sprite", "status", "T-1", "--json"]);
    assert_eq!(events(&output)[0]["degraded"], false, "{}", text(&output));
}

// ---------------------------------------------------------------------------
// Run records.

const RUN_RECORDS: &str = r#"
[settings]
sprite_prefix = "gft"
history = true
[sprite]
setup = "true"
resume = "true"
pass_env = ["GITHUB_ACTIONS", "GITHUB_EVENT_NAME", "GITHUB_RUN_ID"]
[reporting]
project = "demo"
history = true

[[workflows.ci.jobs]]
name = "show"
command = "env"
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
depends_on = ["show"]

[[workflows.suite.jobs]]
name = "test"
command = "env"
"#;

const RUNS_PATH: &str = "/api/v1/projects/demo/workflow-runs";
const CI_ENV: [(&str, &str); 3] = [
    ("GITHUB_ACTIONS", "true"),
    ("GITHUB_EVENT_NAME", "push"),
    ("GITHUB_RUN_ID", "4242"),
];

fn run_records(mock: &common::mock::Mock) -> Vec<Value> {
    mock.requests()
        .iter()
        .filter(|r| r.path == RUNS_PATH)
        .map(|r| r.json())
        .collect()
}

#[test]
fn remote_run_records_are_frozen_remotely_and_published_after_collection() {
    use common::mock::{Mock, Reply};
    let fake = Fake::new(RUN_RECORDS);
    let mock = Mock::store();
    let output = fake.env_run(
        &["run", "ci", "--sprite", "T-1", "--json"],
        &[
            CI_ENV[0],
            CI_ENV[1],
            CI_ENV[2],
            ("GOODFLOWS_URL", &mock.url),
            ("GOODFLOWS_API_KEY", SENTINEL),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let run = run_id(&all);
    let child = all.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "integration")
        .unwrap()["child_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Each remote invocation froze its record without a key and sent nothing.
    let collected = all
        .iter()
        .position(|e| e["type"] == "sprite_collected")
        .unwrap();
    let remote: Vec<&str> = all[..collected]
        .iter()
        .filter(|e| e["type"] == "run_upload_pending")
        .map(|e| {
            assert_eq!(e["reason"], "credentials_unavailable", "{e}");
            e["run_id"].as_str().unwrap()
        })
        .collect();
    assert_eq!(remote, [child.as_str(), run.as_str()]);
    // The controller sent both after collection: the child's, then the root's.
    let uploaded: Vec<(usize, &str)> = all
        .iter()
        .enumerate()
        .filter(|(_, e)| e["type"] == "run_uploaded")
        .map(|(i, e)| (i, e["run_id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        uploaded.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        [child.as_str(), run.as_str()]
    );
    assert!(uploaded.iter().all(|(i, _)| *i > collected));
    let records = run_records(&mock);
    assert_eq!(records.len(), 2);
    assert_eq!(
        (records[0]["run_id"].as_str(), records[1]["run_id"].as_str()),
        (Some(child.as_str()), Some(run.as_str()))
    );
    assert_eq!(records[0]["parent_run_id"], run.as_str());
    assert_eq!(records[0]["caller_job"], "integration");
    // Both carry the snapshot's provenance as `source`, and the forwarded
    // GitHub variables as `ci`.
    let provenance = fake.record(&run)["provenance"].clone();
    let commit = provenance["commit"].as_str().unwrap().to_owned();
    let source =
        serde_json::json!({"commit": commit, "dirty": false, "branch": provenance["branch"]});
    assert!(provenance["branch"].is_string(), "{provenance}");
    for record in &records {
        assert_eq!(record["source"], source, "{record}");
        assert_eq!(
            record["ci"],
            serde_json::json!({"provider": "github_actions", "event": "push", "run_id": "4242"})
        );
    }
    // The root and child history files have the same source; jobs never
    // saw the commit.
    let summaries = of_type(&all, "summary");
    assert_eq!(summaries.len(), 2);
    for summary in summaries {
        let path = summary["history_file"].as_str().unwrap();
        let history: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(history["source"], source, "{history}");
    }
    let job_output: String = of_type(&all, "job_output")
        .iter()
        .map(|e| e["data"].as_str().unwrap().to_owned())
        .collect();
    assert!(job_output.contains("GITHUB_RUN_ID=4242"), "{job_output}");
    assert!(!job_output.contains(&commit), "{job_output}");
    assert!(!fake.argv_log().contains(&commit));
    assert!(!fake.argv_log().contains(SENTINEL));

    // Without forwarding there is no `ci`; the edited config ships untracked
    // bytes, so the archive is dirty. A 503 never changes the exit code.
    fake.write(
        "goodflows.toml",
        &RUN_RECORDS.replace(
            "pass_env = [\"GITHUB_ACTIONS\", \"GITHUB_EVENT_NAME\", \"GITHUB_RUN_ID\"]\n",
            "",
        ),
    );
    let unavailable = Mock::start(|_, _| {
        Reply::Status(503, vec![("Retry-After".into(), "0".into())], String::new())
    });
    let output = fake.env_run(
        &["run", "ci", "--sprite", "T-1", "--json"],
        &[
            CI_ENV[0],
            CI_ENV[1],
            CI_ENV[2],
            ("GOODFLOWS_URL", &unavailable.url),
            ("GOODFLOWS_API_KEY", SENTINEL),
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    let pending: Vec<&Value> = of_type(&all, "run_upload_pending")
        .into_iter()
        .filter(|e| e["reason"] == "service_unavailable")
        .collect();
    assert_eq!(pending.len(), 2, "{all:?}");
    let records = run_records(&unavailable);
    assert!(!records.is_empty());
    for record in &records {
        assert!(record.get("ci").is_none(), "{record}");
        assert_eq!(record["source"]["dirty"], true, "{record}");
    }
}

#[test]
fn a_spool_listing_a_run_record_needs_the_trusted_opt_in() {
    use common::mock::Mock;
    let config = r#"
[settings]
sprite_prefix = "gft"
[sprite]
setup = "true"
resume = "true"
[reporting]
project = "demo"
[[jobs]]
name = "test"
command = 'cp report.jsonl "$GOODFLOWS_REPORT_DIR/r1.jsonl"'
reports = ["test-timings"]
"#;
    let fake = Fake::new(config);
    fake.write("report.jsonl", &common::timing_report("r1", None));
    let mock = Mock::store();
    // The controller trusts the config it loaded; the copy that ships, and
    // that the remote runner uses, turns run-record upload on.
    fake.arm("snapshot-copy-.gitignore");
    let child = spawn_args(
        &fake,
        &["run", "--sprite", "T-1", "--json"],
        &[
            ("GOODFLOWS_URL", &mock.url),
            ("GOODFLOWS_API_KEY", SENTINEL),
        ],
    );
    fake.wait_reached("snapshot-copy-.gitignore");
    fake.write(
        "goodflows.toml",
        &config.replace("project = \"demo\"", "project = \"demo\"\nhistory = true"),
    );
    fake.release("snapshot-copy-.gitignore");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let all = events(&output);
    // The remote runner froze a record, so the spool lists it.
    assert!(of_type(&all, "run_upload_pending")
        .iter()
        .any(|e| e["reason"] == "credentials_unavailable"));
    let refused = of_type(&all, "sprite_publication_refused");
    assert_eq!(refused.len(), 1, "{all:?}");
    assert!(
        refused[0]["reason"]
            .as_str()
            .unwrap()
            .contains("[reporting] history = true"),
        "{}",
        refused[0]
    );
    // Neither the run record nor the spool's test report is sent.
    assert!(mock.requests().is_empty());
}

#[test]
fn a_detached_runs_record_is_published_by_reports_upload() {
    use common::mock::Mock;
    let fake = Fake::new(&RUN_RECORDS.replace("command = \"env\"", "command = \"sleep 2\""));
    let mut controller = spawn_args(
        &fake,
        &["run", "ci", "--sprite", "T-1", "--json"],
        &[CI_ENV[0], CI_ENV[1], CI_ENV[2]],
    );
    let run = wait_running(&fake);
    controller.kill().unwrap();
    let _ = controller.wait();
    wait_for("remote completion", Duration::from_secs(60), || {
        fake.record(&run)["phase"] == "completed"
    });
    let target = fake.root.join("collected");
    let output = fake.run(&[
        "sprite",
        "collect",
        "T-1",
        "--run",
        &run,
        "--output",
        target.to_str().unwrap(),
        "--json",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    let spool = target.join("result/results/reports").join(&run);
    let frozen = std::fs::read(spool.join("run-record.json")).unwrap();
    let mock = Mock::store();
    let outside = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_goodflows"))
        .current_dir(outside.path())
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SENTINEL)
        .args(["reports", "upload", "--json"])
        .arg(&spool)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let summary = events(&output).last().unwrap().clone();
    assert_eq!(summary["run_record"], "acknowledged", "{summary}");
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, RUNS_PATH);
    assert_eq!(requests[0].body, frozen);
    assert_eq!(
        requests[0].json()["source"]["commit"],
        fake.record(&run)["provenance"]["commit"]
    );
}
