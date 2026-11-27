//! goodflows nextest reporter: runs a cargo-nextest command, then writes the
//! JUnit report nextest wrote as one `test-timings/1` JSON Lines report for
//! goodflows to collect. It exits with the command's exit code; a reporter
//! failure prints one warning and never changes it.

mod convert;
mod time;

use std::collections::{hash_map::RandomState, HashMap};
use std::ffi::OsString;
use std::hash::BuildHasher;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use serde_json::Value;

const USAGE: &str = "usage: goodflows-nextest-reporter [--junit <path>] -- <command> [args...]

Runs the command (usually `cargo nextest run ...`), then converts the JUnit
report it wrote into a goodflows test-timings/1 report in GOODFLOWS_REPORT_DIR
(default .goodflows/test-timings). Exits with the command's exit code.

  --junit <path>  The JUnit file to read. By default it is found from the
                  nextest profile's [profile.<name>.junit] path.";

/// Set when this process receives SIGINT or SIGTERM while the command runs.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn main() -> ExitCode {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let (junit, command) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(if message == USAGE { 0 } else { 2 });
        }
    };
    catch_interrupts();
    let started = SystemTime::now();
    let status = match Command::new(&command[0]).args(&command[1..]).status() {
        Ok(status) => status,
        Err(error) => {
            eprintln!(
                "error: goodflows-nextest-reporter could not run {}: {error}",
                command[0].to_string_lossy()
            );
            return ExitCode::from(127);
        }
    };
    let (code, signaled) = exit_code(status);
    let interrupted = signaled || INTERRUPTED.load(Ordering::SeqCst);
    let command: Vec<String> = command
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Err(error) = report(junit, &command, started, status.code(), interrupted) {
        warn(&format!("stopped writing: {error}"));
    }
    ExitCode::from(code)
}

fn parse_args(args: &[OsString]) -> Result<(Option<PathBuf>, Vec<OsString>), String> {
    let mut junit = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let arg = arg.to_string_lossy();
        if arg == "--" {
            let command: Vec<OsString> = rest.cloned().collect();
            if command.is_empty() {
                return Err(format!("error: no command after --\n\n{USAGE}"));
            }
            return Ok((junit, command));
        } else if arg == "-h" || arg == "--help" {
            return Err(USAGE.to_owned());
        } else if arg == "--junit" {
            let path = rest
                .next()
                .ok_or(format!("error: --junit needs a path\n\n{USAGE}"))?;
            junit = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--junit=") {
            junit = Some(PathBuf::from(path));
        } else {
            return Err(format!("error: unexpected argument {arg}\n\n{USAGE}"));
        }
    }
    Err(format!("error: missing -- <command>\n\n{USAGE}"))
}

fn exit_code(status: ExitStatus) -> (u8, bool) {
    if let Some(code) = status.code() {
        return (u8::try_from(code).unwrap_or(1), false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return (u8::try_from(128 + signal).unwrap_or(1), true);
        }
    }
    (1, true)
}

/// Ctrl-C (or goodflows cancelling the job) reaches nextest too, which stops
/// and writes its JUnit report; this process waits for it and records the run
/// as interrupted. A handler rather than SIG_IGN, so the command does not
/// inherit an ignored signal.
fn catch_interrupts() {
    #[cfg(unix)]
    {
        extern "C" fn record(_: libc::c_int) {
            INTERRUPTED.store(true, Ordering::SeqCst);
        }
        let handler = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }
}

fn warn(message: &str) {
    eprintln!("warning: goodflows-nextest-reporter {message}");
}

fn report(
    junit: Option<PathBuf>,
    command: &[String],
    started: SystemTime,
    exit_code: Option<i32>,
    interrupted: bool,
) -> Result<(), String> {
    // Without --junit the workspace is required, to find the JUnit file.
    let workspace = match (workspace(command), &junit) {
        (Ok(workspace), _) => Some(workspace),
        (Err(error), None) => return Err(error),
        (Err(_), Some(_)) => None,
    };
    let junit = match (junit, &workspace) {
        (Some(path), _) => path,
        (None, Some(workspace)) => junit_path(command, &workspace.root)?,
        (None, None) => unreachable!("returned above"),
    };
    let started_us = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64);
    let report_id = format!(
        "{}-{}-{:08x}",
        started_us / 1000,
        std::process::id(),
        RandomState::new().hash_one(started_us) as u32
    );
    let suite_id = ["TEST_TIMING_SUITE_ID", "GOODFLOWS_RUN_ID"]
        .iter()
        .filter_map(|name| env::var(name).ok())
        .find(|id| identifier(id))
        .unwrap_or_else(|| report_id.clone());
    let (partition, partition_count) = partition(command);
    let run = convert::Run {
        suite_id,
        report_id: report_id.clone(),
        partition,
        partition_count,
        runner_version: runner_version(),
        files: workspace.map(|w| w.files).unwrap_or_default(),
        started_us,
        exit_code,
        interrupted,
    };

    // A JUnit file older than this run is a previous run's. Two seconds of
    // slack covers file systems that store whole-second times.
    let fresh = fs::metadata(&junit)
        .and_then(|m| m.modified())
        .is_ok_and(|modified| modified + Duration::from_secs(2) >= started);
    let records = if fresh {
        let xml = fs::read_to_string(&junit)
            .map_err(|e| format!("could not read {}: {e}", junit.display()))?;
        convert::records(&run, &xml).map_err(|e| format!("{}: {e}", junit.display()))?
    } else {
        warn(&format!(
            "found no JUnit report from this run at {}, so the run is recorded as aborted",
            junit.display()
        ));
        convert::aborted(&run)
    };
    write(&report_id, &records)
}

fn write(report_id: &str, records: &[Value]) -> Result<(), String> {
    let directory = env::var_os("GOODFLOWS_REPORT_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(".goodflows").join("test-timings"));
    fs::create_dir_all(&directory)
        .map_err(|e| format!("could not create {}: {e}", directory.display()))?;
    let path = directory.join(format!("{report_id}.jsonl"));
    let mut body = String::new();
    for record in records {
        body.push_str(&record.to_string());
        body.push('\n');
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// The value of an option given as `--name value` or `--name=value`, before
/// any `--` that starts the test binaries' own arguments.
fn option<'a>(command: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut args = command.iter().take_while(|a| *a != "--");
    while let Some(arg) = args.next() {
        for name in names {
            if arg == name {
                return args.next().map(String::as_str);
            }
            if let Some(value) = arg.strip_prefix(name).and_then(|v| v.strip_prefix('=')) {
                return Some(value);
            }
        }
    }
    None
}

/// goodflows's partition when it set both variables validly, then nextest's
/// `--partition <kind>:<m>/<n>`.
fn partition(command: &[String]) -> (Option<String>, u64) {
    let count = |value: &str| {
        value
            .parse::<u64>()
            .ok()
            .filter(|n| (1..=(1 << 53) - 1).contains(n))
    };
    let valid = |p: &str| {
        (1..=64).contains(&p.len())
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
    };
    if let (Ok(p), Some(n)) = (
        env::var("GOODFLOWS_PARTITION"),
        env::var("GOODFLOWS_PARTITION_COUNT")
            .ok()
            .as_deref()
            .and_then(count),
    ) {
        if valid(&p) {
            return (Some(p), n);
        }
    }
    let shard = option(command, &["--partition"])
        .and_then(|v| v.split_once(':'))
        .and_then(|(_, shard)| shard.split_once('/'))
        .and_then(|(m, n)| Some((count(m)?, count(n)?)));
    match shard {
        Some((m, n)) => (Some(m.to_string()), n),
        None => (None, 1),
    }
}

fn identifier(id: &str) -> bool {
    id.len() <= 128
        && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
}

fn runner_version() -> Option<String> {
    let output = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    // "cargo-nextest 0.9.152 (26ebeda79 2026-11-13)"
    output
        .status
        .success()
        .then(|| stdout.split_whitespace().nth(1).map(str::to_owned))?
}

struct Workspace {
    root: PathBuf,
    /// Source file of each test binary, relative to the root, by nextest
    /// binary id.
    files: HashMap<String, String>,
}

/// The cargo workspace the command tests, from `cargo metadata`.
fn workspace(command: &[String]) -> Result<Workspace, String> {
    let mut metadata = Command::new("cargo");
    metadata.args(["metadata", "--no-deps", "--format-version", "1"]);
    if let Some(manifest) = option(command, &["--manifest-path"]) {
        metadata.args(["--manifest-path", manifest]);
    }
    let output = metadata
        .output()
        .map_err(|e| format!("could not run cargo metadata: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo metadata failed: {}", stderr.trim()));
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cargo metadata output: {e}"))?;
    let root = PathBuf::from(
        json["workspace_root"]
            .as_str()
            .ok_or("cargo metadata has no workspace_root")?,
    );
    Ok(Workspace {
        files: binary_files(&json, &root),
        root,
    })
}

/// nextest names each test binary `<package>` (the library),
/// `<package>::<test target>`, or `<package>::<kind>/<target>` for binaries,
/// examples and benchmarks.
fn binary_files(metadata: &Value, root: &Path) -> HashMap<String, String> {
    let mut files = HashMap::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let Some(name) = package["name"].as_str() else {
            continue;
        };
        for target in package["targets"].as_array().into_iter().flatten() {
            let (Some(target_name), Some(src)) =
                (target["name"].as_str(), target["src_path"].as_str())
            else {
                continue;
            };
            let kinds: Vec<&str> = target["kind"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let id = match kinds.first().copied() {
                Some("test") => format!("{name}::{target_name}"),
                Some(kind @ ("bin" | "example" | "bench")) => {
                    format!("{name}::{kind}/{target_name}")
                }
                Some("lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro") => {
                    name.to_owned()
                }
                _ => continue,
            };
            if let Ok(relative) = Path::new(src).strip_prefix(root) {
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect();
                files.insert(id, parts.join("/"));
            }
        }
    }
    files
}

/// `<root>/<store dir>/<profile>/<junit path>`, read from nextest's
/// repository config the way nextest resolves it: the profile from
/// `--profile`/`-P` or `NEXTEST_PROFILE`, its junit path or the one it
/// inherits, and `[store] dir`.
fn junit_path(command: &[String], root: &Path) -> Result<PathBuf, String> {
    let profile = option(command, &["--profile", "-P"])
        .map(str::to_owned)
        .or_else(|| env::var("NEXTEST_PROFILE").ok().filter(|p| !p.is_empty()))
        .unwrap_or_else(|| "default".to_owned());
    let config_path = option(command, &["--config-file"])
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".config").join("nextest.toml"));
    let config: toml::Table = match fs::read_to_string(&config_path) {
        Ok(text) => text
            .parse()
            .map_err(|e| format!("could not parse {}: {e}", config_path.display()))?,
        Err(_) => toml::Table::new(),
    };
    let profiles = config.get("profile").and_then(toml::Value::as_table);
    let mut name = profile.as_str();
    let mut path = None;
    for _ in 0..16 {
        let table = profiles
            .and_then(|p| p.get(name))
            .and_then(toml::Value::as_table);
        path = table
            .and_then(|t| t.get("junit"))
            .and_then(|j| j.get("path"))
            .and_then(toml::Value::as_str);
        if path.is_some() || name == "default" {
            break;
        }
        name = table
            .and_then(|t| t.get("inherits"))
            .and_then(toml::Value::as_str)
            .unwrap_or("default");
    }
    let path = path.ok_or_else(|| {
        format!(
            "JUnit output is not enabled for nextest profile `{profile}`: add [profile.{profile}.junit] \
             path = \"junit.xml\" to {}, or pass --junit <path>",
            config_path.display()
        )
    })?;
    let store = config
        .get("store")
        .and_then(|s| s.get("dir"))
        .and_then(toml::Value::as_str)
        .unwrap_or("target/nextest");
    Ok(root.join(store).join(&profile).join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_ids_name_their_source_files() {
        let metadata = serde_json::json!({"packages": [{"name": "my-app", "targets": [
            {"name": "my_app", "kind": ["lib"], "src_path": "/ws/my-app/src/lib.rs"},
            {"name": "my-app", "kind": ["bin"], "src_path": "/ws/my-app/src/main.rs"},
            {"name": "api", "kind": ["test"], "src_path": "/ws/my-app/tests/api.rs"},
            {"name": "build-script-build", "kind": ["custom-build"], "src_path": "/ws/my-app/build.rs"},
            {"name": "outside", "kind": ["test"], "src_path": "/elsewhere/outside.rs"}
        ]}]});
        let files = binary_files(&metadata, Path::new("/ws"));
        assert_eq!(
            files,
            HashMap::from([
                ("my-app".to_owned(), "my-app/src/lib.rs".to_owned()),
                (
                    "my-app::bin/my-app".to_owned(),
                    "my-app/src/main.rs".to_owned()
                ),
                ("my-app::api".to_owned(), "my-app/tests/api.rs".to_owned()),
            ])
        );
    }

    #[test]
    fn junit_path_follows_profile_inheritance() {
        let dir = std::env::temp_dir().join(format!("gf-nextest-{}", std::process::id()));
        fs::create_dir_all(dir.join(".config")).unwrap();
        fs::write(
            dir.join(".config/nextest.toml"),
            "[store]\ndir = \"out\"\n[profile.base.junit]\npath = \"base.xml\"\n\
             [profile.ci]\ninherits = \"base\"\n[profile.default.junit]\npath = \"default.xml\"\n\
             [profile.other]\n",
        )
        .unwrap();
        let path = |args: &[&str]| {
            let command: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            junit_path(&command, &dir).map(|p| p.strip_prefix(&dir).unwrap().to_path_buf())
        };
        assert_eq!(
            path(&["cargo", "nextest", "run", "--profile", "ci"]),
            Ok("out/ci/base.xml".into())
        );
        assert_eq!(
            path(&["cargo", "nextest", "run", "-P=other"]),
            Ok("out/other/default.xml".into())
        );
        // After `--`, arguments belong to the test binaries.
        assert_eq!(
            path(&["cargo", "nextest", "run", "--", "--profile", "ci"]),
            Ok("out/default/default.xml".into())
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
