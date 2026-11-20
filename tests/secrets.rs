//! Secrets resolved through a fake 1Password CLI (tests/fixtures/fake-op),
//! delivered to the jobs that list them and masked in everything goodflows
//! prints or stores.

mod common;

use common::{stderr, stdout, success, Project};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const VALUE: &str = "s3cr3t-database-url";
const DEV: &str = "op://dev/postgres/url";

/// `PATH` without any directory that holds an `op`, optionally with the fake first.
fn path(fake: bool) -> String {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&inherited).filter(|dir| !dir.join("op").exists());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-op");
    let dirs: Vec<PathBuf> = fake.then_some(fixture).into_iter().chain(dirs).collect();
    std::env::join_paths(dirs).unwrap().into_string().unwrap()
}

/// A project whose fake `op` knows `values` (`REFERENCE=VALUE` lines, `%b`
/// escapes allowed) and logs each call to `.op/log`.
struct Op {
    project: Project,
}

impl Op {
    fn new(config: &str, values: &[(&str, &str)]) -> Self {
        let project = Project::new(config);
        std::fs::create_dir_all(project.dir.join(".op")).unwrap();
        let lines: String = values
            .iter()
            .map(|(reference, value)| format!("{reference}={value}\n"))
            .collect();
        std::fs::write(project.dir.join(".op/values"), lines).unwrap();
        Self { project }
    }

    fn command(&self, fake: bool) -> Command {
        let mut command = self.project.command();
        command
            .env("PATH", path(fake))
            .env("FAKE_OP_LOG", self.project.dir.join(".op/log"))
            .env("FAKE_OP_VALUES", self.project.dir.join(".op/values"))
            .env_remove("DATABASE_URL");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(true).args(args).output().unwrap()
    }

    /// Every `op` call, one line each.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.project.dir.join(".op/log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn read(&self, path: &str) -> String {
        self.project.read(path)
    }

    /// The contents of every log file under `logs`.
    fn logs(&self) -> String {
        fn walk(dir: &Path, out: &mut String) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        let mut out = String::new();
        walk(&self.project.dir.join("logs"), &mut out);
        out
    }
}

fn events(output: &Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn error_event(output: &Output) -> Value {
    events(output)
        .into_iter()
        .find(|event| event["type"] == "error")
        .unwrap_or_else(|| panic!("no error event: {}", stdout(output)))
}

fn started(output: &Output) -> usize {
    events(output)
        .iter()
        .filter(|event| event["type"] == "job_started")
        .count()
}

fn shared_settings() -> String {
    format!("[settings.secrets]\nDATABASE_URL = '{DEV}'\n")
}

#[test]
fn only_the_listing_job_receives_the_secret_and_it_wins_over_the_caller() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\ncommand = 'printf %s \"${{DATABASE_URL:-unset}}\" > integration.txt'\nsecrets = ['DATABASE_URL']\n\
             [[jobs]]\nname = 'unit'\ncommand = 'printf %s \"${{DATABASE_URL:-unset}}\" > unit.txt'\n\
             [[jobs]]\nname = 'steps'\nsecrets = ['DATABASE_URL']\n\
             [[jobs.steps]]\nid = 'one'\ncommand = 'printf %s \"$DATABASE_URL\" > step.txt'\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    success(&p.run(&[]));
    assert_eq!(p.read("integration.txt"), VALUE);
    assert_eq!(p.read("unit.txt"), "unset");
    assert_eq!(p.read("step.txt"), VALUE);
    assert_eq!(p.calls(), [format!("read --no-newline {DEV}")]);

    // A caller variable of the same name reaches only the jobs that do not list it.
    let output = p
        .command(true)
        .env("DATABASE_URL", "from-caller")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("integration.txt"), VALUE);
    assert_eq!(p.read("unit.txt"), "from-caller");
}

#[test]
fn conditions_and_expressions_never_see_the_value() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\nsecrets = ['DATABASE_URL']\n\
             condition = {{ expression = '(env.DATABASE_URL ?? \"none\") == \"none\"' }}\n\
             command = 'touch ran'\n\
             [[jobs]]\nname = 'shell'\nsecrets = ['DATABASE_URL']\n\
             condition = {{ shell = 'test -z \"$DATABASE_URL\"' }}\n\
             command = 'touch shell-ran'\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    success(&p.run(&[]));
    assert!(p.project.dir.join("ran").exists());
    assert!(p.project.dir.join("shell-ran").exists());
}

#[test]
fn each_distinct_reference_is_read_once_before_the_first_job() {
    let other = "op://dev/npm/token";
    let p = Op::new(
        &format!(
            "[[workflows.ci.jobs]]\nname = 'first'\ncommand = 'echo job >> \"$FAKE_OP_LOG\"'\n\
             [[workflows.ci.jobs]]\nname = 'a'\nworkflow = 'one'\ndepends_on = ['first']\n\
             [[workflows.ci.jobs]]\nname = 'b'\nworkflow = 'two'\ndepends_on = ['first']\n\
             [workflows.one.settings.secrets]\nDB = '{DEV}'\n\
             [[workflows.one.jobs]]\nname = 'x'\ncommand = 'true'\nsecrets = ['DB']\n\
             [[workflows.one.jobs]]\nname = 'y'\ncommand = 'true'\nsecrets = ['DB']\n\
             [workflows.two.settings.secrets]\nDATABASE_URL = '{DEV}'\nNPM = '{other}'\n\
             [[workflows.two.jobs]]\nname = 'z'\ncommand = 'true'\nsecrets = ['DATABASE_URL', 'NPM']\n"
        ),
        &[(DEV, VALUE), (other, "npm-token-value")],
    );
    success(&p.run(&["run", "ci"]));
    let calls = p.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0], format!("read --no-newline {DEV}"));
    assert_eq!(calls[1], format!("read --no-newline {other}"));
    // The root's first job started only after every read.
    assert_eq!(calls[2], "job");
}

#[test]
fn a_run_that_selects_no_secret_never_looks_for_op() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'fmt'\ncommand = 'true'\n\
             [[jobs]]\nname = 'integration'\ncommand = 'true'\nsecrets = ['DATABASE_URL']\n",
            shared_settings()
        ),
        &[],
    );
    let output = p.command(false).args(["--jobs", "fmt"]).output().unwrap();
    success(&output);
    assert!(p.calls().is_empty());
}

#[test]
fn a_failed_read_starts_no_job() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'first'\ncommand = 'touch ran'\n\
             [[jobs]]\nname = 'integration'\ncommand = 'true'\nsecrets = ['DATABASE_URL']\n",
            shared_settings()
        ),
        &[],
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let error = error_event(&output);
    assert_eq!(error["code"], "secret_unavailable");
    assert_eq!(error["secret"], "DATABASE_URL");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains(&format!("secret DATABASE_URL ({DEV}) could not be read")));
    assert!(message.contains("isn't an item in the vault"), "{message}");
    assert!(!message.contains("second line"), "{message}");
    assert_eq!(started(&output), 0);
    assert!(!p.project.dir.join("ran").exists());

    // Text mode exits 1 too and names the secret.
    let output = p.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("secret DATABASE_URL"), "{text}");
}

#[test]
fn a_missing_op_starts_no_job() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\ncommand = 'touch ran'\nsecrets = ['DATABASE_URL']\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    let output = p.command(false).arg("--json").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = error_event(&output);
    assert_eq!(error["code"], "secret_unavailable");
    assert_eq!(error["secret"], "DATABASE_URL");
    assert!(error["message"].as_str().unwrap().contains("`op`"));
    assert_eq!(started(&output), 0);
    assert!(!p.project.dir.join("ran").exists());
}

#[test]
fn a_value_that_is_not_utf8_or_contains_nul_starts_no_job() {
    for value in ["bad\\0377value", "bad\\0000value"] {
        let p = Op::new(
            &format!(
                "{}[[jobs]]\nname = 'integration'\ncommand = 'touch ran'\nsecrets = ['DATABASE_URL']\n",
                shared_settings()
            ),
            &[(DEV, value)],
        );
        let output = p.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1));
        let error = error_event(&output);
        assert_eq!(error["code"], "secret_unavailable");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains("not valid UTF-8") || message.contains("contains NUL"),
            "{message}"
        );
        assert!(!message.contains("bad"), "{message}");
        assert_eq!(started(&output), 0);
    }
}

#[test]
fn a_profile_switches_the_reference() {
    let ci = "op://ci/postgres/url";
    let p = Op::new(
        &format!(
            "{}[environments.ci.settings.secrets]\nDATABASE_URL = '{ci}'\n\
             [[jobs]]\nname = 'integration'\ncommand = 'printf %s \"$DATABASE_URL\" > out.txt'\nsecrets = ['DATABASE_URL']\n",
            shared_settings()
        ),
        &[(DEV, VALUE), (ci, "ci-database-url")],
    );
    success(&p.run(&["--env", "ci"]));
    assert_eq!(p.calls(), [format!("read --no-newline {ci}")]);
    assert_eq!(p.read("out.txt"), "ci-database-url");
}

#[test]
fn validate_and_list_never_run_op() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\ncommand = 'true'\nsecrets = ['DATABASE_URL']\n\
             [[jobs]]\nname = 'unit'\ncommand = 'true'\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    success(&p.run(&["validate"]));
    let list = p.run(&["list"]);
    success(&list);
    assert!(
        stdout(&list).contains("    secrets: DATABASE_URL\n"),
        "{}",
        stdout(&list)
    );
    let list = p.run(&["list", "--json"]);
    success(&list);
    let jobs = &events(&list)[0]["jobs"];
    assert_eq!(jobs[0]["secrets"], serde_json::json!(["DATABASE_URL"]));
    assert!(jobs[1].get("secrets").is_none());
    assert!(p.calls().is_empty());

    // An unsupported reference fails validation without `op`.
    let p = Op::new(
        "[settings.secrets]\nDATABASE_URL = '0p://dev/postgres/url'\n[[jobs]]\nname = 'a'\ncommand = 'true'\n",
        &[],
    );
    let output = p.run(&["validate", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(error_event(&output)["code"], "config_invalid");
    assert!(p.calls().is_empty());
}

#[test]
fn the_value_is_masked_in_failure_output_logs_and_last() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\nsecrets = ['DATABASE_URL']\n\
             command = 'echo \"url=$DATABASE_URL\"; echo \"err $DATABASE_URL\" >&2; exit 1'\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    let output = p.run(&["--output", "failures", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let terminal = format!("{}{}", stdout(&output), stderr(&output));
    assert!(terminal.contains("url=[redacted]"), "{terminal}");
    assert!(terminal.contains("err [redacted]"), "{terminal}");
    assert!(!terminal.contains(VALUE), "{terminal}");
    let logs = p.logs();
    assert!(logs.contains("url=[redacted]\n"), "{logs}");
    assert!(logs.contains("err [redacted]\n"), "{logs}");
    assert!(!logs.contains(VALUE), "{logs}");
    let last = stdout(&p.run(&["last"]));
    assert!(last.contains("url=[redacted]"), "{last}");
    assert!(!last.contains(VALUE), "{last}");

    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let json = stdout(&output);
    assert!(!json.contains(VALUE), "{json}");
    let summary = events(&output)
        .into_iter()
        .find(|event| event["type"] == "summary")
        .unwrap();
    let result = &summary["results"][0];
    assert_eq!(result["stdout_tail"], "url=[redacted]\n");
    assert_eq!(result["stderr_tail"], "err [redacted]\n");
    let combined = format!(
        "{}{}",
        result["output_head"].as_str().unwrap(),
        result["output_tail"].as_str().unwrap()
    );
    assert!(combined.contains("url=[redacted]"), "{combined}");
    let last = stdout(&p.run(&["last", "--json"]));
    assert!(last.contains("url=[redacted]"), "{last}");
    assert!(!last.contains(VALUE), "{last}");
}

#[test]
fn a_value_split_across_chunks_is_masked() {
    let p = Op::new(
        &format!(
            "{}[[jobs]]\nname = 'integration'\nsecrets = ['DATABASE_URL']\n\
             command = 'printf \"a s3cr3t-da\"; sleep 0.5; printf \"tabase-url b\"'\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    let output = p.run(&["--json", "--log-dir", "logs"]);
    success(&output);
    let data: String = events(&output)
        .iter()
        .filter(|event| event["type"] == "job_output")
        .map(|event| event["data"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(data, "a [redacted] b");
    let logs = p.logs();
    assert_eq!(logs, "a [redacted] b");
}

#[test]
fn another_job_and_published_outputs_are_masked() {
    let p = Op::new(
        &format!(
            "{}{}",
            shared_settings(),
            r#"[[jobs]]
name = 'writer'
secrets = ['DATABASE_URL']
command = '''printf %s "$DATABASE_URL" > value.txt; printf '{"url": "%s"}' "$DATABASE_URL" > "$GOODFLOWS_OUTPUT"'''
[[jobs]]
name = 'reader'
depends_on = ['writer']
command = 'cat value.txt'
condition = { expression = 'needs.writer.outputs.url == "[redacted]"' }
"#
        ),
        &[(DEV, VALUE)],
    );
    let output = p.run(&["--json"]);
    success(&output);
    assert_eq!(p.read("value.txt"), VALUE);
    let json = stdout(&output);
    assert!(!json.contains(VALUE), "{json}");
    let reader: Vec<_> = events(&output)
        .into_iter()
        .filter(|event| event["type"] == "job_output" && event["name"] == "reader")
        .collect();
    assert_eq!(reader.len(), 1, "{json}");
    assert_eq!(reader[0]["data"], "[redacted]");
    let last = stdout(&p.run(&["last", "--json"]));
    assert!(last.contains("[redacted]"), "{last}");
    assert!(!last.contains(VALUE), "{last}");
}

#[test]
fn a_short_value_is_delivered_unmasked_with_one_warning_per_name() {
    let p = Op::new(
        "[settings.secrets]\nPIN = 'op://dev/pin/code'\nALSO_PIN = 'op://dev/pin/code'\n\
         [[jobs]]\nname = 'integration'\ncommand = 'printf %s \"$PIN $ALSO_PIN\" > out.txt; printf %s \"$PIN\"'\nsecrets = ['PIN', 'ALSO_PIN']\n",
        &[("op://dev/pin/code", "x7q")],
    );
    let output = p.run(&["--log-dir", "logs"]);
    success(&output);
    assert_eq!(p.calls().len(), 1);
    assert_eq!(p.read("out.txt"), "x7q x7q");
    // Not masked in the job log either.
    assert!(p.logs().contains("x7q"), "{}", p.logs());
    let stderr = stderr(&output);
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("shorter than 4 bytes"))
        .collect();
    assert_eq!(warnings.len(), 2, "{stderr}");
    assert!(warnings[0].contains("secret PIN "), "{stderr}");
    assert!(warnings[1].contains("secret ALSO_PIN "), "{stderr}");
    assert!(warnings.iter().all(|w| !w.contains("x7q")), "{stderr}");
}

#[test]
fn a_read_that_times_out_starts_no_job() {
    let other = "op://dev/redis/url";
    // The first read, then a later one.
    for (variable, slow, values) in [
        (
            "GOODFLOWS_TEST_SECRET_FIRST_READ_TIMEOUT_SECS",
            DEV,
            vec![(other, "redis-value")],
        ),
        (
            "GOODFLOWS_TEST_SECRET_READ_TIMEOUT_SECS",
            other,
            vec![(DEV, VALUE)],
        ),
    ] {
        let p = Op::new(
            &format!(
                "{}REDIS_URL = '{other}'\n\
                 [[jobs]]\nname = 'integration'\ncommand = 'touch ran'\nsecrets = ['DATABASE_URL', 'REDIS_URL']\n",
                shared_settings()
            ),
            &values,
        );
        let output = p
            .command(true)
            .env(variable, "1")
            .env("FAKE_OP_SLOW", slow)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
        let error = error_event(&output);
        assert_eq!(error["code"], "secret_unavailable");
        let name = if slow == DEV {
            "DATABASE_URL"
        } else {
            "REDIS_URL"
        };
        assert_eq!(error["secret"], name);
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("secret {name} ({slow})")),
            "{message}"
        );
        assert!(message.contains("op timed out after 1s"), "{message}");
        assert!(!message.contains(VALUE) && !message.contains("redis-value"));
        assert_eq!(started(&output), 0);
        assert!(!p.project.dir.join("ran").exists());
    }
}

#[test]
fn a_numeric_output_and_an_output_key_are_masked() {
    let p = Op::new(
        r#"[settings.secrets]
PIN = 'op://dev/pin/code'
[[jobs]]
name = 'writer'
secrets = ['PIN']
command = '''printf '{"pin": %s, "%s": true}' "$PIN" "$PIN" > "$GOODFLOWS_OUTPUT"'''
"#,
        &[("op://dev/pin/code", "12345678")],
    );
    let output = p.run(&["--json"]);
    success(&output);
    let json = stdout(&output);
    assert!(!json.contains("12345678"), "{json}");
    let finished = events(&output)
        .into_iter()
        .find(|event| event["type"] == "job_finished")
        .unwrap();
    let masked = serde_json::json!({"pin": "[redacted]", "[redacted]": true});
    assert_eq!(finished["result"]["outputs"], masked, "{json}");
    let summary = events(&output)
        .into_iter()
        .find(|event| event["type"] == "summary")
        .unwrap();
    assert_eq!(summary["results"][0]["outputs"], masked, "{json}");
    fn walk(dir: &Path, out: &mut String) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut stored = String::new();
    walk(&p.project.dir.join(".goodflows/last"), &mut stored);
    assert!(stored.contains("[redacted]"), "{stored}");
    assert!(!stored.contains("12345678"), "{stored}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_interrupt_while_reading_kills_op_and_exits_130() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let other = "op://dev/redis/url";
    let p = Op::new(
        &format!(
            "{}REDIS_URL = '{other}'\n\
             [[jobs]]\nname = 'integration'\ncommand = 'touch ran'\nsecrets = ['DATABASE_URL', 'REDIS_URL']\n",
            shared_settings()
        ),
        &[(DEV, VALUE)],
    );
    let log = p.project.dir.join(".op/log");
    // A process the fake `op` left behind: it still carries this test's log path.
    let leftover = || {
        let needle = format!("FAKE_OP_LOG={}\0", log.display());
        std::fs::read_dir("/proc").unwrap().flatten().any(|entry| {
            std::fs::read(entry.path().join("environ"))
                .map(|environ| String::from_utf8_lossy(&environ).contains(&needle))
                .unwrap_or(false)
        })
    };
    let child = p
        .command(true)
        .env("FAKE_OP_SLOW", other)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !p.calls().iter().any(|call| call.ends_with(other)) {
        assert!(start.elapsed() < Duration::from_secs(10), "op was not run");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(leftover(), "the slow read is not running");
    // SAFETY: the child PID identifies the CLI spawned above.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130), "{}", stdout(&output));
    let error = error_event(&output);
    assert_eq!(error["code"], "runtime");
    assert_eq!(
        error["message"],
        "interrupted by signal 2 while reading secrets"
    );
    assert_eq!(started(&output), 0);
    assert!(!p.project.dir.join("ran").exists());
    let start = Instant::now();
    while leftover() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the slow op read is still running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_job_cannot_inspect_a_run_that_holds_secrets() {
    // Root reads any process's /proc entries regardless.
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let probe = "if cat /proc/$PPID/environ > /dev/null 2>&1; then echo readable; else echo protected; fi > probe.txt";
    for (secrets, expected) in [("['DATABASE_URL']", "protected"), ("[]", "readable")] {
        let p = Op::new(
            &format!(
                "{}[[jobs]]\nname = 'holder'\nsecrets = {secrets}\ncommand = 'true'\n\
                 [[jobs]]\nname = 'probe'\ndepends_on = ['holder']\ncommand = '{probe}'\n",
                shared_settings()
            ),
            &[(DEV, VALUE)],
        );
        success(&p.run(&[]));
        assert_eq!(p.read("probe.txt").trim(), expected, "secrets = {secrets}");
    }
}

fn finished(output: &Output) -> Value {
    events(output)
        .into_iter()
        .find(|event| event["type"] == "job_finished")
        .unwrap_or_else(|| panic!("no job_finished: {}", stdout(output)))
}

#[test]
fn an_output_error_never_quotes_the_value() {
    // The whole file is the value: a string, then numbers serde would
    // render differently from what the job wrote.
    for (value, format, column) in [
        (VALUE, r#"'"%s"'"#, 21),
        ("1234.50", "%s", 7),
        ("12345678901234567890123", "%s", 23),
        ("5e10", "%s", 4),
    ] {
        let p = Op::new(
            &format!(
                "{}[[jobs]]\nname = 'writer'\nsecrets = ['DATABASE_URL']\n\
                 command = '''printf {format} \"$DATABASE_URL\" > \"$GOODFLOWS_OUTPUT\"'''\n",
                shared_settings()
            ),
            &[(DEV, value)],
        );
        let output = p.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
        assert_eq!(
            finished(&output)["result"]["error"],
            format!("invalid job outputs: expected a JSON object at line 1 column {column}"),
            "{value}"
        );
        let last = stdout(&p.run(&["last", "--json"]));
        assert!(!last.contains(value), "{last}");
    }
}

#[test]
fn an_out_of_range_number_output_names_its_key() {
    let p = Op::new(
        r#"[[jobs]]
name = 'writer'
command = '''printf '{"n": 1e400}' > "$GOODFLOWS_OUTPUT"'''
"#,
        &[],
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let result = &finished(&output)["result"];
    let error = result["error"].as_str().unwrap();
    assert!(
        error.starts_with("invalid job outputs: output 'n': number out of range"),
        "{error}"
    );
}

#[test]
fn output_keys_that_mask_to_the_same_text_fail_the_job() {
    let p = Op::new(
        r#"[settings.secrets]
A = 'op://dev/a/value'
B = 'op://dev/b/value'
[[jobs]]
name = 'writer'
secrets = ['A', 'B']
command = '''printf '{"%s": 1, "%s": 2}' "$A" "$B" > "$GOODFLOWS_OUTPUT"'''
"#,
        &[
            ("op://dev/a/value", "first-value"),
            ("op://dev/b/value", "second-value"),
        ],
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let json = stdout(&output);
    assert!(
        !json.contains("first-value") && !json.contains("second-value"),
        "{json}"
    );
    let result = &finished(&output)["result"];
    assert_eq!(result["status"], "failed");
    assert_eq!(
        result["error"],
        "invalid job outputs: output '[redacted]' is duplicated after masking"
    );
}

#[test]
fn a_number_is_masked_as_written_not_as_parsed() {
    // Too long for an integer, it parses as a float that renders as
    // 1.2345678901234568e22; the parsed 1234.50 drops its trailing zero.
    for value in ["12345678901234567890123", "1234.50"] {
        let p = Op::new(
            r#"[settings.secrets]
NUM = 'op://dev/num/value'
[[jobs]]
name = 'writer'
secrets = ['NUM']
command = '''printf '{"n": %s}' "$NUM" > "$GOODFLOWS_OUTPUT"'''
"#,
            &[("op://dev/num/value", value)],
        );
        let output = p.run(&["--json"]);
        success(&output);
        assert_eq!(
            finished(&output)["result"]["outputs"],
            serde_json::json!({"n": "[redacted]"}),
            "{value}: {}",
            stdout(&output)
        );
        assert!(!stdout(&output).contains(value), "{value}");
    }
}
