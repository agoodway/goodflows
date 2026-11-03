#![cfg(unix)]
mod common;
use common::mock::{ack, Mock, Reply};
use common::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const SECRET: &str = "synthetic-secret-7f3a9c";
const REPORTING: &str = "[reporting]\nproject = \"goodcases\"\n";

fn project(jobs: &str) -> Project {
    project_with(REPORTING, jobs)
}

fn project_with(reporting: &str, jobs: &str) -> Project {
    let p = Project::new(&format!("{reporting}\n{jobs}"));
    std::fs::create_dir_all(p.dir.join("fixtures")).unwrap();
    for (id, partition) in [("r1", "1"), ("r2", "2"), ("r3", "3")] {
        std::fs::write(
            p.dir.join(format!("fixtures/{id}.jsonl")),
            timing_report(id, Some(partition)),
        )
        .unwrap();
    }
    p
}

fn copy(ids: &[&str]) -> String {
    ids.iter()
        .map(|id| format!("cp fixtures/{id}.jsonl \"$GOODFLOWS_REPORT_DIR/{id}.jsonl\""))
        .collect::<Vec<_>>()
        .join(" && ")
}

fn by_type(events: &[Value], kind: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|e| e["type"] == kind)
        .cloned()
        .collect()
}

fn run_dirs(p: &Project) -> Vec<PathBuf> {
    run_dirs_in(&p.dir.join(".goodflows/reports"))
}

fn run_dirs_in(spool: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(spool)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("manifest.json").exists())
        .collect();
    dirs.sort();
    dirs
}

fn dir_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap()
}

fn run_with(p: &Project, args: &[&str], url: Option<&str>) -> std::process::Output {
    let mut command = p.command();
    command.args(args);
    if let Some(url) = url {
        command
            .env("GOODFLOWS_URL", url)
            .env("GOODFLOWS_API_KEY", SECRET);
    }
    command.output().unwrap()
}

fn upload(cwd: &Path, run_dir: &Path, url: Option<&str>) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_goodflows"));
    command
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env_remove("GOODFLOWS_URL")
        .env_remove("GOODFLOWS_API_KEY")
        .args(["reports", "upload", "--json"])
        .arg(run_dir);
    if let Some(url) = url {
        command
            .env("GOODFLOWS_URL", url)
            .env("GOODFLOWS_API_KEY", SECRET);
    }
    command.output().unwrap()
}

fn assert_no_secret(p: &Project, output: &std::process::Output) {
    assert!(!stdout(output).contains(SECRET), "{}", stdout(output));
    assert!(!stderr(output).contains(SECRET));
    for file in files_under(&p.dir) {
        let content = std::fs::read(&file).unwrap();
        assert!(
            !String::from_utf8_lossy(&content).contains(SECRET),
            "secret in {}",
            file.display()
        );
    }
}

// ---- 1.x configuration ----

#[test]
fn invalid_reporting_config_fails_before_any_job_runs() {
    let job =
        |extra: &str| format!("[[jobs]]\nname = \"test\"\ncommand = \"touch ran\"\n{extra}\n");
    let cases = [
        (REPORTING.to_owned(), job("reports = [\"junit\"]"), "unsupported format"),
        (
            REPORTING.to_owned(),
            job("reports = [\"exunit-timings\", \"exunit-timings\"]"),
            "duplicate format",
        ),
        (String::new(), job("reports = [\"exunit-timings\"]"), "requires a top-level [reporting]"),
        (
            format!("{REPORTING}token = \"gfk_live_123\"\n"),
            job(""),
            "must not be written inline",
        ),
        (
            format!("{REPORTING}api_key = \"gfk_live_123\"\n"),
            job(""),
            "must not be written inline",
        ),
        ("[reporting]\nproject = \"Good Cases\"\n".into(), job(""), "reporting project"),
        ("[reporting]\nproject = \"\"\n".into(), job(""), "reporting project"),
        (format!("[reporting]\nproject = \"{}\"\n", "a".repeat(65)), job(""), "reporting project"),
        (
            format!("{REPORTING}url = \"https://flows.example.com\"\n"),
            job(""),
            "must not be written inline",
        ),
        ("[reporting]\nproject = \"-goodcases\"\n".into(), job(""), "reporting project"),
        ("[reporting]\nproject = \"_goodcases\"\n".into(), job(""), "reporting project"),
        (
            format!("{REPORTING}url_env = \"SAME\"\ntoken_env = \"SAME\"\n"),
            job(""),
            "must name different variables",
        ),
        (
            format!("{REPORTING}url_env = \"GOODFLOWS_RUN_ID\"\n"),
            job(""),
            "url_env must not name a reserved goodflows variable",
        ),
        (
            format!("{REPORTING}token_env = \"GOODFLOWS_REPORT_DIR\"\n"),
            job(""),
            "token_env must not name a reserved goodflows variable",
        ),
        (
            format!("{REPORTING}token_env = \"GOODFLOWS_OUTPUT\"\n"),
            job(""),
            "token_env must not name a reserved goodflows variable",
        ),
        (format!("{REPORTING}token_env = \"gfk live\"\n"), job(""), "token_env"),
        (format!("{REPORTING}url_env = \"1URL\"\n"), job(""), "url_env"),
        (format!("{REPORTING}unknown = 1\n"), job(""), "unknown field"),
        (format!("{REPORTING}spool_dir = \"\"\n"), job(""), "spool_dir must not be empty"),
        (
            REPORTING.to_owned(),
            format!(
                "[[jobs]]\nname = \"{}\"\ncommand = \"touch ran\"\nreports = [\"exunit-timings\"]\n",
                "j".repeat(65)
            ),
            "at most 64 UTF-8 bytes",
        ),
        (
            REPORTING.to_owned(),
            "[[jobs]]\nname = \"te\\tst\"\ncommand = \"touch ran\"\nreports = [\"exunit-timings\"]\n".into(),
            "control characters",
        ),
        (
            String::new(),
            job("env = { GOODFLOWS_RUN_ID = \"x\" }"),
            "reserved variable GOODFLOWS_RUN_ID",
        ),
        (
            String::new(),
            "[[jobs]]\nname = \"test\"\n[[jobs.steps]]\nid = \"s\"\ncommand = \"touch ran\"\nenv = { GOODFLOWS_REPORT_DIR = \"x\" }\n".into(),
            "reserved variable GOODFLOWS_REPORT_DIR",
        ),
    ];
    for (reporting, jobs, message) in cases {
        let p = Project::new(&format!("{reporting}\n{jobs}"));
        for args in [vec!["validate", "--json"], vec!["--json"]] {
            let output = p.run(&args);
            assert_eq!(output.status.code(), Some(1), "{reporting}{jobs}");
            let event: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(event["type"], "error");
            assert!(
                event["message"].as_str().unwrap().contains(message),
                "{message}: {}",
                event["message"]
            );
            assert!(!p.dir.join("ran").exists());
            assert!(!p.dir.join(".goodflows").exists());
        }
    }
}

#[test]
fn job_level_reporting_errors_name_the_job() {
    let cases = [
        (
            REPORTING.to_owned(),
            format!("{}a", "é".repeat(32)),
            "reports = [\"exunit-timings\"]\ncommand = \"touch ran\"".to_owned(),
            "at most 64 UTF-8 bytes",
        ),
        (
            REPORTING.to_owned(),
            "probe".into(),
            "command = \"touch ran\"\nenv = { GOODFLOWS_API_KEY = \"override\" }".into(),
            "env must not set GOODFLOWS_API_KEY: it is the reporting token_env",
        ),
        (
            format!("{REPORTING}token_env = \"CUSTOM_KEY\"\n"),
            "stepped".into(),
            "[[jobs.steps]]\nid = \"s\"\ncommand = \"touch ran\"\nenv = { CUSTOM_KEY = \"override\" }".into(),
            "env must not set CUSTOM_KEY: it is the reporting token_env",
        ),
    ];
    for (reporting, name, body, message) in cases {
        let p = Project::new(&format!(
            "{reporting}\n[[jobs]]\nname = \"{name}\"\n{body}\n"
        ));
        let output = p.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1), "{name}");
        let event: Value = serde_json::from_slice(&output.stdout).unwrap();
        let text = event["message"].as_str().unwrap();
        assert!(text.contains(message), "{message}: {text}");
        assert!(text.contains(&format!("job '{name}'")), "{text}");
        assert!(!p.dir.join("ran").exists());
        assert!(!p.dir.join(".goodflows").exists());
    }
}

#[test]
fn reporting_config_boundaries_are_accepted() {
    let slug = format!("0{}", "a-_".repeat(21));
    assert_eq!(slug.len(), 64);
    let p = Project::new(&format!(
        r#"
[reporting]
project = "{slug}"
url_env = "MY_URL"
token_env = "MY_KEY"

[[jobs]]
name = "{ascii}"
command = "true"
reports = ["exunit-timings"]

[[jobs]]
name = "{multibyte}"
command = "true"
reports = ["exunit-timings"]

[[jobs]]
name = "{long}"
command = "true"
env = {{ GOODFLOWS_API_KEY = "not the token here" }}
"#,
        ascii = "j".repeat(64),
        multibyte = "é".repeat(32),
        long = "n".repeat(100),
    ));
    let output = p.run(&["validate", "--json"]);
    success(&output);
    assert!(!p.dir.join(".goodflows").exists());
}

#[test]
fn inspection_never_creates_spools_or_contacts_the_endpoint() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    for args in [
        vec!["validate"],
        vec!["validate", "--json"],
        vec!["list"],
        vec!["list", "--json"],
        vec!["list", "--workflows"],
    ] {
        let output = run_with(&p, &args, Some(&mock.url));
        success(&output);
        assert!(!stdout(&output).contains(SECRET));
        if args == ["list"] {
            let text = stdout(&output);
            assert!(text.contains("Reporting: project=goodcases url_env=GOODFLOWS_URL token_env=GOODFLOWS_API_KEY"), "{text}");
            assert!(text.contains("reports: exunit-timings"));
        }
        if args == ["list", "--json"] {
            let event: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(event["jobs"][0]["reports"], json!(["exunit-timings"]));
            assert_eq!(
                event["reporting"],
                json!({"project":"goodcases", "url_env":"GOODFLOWS_URL", "token_env":"GOODFLOWS_API_KEY",
                    "spool_dir": p.dir.canonicalize().unwrap().join(".goodflows/reports")})
            );
        }
    }
    assert!(!p.dir.join(".goodflows").exists());
    assert_eq!(mock.connections(), 0);
}

#[test]
fn workflows_environments_selection_and_relative_spool_dirs_keep_reporting() {
    let p = Project::new("");
    std::fs::create_dir_all(p.dir.join("conf")).unwrap();
    std::fs::create_dir_all(p.dir.join("app")).unwrap();
    std::fs::write(p.dir.join("app/r1.jsonl"), timing_report("r1", None)).unwrap();
    std::fs::write(
        p.dir.join("conf/goodflows.toml"),
        r#"
[reporting]
project = "goodcases"
spool_dir = "spool"

[environments.local.settings]
parallelism = 1

[workflows.ci.settings]
workdir = "../app"

[[workflows.ci.jobs]]
name = "setup"
command = "true"

[[workflows.ci.jobs]]
name = "test"
command = 'cp r1.jsonl "$GOODFLOWS_REPORT_DIR/"'
depends_on = ["setup"]
reports = ["exunit-timings"]

[[workflows.ci.jobs]]
name = "unselected"
command = "touch unselected"
reports = ["exunit-timings"]
"#,
    )
    .unwrap();
    let output = p.run(&[
        "run",
        "ci",
        "--config",
        "conf/goodflows.toml",
        "--env",
        "local",
        "--jobs",
        "test",
        "--json",
    ]);
    success(&output);
    assert!(!p.dir.join("app/unselected").exists());
    // Relative to the config file, not the job workdir or the CLI cwd.
    let runs = run_dirs_in(&p.dir.join("conf/spool"));
    assert_eq!(runs.len(), 1);
    assert!(!p.dir.join("app/spool").exists());
    let manifest = manifest(&runs[0]);
    assert_eq!(manifest["workflow"], "ci");
    assert_eq!(manifest["environment"], "local");
    let jobs: Vec<_> = manifest["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["name"].clone())
        .collect();
    assert_eq!(jobs, [json!("test")]);
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(runs[0].join("test/pending/000.json")).unwrap())
            .unwrap();
    assert_eq!(snapshot["workflow"], "ci");
    assert_eq!(snapshot["environment"], "local");

    // A reporting block with no selected reporting jobs does no reporting work.
    let output = p.run(&[
        "run",
        "ci",
        "--config",
        "conf/goodflows.toml",
        "--jobs",
        "setup",
        "--json",
    ]);
    success(&output);
    assert_eq!(run_dirs_in(&p.dir.join("conf/spool")).len(), 1);
    assert!(by_type(&events(&output), "reporting_skipped").is_empty());
}

#[test]
fn unconfigured_workflows_keep_their_event_stream() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='true'");
    let output = p.run(&["--json"]);
    success(&output);
    let kinds: Vec<_> = events(&output)
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        ["run_started", "job_started", "job_finished", "summary"]
    );
    assert!(!p.dir.join(".goodflows").exists());
}

// ---- 2.x capture and collection ----

#[test]
fn local_only_collection_without_credentials() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1", "r2", "r3"])
    ));
    let output = p.run(&["--json"]);
    success(&output);
    let events = events(&output);
    assert_eq!(events.last().unwrap()["type"], "summary");
    assert_eq!(events.last().unwrap()["success"], true);
    let collected = by_type(&events, "reports_collected");
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0]["reports"], 3);
    assert_eq!(collected[0]["outcome"], "collected");
    assert_eq!(collected[0]["report_ids"], json!(["r1", "r2", "r3"]));
    let skipped = by_type(&events, "reporting_skipped");
    assert_eq!(skipped[0]["reason"], "credentials_unavailable");
    assert_eq!(skipped[0]["pending"], 3);
    let runs = run_dirs(&p);
    assert_eq!(runs.len(), 1);
    let manifest = manifest(&runs[0]);
    assert_eq!(manifest["state"], "finalized");
    assert_eq!(manifest["jobs"][0]["status"], "passed");
    // Raw producer files remain alongside frozen snapshots.
    for id in ["r1", "r2", "r3"] {
        assert!(runs[0].join(format!("test/reports/{id}.jsonl")).exists());
    }
    for i in 0..3 {
        assert!(runs[0].join(format!("test/pending/{i:03}.json")).exists());
    }
    let text = p.run(&[]);
    success(&text);
    let text = stdout(&text);
    assert!(
        text.contains("reports test: 3 collected (collected)"),
        "{text}"
    );
    assert!(text.contains("reporting skipped: credentials unavailable (3 reports kept locally)"));
    assert!(text.trim_end().ends_with("All jobs passed"));
}

#[test]
fn manifest_holds_only_structural_metadata_and_env_names() {
    let p = project_with(
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"MY_FLOWS_URL\"\ntoken_env = \"MY_FLOWS_KEY\"\n",
        &format!(
            "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\nenv = {{ UNRELATED = \"leak-me\" }}\n",
            copy(&["r1"])
        ),
    );
    let output = p
        .command()
        .args(["--json"])
        .env("MY_FLOWS_URL", "http://127.0.0.1:9/")
        .env("MY_FLOWS_KEY", SECRET)
        .env("OTHER_VALUE", "other-env-value")
        .output()
        .unwrap();
    success(&output);
    let dir = &run_dirs(&p)[0];
    let manifest = manifest(dir);
    let keys: Vec<_> = manifest.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "environment",
            "jobs",
            "project",
            "source",
            "state",
            "token_env",
            "url_env",
            "version",
            "workflow",
            "workflow_run_id"
        ]
    );
    assert_eq!(manifest["url_env"], "MY_FLOWS_URL");
    assert_eq!(manifest["token_env"], "MY_FLOWS_KEY");
    let text = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
    for leaked in [SECRET, "127.0.0.1:9", "leak-me", "other-env-value"] {
        assert!(!text.contains(leaked), "{leaked}");
    }
    let id = manifest["workflow_run_id"].as_str().unwrap();
    assert_eq!(dir.file_name().unwrap(), id);
    assert_eq!(id, id.to_lowercase());
    assert!(uuid_like(id));
}

fn uuid_like(id: &str) -> bool {
    let parts: Vec<_> = id.split('-').collect();
    parts.iter().map(|p| p.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        && parts[2].starts_with('4')
}

#[test]
fn concurrent_invocations_and_stale_files_stay_isolated() {
    let p = project(
        // Each job announces itself, then waits (bounded) for the other one, so
        // both invocations are provably inside their jobs at the same time.
        "[[jobs]]\nname = \"test\"\ncommand = 'cp \"fixtures/$WHICH.jsonl\" \"$GOODFLOWS_REPORT_DIR/\"; touch \"started-$WHICH\"; i=0; while [ ! -e \"started-$OTHER\" ] && [ $i -lt 500 ]; do sleep 0.02; i=$((i+1)); done; [ -e \"started-$OTHER\" ] && touch \"overlapped-$WHICH\"'\nreports = [\"exunit-timings\"]\n",
    );
    // Historical files in the default formatter location and an older run are never read.
    std::fs::create_dir_all(p.dir.join(".goodflows/reports/old/test/reports")).unwrap();
    std::fs::write(
        p.dir.join(".goodflows/reports/old/test/reports/r3.jsonl"),
        timing_report("r3", None),
    )
    .unwrap();
    std::fs::create_dir_all(p.dir.join("_build/test/timings")).unwrap();
    std::fs::write(
        p.dir.join("_build/test/timings/r3.jsonl"),
        timing_report("r3", None),
    )
    .unwrap();
    let spawn = |which: &str, other: &str| {
        p.command()
            .args(["--json"])
            .env("WHICH", which)
            .env("OTHER", other)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let (a, b) = (spawn("r1", "r2"), spawn("r2", "r1"));
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    success(&a);
    success(&b);
    assert!(p.dir.join("overlapped-r1").exists() && p.dir.join("overlapped-r2").exists());
    let ids = |output: &std::process::Output| {
        by_type(&events(output), "reports_collected")[0]["report_ids"].clone()
    };
    assert_eq!(ids(&a), json!(["r1"]));
    assert_eq!(ids(&b), json!(["r2"]));
    let runs = run_dirs(&p);
    assert_eq!(runs.len(), 2);
    assert_ne!(
        manifest(&runs[0])["workflow_run_id"],
        manifest(&runs[1])["workflow_run_id"]
    );
}

#[test]
fn distinct_job_names_get_distinct_reversible_directories() {
    let p = project(&format!(
        "[[jobs]]\nname = \"Test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n[[jobs]]\nname = \"a/b c\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"]),
        copy(&["r2"]),
        copy(&["r3"])
    ));
    let output = p.run(&["--json"]);
    success(&output);
    let dir = &run_dirs(&p)[0];
    let manifest = manifest(dir);
    let dirs: Vec<_> = manifest["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| (j["name"].clone(), j["dir"].clone()))
        .collect();
    assert_eq!(
        dirs,
        [
            (json!("Test"), json!("%54est")),
            (json!("test"), json!("test")),
            (json!("a/b c"), json!("a%2Fb%20c")),
        ]
    );
    assert!(dir.join("%54est/reports/r1.jsonl").exists());
    assert!(dir.join("test/reports/r2.jsonl").exists());
    assert!(dir.join("a%2Fb%20c/reports/r3.jsonl").exists());
}

#[test]
fn source_context_is_null_outside_git_and_captured_inside() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    success(&p.run(&["--json"]));
    let snapshot = |p: &Project| -> Value {
        let dir = run_dirs(p).pop().unwrap();
        serde_json::from_slice(&std::fs::read(dir.join("test/pending/000.json")).unwrap()).unwrap()
    };
    assert_eq!(
        snapshot(&p)["source"],
        json!({"commit":null,"branch":null,"dirty":null})
    );

    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&p.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q", "-b", "feature/x"]);
    git(&["add", "goodflows.toml"]);
    git(&["commit", "-q", "-m", "init"]);
    std::fs::remove_dir_all(p.dir.join(".goodflows")).unwrap();
    success(&p.run(&["--json"]));
    let source = &snapshot(&p)["source"];
    assert_eq!(source["commit"].as_str().unwrap().len(), 40);
    assert_eq!(source["branch"], "feature/x");
    assert_eq!(source["dirty"], false);
}

#[test]
fn unwritable_spool_runs_checks_without_reporting_env() {
    for (command, code) in [("true", 0), ("exit 3", 1)] {
        // `plain` must run even after `test` fails, whatever the parallelism.
        let p = project_with(
            "[reporting]\nproject = \"goodcases\"\nspool_dir = \"blocker/spool\"\n",
            &format!(
                "[settings]\nfail_fast = false\n[[jobs]]\nname = \"test\"\ncommand = 'printf %s \"${{GOODFLOWS_REPORT_DIR-unset}}:${{GOODFLOWS_RUN_ID-unset}}\" > seen; {command}'\nreports = [\"exunit-timings\"]\n[[jobs]]\nname = \"plain\"\ncommand = 'printf %s \"${{GOODFLOWS_REPORT_DIR-unset}}:${{GOODFLOWS_RUN_ID-unset}}\" > plain-seen'\n"
            ),
        );
        std::fs::write(p.dir.join("blocker"), "file, not a directory").unwrap();
        // Values inherited from an enclosing invocation must not reach any job either.
        let output = p
            .command()
            .arg("--json")
            .env("GOODFLOWS_REPORT_DIR", p.dir.join("inherited"))
            .env("GOODFLOWS_RUN_ID", "inherited-run-id")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        let events = events(&output);
        assert_eq!(
            by_type(&events, "reporting_skipped")[0]["reason"],
            "spool_unavailable"
        );
        assert!(by_type(&events, "reports_collected").is_empty());
        assert_eq!(events.last().unwrap()["type"], "summary");
        assert_eq!(p.read("seen"), "unset:unset");
        assert_eq!(p.read("plain-seen"), "unset:unset");
    }
    // A reporting block whose selected jobs produce no reports still scrubs inherited values.
    let p = project(
        "[[jobs]]\nname = \"plain\"\ncommand = 'printf %s \"${GOODFLOWS_REPORT_DIR-unset}:${GOODFLOWS_RUN_ID-unset}\" > plain-seen'\n",
    );
    let output = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_REPORT_DIR", p.dir.join("inherited"))
        .env("GOODFLOWS_RUN_ID", "inherited-run-id")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("plain-seen"), "unset:unset");
    assert!(!p.dir.join(".goodflows").exists());
}

#[test]
fn token_never_reaches_children_expressions_outputs_or_state() {
    // The default name and a custom token_env are scrubbed the same way.
    for (reporting, key) in [
        (REPORTING.to_owned(), "GOODFLOWS_API_KEY"),
        (
            format!("{REPORTING}token_env = \"CUSTOM_KEY\"\n"),
            "CUSTOM_KEY",
        ),
    ] {
        token_is_scrubbed(&reporting, key);
    }
}

fn token_is_scrubbed(reporting: &str, key: &str) {
    let p = project_with(
        reporting,
        &format!(
            r#"
[[jobs]]
name = "shell"
condition = "env > condition-env.txt"
command = 'env > shell-env.txt; {copy}'
reports = ["exunit-timings"]

[[jobs]]
name = "exec"
shell = false
command = "sh -c 'env > exec-env.txt'"

[[jobs]]
name = "stepped"
condition = {{ expression = "env.{key} == null" }}
outputs = {{ token = "env.{key} ?? steps.probe.outputs.seen" }}
[[jobs.steps]]
id = "probe"
condition = {{ shell = "env > step-condition-env.txt" }}
command = 'env > step-env.txt; printf "{{\"seen\":\"%s\"}}" "${{{key}-absent}}" > "$GOODFLOWS_OUTPUT"'
[[jobs.steps]]
id = "expr"
condition = {{ expression = "env.{key} == null" }}
command = "touch expression-was-null"
"#,
            copy = copy(&["r1"])
        ),
    );
    // An invalid origin keeps the run network-free while the key is exported,
    // and values inherited from an enclosing invocation must not leak through.
    let output = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_URL", "http://flows.example.com")
        .env(key, SECRET)
        .env("GOODFLOWS_RUN_ID", "inherited-run-id")
        .env("GOODFLOWS_REPORT_DIR", "/inherited/report/dir")
        .output()
        .unwrap();
    success(&output);
    for file in [
        "condition-env.txt",
        "shell-env.txt",
        "exec-env.txt",
        "step-condition-env.txt",
        "step-env.txt",
    ] {
        let env = p.read(file);
        assert!(!env.contains(&format!("{key}=")), "{key} {file}: {env}");
        assert!(!env.contains(SECRET), "{key} {file}");
        assert!(!env.contains("inherited"), "{key} {file}: {env}");
    }
    assert!(p.dir.join("expression-was-null").exists(), "{key}");
    // Reserved values are injected only into the report-producing job.
    let shell_env = p.read("shell-env.txt");
    let run_id = manifest(&run_dirs(&p)[0])["workflow_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(shell_env.contains(&format!("GOODFLOWS_RUN_ID={run_id}")));
    let report_dir = shell_env
        .lines()
        .find_map(|l| l.strip_prefix("GOODFLOWS_REPORT_DIR="))
        .unwrap();
    assert!(Path::new(report_dir).is_absolute());
    assert!(report_dir.ends_with(&format!("{run_id}/shell/reports")));
    for file in ["exec-env.txt", "step-env.txt"] {
        let env = p.read(file);
        assert!(!env.contains("GOODFLOWS_REPORT_DIR"), "{file}: {env}");
        assert!(!env.contains("GOODFLOWS_RUN_ID"), "{file}: {env}");
    }
    let events = events(&output);
    let summary = events.last().unwrap();
    let stepped = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "stepped")
        .unwrap();
    assert_eq!(stepped["outputs"]["token"], "absent");
    assert_no_secret(&p, &output);
}

#[test]
fn failed_jobs_report_and_skipped_jobs_do_not() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}; exit 4'\nreports = [\"exunit-timings\"]\n[[jobs]]\nname = \"after\"\ncommand = '{}'\ndepends_on = [\"test\"]\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"]),
        copy(&["r2"])
    ));
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let collected = by_type(&events, "reports_collected");
    assert_eq!(collected[0]["job"], "test");
    assert_eq!(collected[0]["reports"], 1);
    assert_eq!(collected[1]["job"], "after");
    assert_eq!(collected[1]["outcome"], "not_executed");
    assert_eq!(collected[1]["reports"], 0);
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["results"][0]["status"], "failed");
    assert_eq!(summary["results"][0]["exit_code"], 4);
    assert_eq!(summary["results"][1]["status"], "skipped");
    let snapshot: Value = serde_json::from_slice(
        &std::fs::read(run_dirs(&p)[0].join("test/pending/000.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot["job_status"], "failed");
    assert_eq!(dir_entries(&run_dirs(&p)[0].join("after/pending")), 0);
}

#[test]
fn condition_skipped_jobs_are_not_executed_and_conditions_get_no_reporting_env() {
    // A condition that tries to plant a report must neither see the directory nor be collected.
    let probe = |file: &str| {
        format!(
            r#"printf "%s" "${{GOODFLOWS_REPORT_DIR-unset}}:${{GOODFLOWS_RUN_ID-unset}}" > {file}; if [ -n "${{GOODFLOWS_REPORT_DIR-}}" ]; then cp fixtures/r1.jsonl "$GOODFLOWS_REPORT_DIR/"; fi"#
        )
    };
    let p = project(&format!(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "skipped"
condition = {{ shell = '{}; exit 1' }}
command = '{}'
reports = ["exunit-timings"]
[[jobs]]
name = "expr"
condition = {{ expression = "env.GOODFLOWS_REPORT_DIR != null || env.GOODFLOWS_RUN_ID != null" }}
command = "touch expr-ran"
reports = ["exunit-timings"]
[[jobs]]
name = "stepped"
reports = ["exunit-timings"]
[[jobs.steps]]
id = "probe"
condition = {{ shell = '{}' }}
command = 'printf "%s" "$GOODFLOWS_REPORT_DIR" > step-dir'
"#,
        probe("cond-seen"),
        copy(&["r2"]),
        probe("step-cond-seen"),
    ));
    // An inherited value must not reach conditions either.
    let inherited = p.dir.join("inherited");
    std::fs::create_dir(&inherited).unwrap();
    let output = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_REPORT_DIR", &inherited)
        .env("GOODFLOWS_RUN_ID", "inherited")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("cond-seen"), "unset:unset");
    assert_eq!(p.read("step-cond-seen"), "unset:unset");
    assert!(!p.dir.join("expr-ran").exists());
    assert_eq!(std::fs::read_dir(&inherited).unwrap().count(), 0);
    let dir = &run_dirs(&p)[0];
    assert!(p.read("step-dir").starts_with(&*dir.to_string_lossy()));
    let events = events(&output);
    let outcome = |job: &str| {
        by_type(&events, "reports_collected")
            .into_iter()
            .find(|e| e["job"] == job)
            .unwrap()
    };
    for job in ["skipped", "expr"] {
        assert_eq!(outcome(job)["outcome"], "not_executed", "{job}");
        assert_eq!(outcome(job)["reports"], 0);
        assert_eq!(dir_entries(&dir.join(job).join("pending")), 0);
    }
    assert_eq!(outcome("stepped")["outcome"], "no_reports");
    let manifest = manifest(dir);
    let job = |name: &str| {
        manifest["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(job("skipped")["status"], "skipped");
    assert_eq!(job("skipped")["executed"], false);
    assert_eq!(job("expr")["executed"], false);
    assert_eq!(job("stepped")["executed"], true);
}

#[test]
fn job_cancelled_before_its_command_spawns_is_not_executed() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncondition = {{ shell = 'touch cond-started; sleep 10' }}\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let child = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SECRET)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&p.dir.join("cond-started"));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(143));
    let events = events(&output);
    let collected = by_type(&events, "reports_collected");
    assert_eq!(collected[0]["outcome"], "not_executed");
    assert_eq!(collected[0]["reports"], 0);
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["jobs"][0]["status"], "cancelled");
    assert_eq!(manifest(dir)["jobs"][0]["executed"], false);
    assert_eq!(mock.connections(), 0);
}

#[test]
fn missing_empty_symlinked_and_late_reports_are_explicit_outcomes() {
    let mock = Mock::store();
    let p = project(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "removed"
command = 'rmdir "$GOODFLOWS_REPORT_DIR"'
reports = ["exunit-timings"]
[[jobs]]
name = "empty"
command = "true"
reports = ["exunit-timings"]
[[jobs]]
name = "escape"
command = 'ln -s "$PWD/fixtures/r1.jsonl" "$GOODFLOWS_REPORT_DIR/r1.jsonl"'
reports = ["exunit-timings"]
[[jobs]]
name = "late"
command = '(sleep 0.5; cp fixtures/r2.jsonl "$GOODFLOWS_REPORT_DIR/late.jsonl") & echo "$GOODFLOWS_REPORT_DIR" > late-dir'
reports = ["exunit-timings"]
"#,
    );
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let events = events(&output);
    let outcome = |job: &str| {
        by_type(&events, "reports_collected")
            .into_iter()
            .find(|e| e["job"] == job)
            .unwrap()
            .clone()
    };
    assert_eq!(outcome("removed")["outcome"], "missing_directory");
    assert_eq!(outcome("empty")["outcome"], "no_reports");
    assert_eq!(outcome("escape")["reports"], 0);
    assert_eq!(outcome("escape")["rejected"], json!({"symlink": 1}));
    // The job's process group is killed before collection, so a late writer never lands.
    assert_eq!(outcome("late")["outcome"], "no_reports");
    std::thread::sleep(Duration::from_millis(800));
    let late_dir = p.read("late-dir");
    assert!(!Path::new(late_dir.trim()).join("late.jsonl").exists());
    assert_eq!(mock.connections(), 0);
}

#[test]
fn parallel_producers_share_one_job_directory() {
    let p = project(
        "[[jobs]]\nname = \"test\"\ncommand = 'for id in r1 r2 r3; do (sleep 0.1; cp \"fixtures/$id.jsonl\" \"$GOODFLOWS_REPORT_DIR/\") & done; wait'\nreports = [\"exunit-timings\"]\n",
    );
    let output = p.run(&["--json"]);
    success(&output);
    assert_eq!(
        by_type(&events(&output), "reports_collected")[0]["report_ids"],
        json!(["r1", "r2", "r3"])
    );
}

#[test]
fn invalid_and_duplicate_reports_are_rejected_without_touching_files() {
    let p = project(
        "[[jobs]]\nname = \"test\"\ncommand = 'cp fixtures/r1.jsonl \"$GOODFLOWS_REPORT_DIR/a.jsonl\"; cp fixtures/r1.jsonl \"$GOODFLOWS_REPORT_DIR/b.jsonl\"; printf \"{bad\\n\" > \"$GOODFLOWS_REPORT_DIR/c.jsonl\"; cp fixtures/r2.jsonl \"$GOODFLOWS_REPORT_DIR/d.jsonl\"'\nreports = [\"exunit-timings\"]\n",
    );
    let output = p.run(&["--json"]);
    success(&output);
    let collected = by_type(&events(&output), "reports_collected")[0].clone();
    assert_eq!(collected["report_ids"], json!(["r2"]));
    assert_eq!(
        collected["rejected"],
        json!({"duplicate_report_id": 2, "malformed_record": 1})
    );
    let dir = run_dirs(&p)[0].join("test/reports");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.jsonl")).unwrap(),
        timing_report("r1", Some("1"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("c.jsonl")).unwrap(),
        "{bad\n"
    );
}

#[test]
fn invalid_filters_are_rejected_without_touching_files() {
    let mock = Mock::store();
    let p = project(
        "[[jobs]]\nname = \"test\"\ncommand = 'cp fixtures/spaced.jsonl fixtures/long.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n",
    );
    let with_include = |id: &str, include: Value| {
        let mut lines: Vec<String> = timing_report(id, Some("1"))
            .lines()
            .map(str::to_owned)
            .collect();
        let mut started: Value = serde_json::from_str(&lines[0]).unwrap();
        started["filters"]["include"] = include;
        lines[0] = started.to_string();
        lines.iter().map(|l| format!("{l}\n")).collect::<String>()
    };
    let spaced = with_include("spaced", json!([{"tag": "slow test", "value": true}]));
    let long = with_include(
        "long",
        Value::Array(vec![json!({"tag": "slow", "value": true}); 257]),
    );
    std::fs::write(p.dir.join("fixtures/spaced.jsonl"), &spaced).unwrap();
    std::fs::write(p.dir.join("fixtures/long.jsonl"), &long).unwrap();
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let collected = by_type(&events(&output), "reports_collected")[0].clone();
    assert_eq!(collected["reports"], 0);
    assert_eq!(collected["rejected"], json!({"unsupported_filter": 2}));
    let dir = run_dirs(&p)[0].join("test");
    let pending = dir.join("pending");
    assert!(!pending.exists() || dir_entries(&pending) == 0);
    assert_eq!(mock.connections(), 0);
    assert_eq!(
        std::fs::read(dir.join("reports/spaced.jsonl")).unwrap(),
        spaced.as_bytes()
    );
    assert_eq!(
        std::fs::read(dir.join("reports/long.jsonl")).unwrap(),
        long.as_bytes()
    );
}

#[test]
fn scheduler_errors_mark_the_manifest_aborted() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    std::fs::write(p.dir.join("logs"), "not a directory").unwrap();
    let output = p.run(&["--json", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    assert_eq!(events.last().unwrap()["type"], "error");
    assert!(by_type(&events, "reports_collected").is_empty());
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["state"], "aborted");
    let replay = upload(&p.dir, dir, None);
    assert_eq!(replay.status.code(), Some(1));
    assert!(stdout(&replay).contains("not finalized"));
}

fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn signal_freezes_snapshots_locally_without_requests() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}; touch written; sleep 10'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let mut command = p.command();
    command
        .args(["--json"])
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SECRET)
        .stdout(Stdio::piped());
    let child = command.spawn().unwrap();
    wait_for(&p.dir.join("written"));
    let start = Instant::now();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(143));
    let events = events(&output);
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["reason"], "cancelled");
    assert_eq!(events.last().unwrap()["type"], "summary");
    assert_eq!(mock.connections(), 0);
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["state"], "finalized");
    assert_eq!(manifest(dir)["jobs"][0]["status"], "cancelled");
    assert!(dir.join("test/pending/000.json").exists());
}

// ---- 3.x delivery ----

#[test]
fn three_partitions_upload_with_shared_identity() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1", "r2", "r3"])
    ));
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let run_id = requests[0].json()["workflow_run_id"].clone();
    let mut report_ids = Vec::new();
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/projects/goodcases/test-reports");
        assert_eq!(request.headers["authorization"], format!("Bearer {SECRET}"));
        assert_eq!(request.headers["content-type"], "application/json");
        let body = request.json();
        assert!(body.get("project").is_none() && body.get("account_id").is_none());
        assert_eq!(body["workflow_run_id"], run_id);
        assert_eq!(body["workflow"], "default");
        assert_eq!(body["environment"], Value::Null);
        assert_eq!(body["job"], "test");
        assert_eq!(body["job_status"], "passed");
        assert_eq!(body["report"]["suite_id"], "suite-1");
        assert_eq!(body["report"]["partition_count"], 3);
        assert_eq!(body["report"]["truncated"], false);
        assert_eq!(body["report"]["records"][1]["duration_us"], 42);
        report_ids.push(body["report"]["report_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(report_ids, ["r1", "r2", "r3"]);
    let events = events(&output);
    let uploaded = by_type(&events, "report_uploaded");
    assert_eq!(uploaded.len(), 3);
    assert_eq!(uploaded[0]["duplicate"], false);
    assert_eq!(events.last().unwrap()["type"], "summary");
    let dir = &run_dirs(&p)[0];
    for i in 0..3 {
        assert!(dir.join(format!("test/acks/{i:03}.json")).exists());
    }
    // Snapshots are kept after acknowledgement and replay sends nothing more.
    assert!(dir.join("test/pending/000.json").exists());
    let replay = upload(&p.dir, dir, Some(&mock.url));
    success(&replay);
    assert_eq!(mock.requests().len(), 3);
    let summary = events_of(&replay).pop().unwrap();
    assert_eq!(summary["type"], "reports_upload_summary");
    assert_eq!(summary["acknowledged"], 3);
    assert_no_secret(&p, &output);
}

fn events_of(output: &std::process::Output) -> Vec<Value> {
    events(output)
}

#[test]
fn delivery_failures_never_change_check_exit_status() {
    type MockFactory = Box<dyn Fn() -> Mock>;
    let cases: Vec<(MockFactory, &str, Option<u16>)> = vec![
        (
            Box::new(|| {
                Mock::start(|_, _| {
                    Reply::Status(404, vec![], r#"{"errors":{"detail":"secret-body"}}"#.into())
                })
            }),
            "project_not_found",
            Some(404),
        ),
        (
            Box::new(|| Mock::start(|_, _| Reply::Status(409, vec![], String::new()))),
            "conflict",
            Some(409),
        ),
        (
            Box::new(|| Mock::start(|_, _| Reply::Status(401, vec![], String::new()))),
            "unauthorized",
            Some(401),
        ),
        (
            Box::new(|| Mock::start(|_, _| Reply::Status(403, vec![], String::new()))),
            "forbidden",
            Some(403),
        ),
        (
            Box::new(|| {
                Mock::start(|r, _| {
                    Reply::Status(
                        200,
                        vec![],
                        ack(
                            "00000000-0000-4000-8000-000000000001",
                            &format!("{}x", r.json()["report"]["report_id"].as_str().unwrap()),
                            false,
                        ),
                    )
                })
            }),
            "mismatched_acknowledgement",
            Some(200),
        ),
        (
            Box::new(|| Mock::start(|_, _| Reply::Status(201, vec![], "{\"data\":\"ok\"}".into()))),
            "invalid_acknowledgement",
            Some(201),
        ),
    ];
    for (command, code) in [("true", 0), ("exit 2", 1)] {
        for (mock, reason, status) in &cases {
            let mock = mock();
            let p = project(&format!(
                "[[jobs]]\nname = \"test\"\ncommand = '{}; {command}'\nreports = [\"exunit-timings\"]\n",
                copy(&["r1"])
            ));
            let output = run_with(&p, &["--json"], Some(&mock.url));
            assert_eq!(output.status.code(), Some(code), "{reason}");
            let events = events(&output);
            let pending = by_type(&events, "report_upload_pending");
            assert_eq!(pending[0]["reason"], *reason);
            assert_eq!(pending[0]["http_status"], json!(status));
            assert_eq!(mock.requests().len(), 1, "{reason} must not retry");
            assert!(!stdout(&output).contains("secret-body"));
            assert_eq!(events.last().unwrap()["type"], "summary");
            assert_eq!(events.last().unwrap()["success"], code == 0);
            let dir = &run_dirs(&p)[0];
            assert!(dir.join("test/pending/000.json").exists());
            assert!(!dir.join("test/acks/000.json").exists());
        }
    }
}

#[test]
fn redirects_are_not_followed_and_credentials_stay_put() {
    let elsewhere = Mock::store();
    let location = format!("{}/api/v1/projects/goodcases/test-reports", elsewhere.url);
    let mock = Mock::start(move |_, _| {
        Reply::Status(
            307,
            vec![("Location".into(), location.clone())],
            String::new(),
        )
    });
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let pending = by_type(&events(&output), "report_upload_pending")[0].clone();
    assert_eq!(pending["reason"], "redirect");
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(elsewhere.connections(), 0);
    assert!(!stdout(&output).contains(&elsewhere.url));
}

#[test]
fn invalid_origins_skip_delivery_without_requests() {
    let mock = Mock::store();
    let port = mock.url.rsplit(':').next().unwrap().to_owned();
    for url in [
        format!("{}/prefix", mock.url),
        format!("{}/?q=1", mock.url),
        format!("{}/#f", mock.url),
        format!("http://user:pw@127.0.0.1:{port}"),
        "http://flows.example.com".to_owned(),
    ] {
        let p = project(&format!(
            "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
            copy(&["r1"])
        ));
        let output = run_with(&p, &["--json"], Some(&url));
        success(&output);
        let skipped = by_type(&events(&output), "reporting_skipped")[0].clone();
        assert_eq!(skipped["reason"], "invalid_url", "{url}");
        assert!(!stdout(&output).contains("pw@"));
    }
    assert_eq!(mock.connections(), 0);
}

#[test]
fn offline_and_transient_failures_retry_then_stay_pending() {
    // A bound-then-dropped port refuses connections.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let start = Instant::now();
    let output = run_with(&p, &["--json"], Some(&format!("http://127.0.0.1:{port}")));
    success(&output);
    assert!(start.elapsed() < Duration::from_secs(20));
    let first = events(&output);
    let pending = by_type(&first, "report_upload_pending")[0].clone();
    assert_eq!(pending["reason"], "transport_error");
    // Retries took seconds, but job duration is command time only.
    assert!(start.elapsed() >= Duration::from_secs(3));
    let summary = first.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert!(summary["results"][0]["duration_ms"].as_u64().unwrap() < 2000);

    // 503 with Retry-After is retried, then succeeds.
    let rows = std::sync::Mutex::new(Default::default());
    let next = std::sync::Mutex::new(0);
    let mock = Mock::start(move |request, index| {
        if index == 0 {
            Reply::Status(503, vec![("Retry-After".into(), "0".into())], String::new())
        } else {
            common::mock::store_reply(&rows, &next, request)
        }
    });
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    assert_eq!(by_type(&events(&output), "report_uploaded").len(), 1);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

#[test]
fn ack_state_write_failure_keeps_snapshot_replayable() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{} && rmdir \"$GOODFLOWS_REPORT_DIR/../acks\" && touch \"$GOODFLOWS_REPORT_DIR/../acks\"'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let pending = by_type(&events(&output), "report_upload_pending")[0].clone();
    assert_eq!(pending["reason"], "state_write_failed");
    let dir = &run_dirs(&p)[0];
    assert!(dir.join("test/pending/000.json").exists());
    std::fs::remove_file(dir.join("test/acks")).unwrap();
    let replay = upload(&p.dir, dir, Some(&mock.url));
    success(&replay);
    let uploaded = by_type(&events(&replay), "report_uploaded")[0].clone();
    assert_eq!(uploaded["duplicate"], true);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

#[test]
fn text_output_shows_delivery_and_summary_stays_last() {
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let output = run_with(&p, &[], Some(&mock.url));
    success(&output);
    let text = stdout(&output);
    assert!(
        text.contains("reports test: 1 collected (collected)"),
        "{text}"
    );
    assert!(text.contains("report test/r1 uploaded"));
    let summary = text.find("Summary:").unwrap();
    assert!(text.find("report test/r1 uploaded").unwrap() < summary);
    let p2 = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let conflict = Mock::start(|_, _| Reply::Status(409, vec![], String::new()));
    let text = stdout(&run_with(&p2, &[], Some(&conflict.url)));
    assert!(
        text.contains("report test/r1 pending: conflict (HTTP 409), not retried by reports upload"),
        "{text}"
    );

    // Rejection codes, the saved run directory and duplicate acknowledgements.
    let p3 = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{} && printf \"{{bad\\n\" > \"$GOODFLOWS_REPORT_DIR/bad.jsonl\"'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let stored = Mock::start(|r, _| {
        let id = r.json()["report"]["report_id"].as_str().unwrap().to_owned();
        Reply::Status(
            200,
            vec![],
            ack("00000000-0000-4000-8000-000000000009", &id, true),
        )
    });
    let output = run_with(&p3, &[], Some(&stored.url));
    success(&output);
    let text = stdout(&output);
    let run_dir = run_dirs(&p3).pop().unwrap();
    assert!(
        text.contains("reports test: 1 collected (collected)  rejected: malformed_record=1"),
        "{text}"
    );
    assert!(
        text.contains(&format!("  saved in {}", run_dir.display())),
        "{text}"
    );
    assert!(
        text.contains("report test/r1 uploaded (already stored)"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("All jobs passed"), "{text}");

    // Delivery skips.
    let text = stdout(&run_with(&p3, &[], Some("http://flows.example.com")));
    assert!(
        text.contains("reporting skipped: invalid url (1 reports kept locally)"),
        "{text}"
    );
    let p4 = project_with(
        "[reporting]\nproject = \"goodcases\"\nspool_dir = \"blocker/spool\"\n",
        "[[jobs]]\nname = \"test\"\ncommand = \"true\"\nreports = [\"exunit-timings\"]\n",
    );
    std::fs::write(p4.dir.join("blocker"), "file").unwrap();
    let output = p4.run(&[]);
    success(&output);
    let text = stdout(&output);
    assert!(
        text.contains("reporting skipped: spool unavailable\n"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("All jobs passed"), "{text}");
}

// ---- 4.x replay ----

fn saved_run(mock_url: Option<&str>) -> (Project, PathBuf) {
    let p = project_with(
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"SAVED_URL\"\ntoken_env = \"SAVED_KEY\"\n",
        &format!(
            "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
            copy(&["r1", "r2"])
        ),
    );
    let mut command = p.command();
    command.args(["--json"]);
    if let Some(url) = mock_url {
        command.env("SAVED_URL", url).env("SAVED_KEY", SECRET);
    }
    success(&command.output().unwrap());
    let dir = run_dirs(&p).pop().unwrap();
    (p, dir)
}

fn replay(cwd: &Path, run_dir: &Path, url: Option<&str>, extra: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_goodflows"));
    command
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .args(["reports", "upload"])
        .arg(run_dir)
        .args(extra);
    if let Some(url) = url {
        command.env("SAVED_URL", url).env("SAVED_KEY", SECRET);
    }
    command.output().unwrap()
}

#[test]
fn replay_runs_outside_the_checkout_from_saved_snapshots_only() {
    let (p, dir) = saved_run(None);
    // Move the run elsewhere and delete the checkout's config and fixtures.
    let elsewhere = tempfile::tempdir().unwrap();
    let moved = elsewhere.path().join("run");
    std::fs::rename(&dir, &moved).unwrap();
    std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
    std::fs::remove_dir_all(p.dir.join("fixtures")).unwrap();
    // A fake git on PATH proves replay never reads current Git state.
    let bin = elsewhere.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = elsewhere.path().join("git-called");
    std::fs::write(
        bin.join("git"),
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let before = std::fs::read(moved.join("test/pending/000.json")).unwrap();

    let mock = Mock::store();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_goodflows"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    command
        .current_dir(elsewhere.path())
        .env("PATH", path)
        .env("SAVED_URL", &mock.url)
        .env("SAVED_KEY", SECRET)
        .env_remove("GOODFLOWS_URL")
        .env_remove("GOODFLOWS_API_KEY")
        .args(["reports", "upload", "run", "--json"]);
    let output = command.output().unwrap();
    success(&output);
    assert!(!marker.exists());
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, before);
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {SECRET}")
    );
    let events = events(&output);
    assert_eq!(by_type(&events, "report_uploaded").len(), 2);
    assert!(by_type(&events, "job_started").is_empty());
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "reports_upload_summary");
    assert_eq!(summary["workflow"], "default");
    assert_eq!(
        (summary["acknowledged"].clone(), summary["pending"].clone()),
        (json!(2), json!(0))
    );

    // Everything already acknowledged: no requests, exit 0.
    let again = replay(elsewhere.path(), &moved, Some(&mock.url), &[]);
    success(&again);
    assert_eq!(mock.requests().len(), 2);
    assert!(stdout(&again).contains("Reports: 2 acknowledged, 0 pending, 0 rejected, 0 invalid"));
}

#[test]
fn replay_completes_after_a_lost_acknowledgement() {
    // The server commits every request, but the first two responses are lost.
    let rows = std::sync::Mutex::new(Default::default());
    let next = std::sync::Mutex::new(0);
    let mock = Mock::start(move |request, index| {
        let reply = common::mock::store_reply(&rows, &next, request);
        if index < 2 {
            Reply::Close
        } else {
            reply
        }
    });
    let (p, dir) = saved_run(None);
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let uploaded = by_type(&events(&output), "report_uploaded");
    assert_eq!(uploaded.len(), 2);
    // r1 was committed by the lost first attempt, so its retry is an idempotent duplicate.
    assert_eq!(uploaded[0]["report_id"], "r1");
    assert_eq!(uploaded[0]["duplicate"], true);
    let requests = mock.requests();
    assert_eq!(requests[0].body, requests[1].body);
    assert!(dir.join("test/acks/000.json").exists());
    assert!(dir.join("test/acks/001.json").exists());
    let first_ack = std::fs::read(dir.join("test/acks/000.json")).unwrap();
    // A later replay sends nothing and keeps the recorded acknowledgement.
    let again = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&again);
    assert_eq!(mock.requests().len(), requests.len());
    assert_eq!(
        std::fs::read(dir.join("test/acks/000.json")).unwrap(),
        first_ack
    );
}

#[test]
fn replay_exits_nonzero_while_reports_remain_pending_or_invalid() {
    let (p, dir) = saved_run(None);
    // No credentials.
    let output = replay(&p.dir, &dir, None, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        by_type(&events(&output), "reporting_skipped")[0]["reason"],
        "credentials_unavailable"
    );
    // Invalid origin.
    let output = replay(&p.dir, &dir, Some("http://flows.example.com"), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        by_type(&events(&output), "reporting_skipped")[0]["reason"],
        "invalid_url"
    );
    // Terminal 401.
    let mock = Mock::start(|_, _| Reply::Status(401, vec![], String::new()));
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        by_type(&events(&output), "report_upload_pending")[0]["reason"],
        "unauthorized"
    );
    // One upload succeeds but another snapshot is corrupt.
    std::fs::write(dir.join("test/pending/001.json"), "{}").unwrap();
    let store = Mock::store();
    let output = replay(&p.dir, &dir, Some(&store.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = events(&output).pop().unwrap();
    assert_eq!(
        (summary["acknowledged"].clone(), summary["invalid"].clone()),
        (json!(1), json!(1))
    );
}

#[test]
fn replay_refuses_unfinished_manifests_and_leaves_files() {
    let (p, dir) = saved_run(None);
    let path = dir.join("manifest.json");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, original.replace("\"finalized\"", "\"running\"")).unwrap();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let event = events(&output).pop().unwrap();
    assert_eq!(event["type"], "error");
    assert!(event["message"].as_str().unwrap().contains("not finalized"));
    assert_eq!(mock.connections(), 0);
    assert!(dir.join("test/reports/r1.jsonl").exists());
    assert!(dir.join("test/pending/000.json").exists());
    let missing = replay(&p.dir, &p.dir.join("nope"), Some(&mock.url), &[]);
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn concurrent_replays_merge_acknowledgements() {
    let (p, dir) = saved_run(None);
    let store = Mock::store();
    let spawn = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_goodflows"));
        command
            .current_dir(&p.dir)
            .args(["reports", "upload", "--json"])
            .arg(&dir)
            .env("SAVED_URL", &store.url)
            .env("SAVED_KEY", SECRET)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let (a, b) = (spawn(), spawn());
    success(&a.wait_with_output().unwrap());
    success(&b.wait_with_output().unwrap());
    for ack in ["test/acks/000.json", "test/acks/001.json"] {
        let ack: Value = serde_json::from_slice(&std::fs::read(dir.join(ack)).unwrap()).unwrap();
        assert!(ack["id"]
            .as_str()
            .unwrap()
            .starts_with("00000000-0000-4000-8000-"));
    }
    // The store held one row per identity no matter how many requests arrived.
    let mut ids: Vec<_> = store
        .requests()
        .iter()
        .map(|r| r.json()["report"]["report_id"].clone())
        .collect();
    ids.sort_by_key(|v| v.to_string());
    ids.dedup();
    assert_eq!(ids, [json!("r1"), json!("r2")]);
    success(&replay(&p.dir, &dir, Some(&store.url), &[]));
}

#[test]
fn replay_signal_code_takes_precedence() {
    let (p, dir) = saved_run(None);
    let mock = Mock::start(|_, _| Reply::Hang(Duration::from_secs(20)));
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_goodflows"))
        .current_dir(&p.dir)
        .args(["reports", "upload", "--json"])
        .arg(&dir)
        .env("SAVED_URL", &mock.url)
        .env("SAVED_KEY", SECRET)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while mock.requests().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(130));
    let events = events(&output);
    assert!(by_type(&events, "report_upload_pending")
        .iter()
        .all(|e| e["reason"] == "cancelled"));
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn replay_rejects_flags_that_do_not_apply() {
    let (p, dir) = saved_run(None);
    for flag in [
        vec!["--jobs", "test"],
        vec!["--env", "x"],
        vec!["--config", "goodflows.toml"],
        vec!["--parallelism", "2"],
        vec!["--fail-fast"],
        vec!["--no-fail-fast"],
        vec!["--log-dir", "logs"],
    ] {
        let mut args = flag.clone();
        args.push("--json");
        let output = replay(&p.dir, &dir, None, &args);
        assert_eq!(output.status.code(), Some(1), "{flag:?}");
        let event = events(&output).pop().unwrap();
        assert!(event["message"]
            .as_str()
            .unwrap()
            .contains("cannot be used with reports upload"));
    }
}

// ---- review follow-ups ----

fn job_event(events: &[Value], kind: &str, job: &str) -> Value {
    by_type(events, kind)
        .into_iter()
        .find(|e| e["job"] == job)
        .unwrap_or_else(|| panic!("no {kind} for {job}"))
}

fn snapshot_of(dir: &Path, job: &str) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(job).join("pending/000.json")).unwrap()).unwrap()
}

#[test]
fn run_signal_during_publish_stops_promptly() {
    let mock = Mock::start(|_, _| Reply::Hang(Duration::from_secs(20)));
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let child = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SECRET)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while mock.requests().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no request arrived"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let signalled = Instant::now();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let output = child.wait_with_output().unwrap();
    assert!(signalled.elapsed() < Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(143));
    let events = events(&output);
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["reason"], "cancelled");
    assert_eq!(events.last().unwrap()["type"], "summary");
    assert_eq!(events.last().unwrap()["success"], true);
    assert_eq!(mock.requests().len(), 1);
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["state"], "finalized");
    assert!(!dir.join("test/acks/000.json").exists());
    assert!(dir.join("test/pending/000.json").exists());
}

#[test]
fn timed_out_failed_step_and_condition_skipped_step_jobs() {
    let p = project(&format!(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "slow"
timeout_secs = 1
command = '{} && sleep 10'
reports = ["exunit-timings"]
[[jobs]]
name = "stepped"
reports = ["exunit-timings"]
[[jobs.steps]]
id = "write"
command = '{}'
[[jobs.steps]]
id = "fail"
command = "exit 5"
[[jobs]]
name = "never"
reports = ["exunit-timings"]
[[jobs.steps]]
id = "a"
condition = {{ expression = "false" }}
command = "touch never-a"
[[jobs.steps]]
id = "b"
condition = {{ expression = "false" }}
command = "touch never-b"
"#,
        copy(&["r1"]),
        copy(&["r2"])
    ));
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let dir = &run_dirs(&p)[0];
    assert_eq!(
        job_event(&events, "reports_collected", "slow")["report_ids"],
        json!(["r1"])
    );
    assert_eq!(snapshot_of(dir, "slow")["job_status"], "timed_out");
    assert_eq!(
        job_event(&events, "reports_collected", "stepped")["report_ids"],
        json!(["r2"])
    );
    assert_eq!(snapshot_of(dir, "stepped")["job_status"], "failed");
    // Every step condition was false, so no command ever spawned.
    let never = job_event(&events, "reports_collected", "never");
    assert_eq!(never["outcome"], "not_executed");
    assert!(!p.dir.join("never-a").exists() && !p.dir.join("never-b").exists());
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["timed_out"], 1);
    assert_eq!(summary["failed"], 1);
}

#[test]
fn snapshot_write_failures_are_reported_and_never_follow_symlinks() {
    let p = project(&format!(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "blocked"
command = '{} && rmdir "$GOODFLOWS_REPORT_DIR/../pending" && touch "$GOODFLOWS_REPORT_DIR/../pending"'
reports = ["exunit-timings"]
[[jobs]]
name = "linked"
command = '{} && mkdir -p elsewhere && rmdir "$GOODFLOWS_REPORT_DIR/../pending" && ln -s "$PWD/elsewhere" "$GOODFLOWS_REPORT_DIR/../pending"'
reports = ["exunit-timings"]
"#,
        copy(&["r1"]),
        copy(&["r2"])
    ));
    let output = p.run(&["--json"]);
    success(&output);
    let events = events(&output);
    for job in ["blocked", "linked"] {
        let collected = job_event(&events, "reports_collected", job);
        assert_eq!(collected["reports"], 0, "{job}");
        assert_eq!(
            collected["rejected"],
            json!({"snapshot_write_failed": 1}),
            "{job}"
        );
    }
    assert_eq!(dir_entries(&p.dir.join("elsewhere")), 0);
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["state"], "finalized");
    // The raw report stays for diagnosis.
    assert!(dir.join("blocked/reports/r1.jsonl").exists());
    assert_eq!(events.last().unwrap()["type"], "summary");
}

#[test]
fn finalize_failure_keeps_everything_local() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: permission checks do not apply to root");
        return;
    }
    let mock = Mock::store();
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{} && chmod 500 \"$GOODFLOWS_REPORT_DIR/../..\"'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let output = run_with(&p, &["--json"], Some(&mock.url));
    let dir = run_dirs(&p).pop().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    success(&output);
    let events = events(&output);
    assert_eq!(by_type(&events, "reports_collected")[0]["reports"], 1);
    let skipped = by_type(&events, "reporting_skipped")[0].clone();
    assert_eq!(skipped["reason"], "spool_unavailable");
    assert_eq!(skipped["pending"], 1);
    assert_eq!(mock.connections(), 0);
    assert_eq!(events.last().unwrap()["type"], "summary");
    assert_eq!(manifest(&dir)["state"], "running");
    let replay = upload(&p.dir, &dir, Some(&mock.url));
    assert_eq!(replay.status.code(), Some(1));
    assert!(stdout(&replay).contains("not finalized"));
}

#[test]
fn closed_stdout_still_finalizes_the_run() {
    use std::io::BufRead;
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let mut child = p
        .command()
        .arg("--json")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 && !line.contains("job_finished") {
        line.clear();
    }
    drop(reader);
    child.wait().unwrap();
    let dir = &run_dirs(&p)[0];
    assert_eq!(manifest(dir)["state"], "finalized");
    assert_eq!(
        manifest(dir)["jobs"][0]["collection"]["reports"][0]["index"],
        0
    );
    assert!(dir.join("test/pending/000.json").exists());
}

#[test]
fn truncated_reports_upload_and_oversized_sources_stay_on_disk() {
    let p = project(
        r#"[[jobs]]
name = "test"
command = 'head -n 2 fixtures/r1.jsonl > "$GOODFLOWS_REPORT_DIR/a.jsonl" && printf "{\"event\":\"test_fin" >> "$GOODFLOWS_REPORT_DIR/a.jsonl" && truncate -s 40M "$GOODFLOWS_REPORT_DIR/big.jsonl"'
reports = ["exunit-timings"]
"#,
    );
    let mock = Mock::store();
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let collected = by_type(&events(&output), "reports_collected")[0].clone();
    assert_eq!(collected["report_ids"], json!(["r1"]));
    assert_eq!(collected["rejected"], json!({"request_too_large": 1}));
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let body = requests[0].json();
    assert_eq!(body["report"]["truncated"], true);
    assert_eq!(body["report"]["records"].as_array().unwrap().len(), 2);
    let big = run_dirs(&p)[0].join("test/reports/big.jsonl");
    assert_eq!(std::fs::metadata(big).unwrap().len(), 40 * 1024 * 1024);
}

#[test]
fn state_directories_are_private_and_exist_before_jobs_run() {
    use std::os::unix::fs::PermissionsExt;
    let p = project(
        "[[jobs]]\nname = \"test\"\ncommand = 'ls -d \"$GOODFLOWS_REPORT_DIR/../pending\" \"$GOODFLOWS_REPORT_DIR/../acks\" > seen'\nreports = [\"exunit-timings\"]\n",
    );
    success(&p.run(&["--json"]));
    assert_eq!(p.read("seen").lines().count(), 2);
    let dir = &run_dirs(&p)[0];
    for path in [
        dir.clone(),
        dir.join("test"),
        dir.join("test/reports"),
        dir.join("test/pending"),
        dir.join("test/acks"),
    ] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", path.display());
    }
}

fn edit_manifest(dir: &Path, edit: impl Fn(&mut Value)) {
    let mut value = manifest(dir);
    edit(&mut value);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

#[test]
fn replay_refuses_tampered_manifests_without_contacting_the_server() {
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|m| m["version"] = json!(3)),
            "unsupported manifest version 3",
        ),
        (
            Box::new(|m| m["version"] = json!(2)),
            "manifest version 2 requires at least one job with partition entries",
        ),
        (
            Box::new(|m| m["token_env"] = json!("GOODFLOWS_OUTPUT")),
            "token_env must not name a reserved",
        ),
        (
            Box::new(|m| m["url_env"] = json!("GOODFLOWS_REPORT_DIR")),
            "url_env must not name a reserved",
        ),
        (
            Box::new(|m| m["url_env"] = json!("SAVED_KEY")),
            "must name different variables",
        ),
        (
            Box::new(|m| m["token_env"] = json!("BAD NAME")),
            "token_env must be an environment variable name",
        ),
        (
            Box::new(|m| m["project"] = json!("../admin")),
            "project must be",
        ),
        (
            Box::new(|m| m["project"] = json!("goodcases?x=1")),
            "project must be",
        ),
        (
            Box::new(|m| {
                let id = m["workflow_run_id"].as_str().unwrap().to_uppercase();
                m["workflow_run_id"] = json!(id);
            }),
            "lowercase canonical UUID",
        ),
        (
            Box::new(|m| m["jobs"][0]["name"] = json!("j".repeat(65))),
            "job names",
        ),
        (
            Box::new(|m| m["jobs"][0]["name"] = json!("a\u{7}b")),
            "job names",
        ),
        (
            Box::new(|m| {
                let job = m["jobs"][0].clone();
                m["jobs"].as_array_mut().unwrap().push(job);
            }),
            "duplicate job name",
        ),
        (Box::new(|m| m["workflow"] = json!("")), "workflow name"),
    ];
    let mock = Mock::store();
    for (edit, message) in cases {
        let (p, dir) = saved_run(None);
        edit_manifest(&dir, edit);
        let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
        assert_eq!(output.status.code(), Some(1), "{message}");
        let event = events(&output).pop().unwrap();
        assert_eq!(event["type"], "error");
        let text = event["message"].as_str().unwrap();
        assert!(text.contains(message), "{message}: {text}");
    }
    assert_eq!(mock.connections(), 0);
}

#[test]
fn replay_derives_paths_and_refuses_escapes() {
    let (p, dir) = saved_run(None);
    // A planted target outside the run must never be read or written.
    let outside = dir.parent().unwrap().parent().unwrap().join("x");
    std::fs::create_dir_all(outside.join("pending")).unwrap();
    std::fs::copy(
        dir.join("test/pending/000.json"),
        outside.join("pending/000.json"),
    )
    .unwrap();
    edit_manifest(&dir, |m| m["jobs"][0]["dir"] = json!("../../x"));
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|e| e["reason"] == "invalid_snapshot"));
    assert_eq!(events.last().unwrap()["invalid"], 2);
    assert_eq!(mock.connections(), 0);
    assert!(!outside.join("acks").exists());
}

#[test]
fn replay_revalidates_snapshots_before_sending() {
    let mock = Mock::store();
    // A modified snapshot (extra field), a symlinked snapshot and an oversized one.
    let (p, dir) = saved_run(None);
    let path = dir.join("test/pending/000.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["project"] = json!("goodcases");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let copy = p.dir.join("copy.json");
    std::fs::rename(dir.join("test/pending/001.json"), &copy).unwrap();
    std::os::unix::fs::symlink(&copy, dir.join("test/pending/001.json")).unwrap();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    assert!(by_type(&events, "report_upload_pending")
        .iter()
        .all(|e| e["reason"] == "invalid_snapshot"));
    assert_eq!(events.last().unwrap()["invalid"], 2);
    assert_eq!(mock.connections(), 0);

    let (p, dir) = saved_run(None);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("test/pending/000.json"))
        .unwrap();
    file.set_len(32 * 1024 * 1024 + 1).unwrap();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = events_of(&output).pop().unwrap();
    assert_eq!(
        (summary["acknowledged"].clone(), summary["invalid"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.requests()[0].json()["report"]["report_id"], "r2");
}

#[test]
fn replay_resends_malformed_or_mismatched_acknowledgements() {
    let (p, dir) = saved_run(None);
    std::fs::write(dir.join("test/acks/000.json"), "{}").unwrap();
    std::fs::write(
        dir.join("test/acks/001.json"),
        r#"{"id":"00000000-0000-4000-8000-000000000001","report_id":"r1","status":"complete","duplicate":false}"#,
    )
    .unwrap();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    assert_eq!(mock.requests().len(), 2);
    for (ack, report_id) in [("000", "r1"), ("001", "r2")] {
        let ack: Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("test/acks/{ack}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(ack["report_id"], report_id);
    }
}

#[test]
fn server_rejections_are_recorded_and_not_resent() {
    let conflict = Mock::start(|_, _| Reply::Status(409, vec![], String::new()));
    let (p, dir) = saved_run(Some(&conflict.url));
    assert_eq!(conflict.requests().len(), 2);
    let marker: Value =
        serde_json::from_slice(&std::fs::read(dir.join("test/acks/000.rejected.json")).unwrap())
            .unwrap();
    assert_eq!(
        marker,
        json!({"report_id":"r1", "reason":"conflict", "http_status":409})
    );
    let store = Mock::store();
    let output = replay(&p.dir, &dir, Some(&store.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(store.connections(), 0);
    let events = events(&output);
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .all(|e| e["reason"] == "rejected_by_server" && e["http_status"] == 409));
    let summary = events.last().unwrap();
    assert_eq!(
        (summary["rejected"].clone(), summary["success"].clone()),
        (json!(2), json!(false))
    );
    // Removing a marker makes that report replayable again.
    std::fs::remove_file(dir.join("test/acks/000.rejected.json")).unwrap();
    let output = replay(&p.dir, &dir, Some(&store.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(store.requests().len(), 1);
    let summary = events_of(&output).pop().unwrap();
    assert_eq!(
        (summary["acknowledged"].clone(), summary["rejected"].clone()),
        (json!(1), json!(1))
    );
    // Other terminal statuses are recorded the same way; auth failures are not.
    for (code, terminal) in [
        (400, true),
        (413, true),
        (415, true),
        (422, true),
        (401, false),
        (404, false),
    ] {
        let mock = Mock::start(move |_, _| Reply::Status(code, vec![], String::new()));
        let (_p, dir) = saved_run(Some(&mock.url));
        assert_eq!(
            dir.join("test/acks/000.rejected.json").exists(),
            terminal,
            "{code}"
        );
    }
}

#[test]
fn replay_with_collection_rejections_sends_the_rest_and_exits_nonzero() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = '{} && printf \"{{bad\\n\" > \"$GOODFLOWS_REPORT_DIR/bad.jsonl\"'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    success(&p.run(&["--json"]));
    let dir = run_dirs(&p).pop().unwrap();
    let mock = Mock::store();
    for expected_requests in [1, 1] {
        let output = upload(&p.dir, &dir, Some(&mock.url));
        assert_eq!(output.status.code(), Some(1));
        let summary = events(&output).pop().unwrap();
        assert_eq!(
            (
                summary["acknowledged"].clone(),
                summary["invalid"].clone(),
                summary["pending"].clone()
            ),
            (json!(1), json!(1), json!(0))
        );
        assert_eq!(mock.requests().len(), expected_requests);
    }
}

#[test]
fn replay_budget_flag_is_bounded() {
    let (p, dir) = saved_run(None);
    let mock = Mock::store();
    success(&replay(&p.dir, &dir, Some(&mock.url), &["--budget", "120"]));
    for bad in ["0", "3601", "x"] {
        let output = replay(&p.dir, &dir, None, &["--budget", bad]);
        assert_eq!(output.status.code(), Some(2), "{bad}");
    }
}

#[test]
fn unpartitioned_jobs_never_see_inherited_partition_variables_with_reporting_enabled() {
    // Reporting adds env removals and injections; inherited partition values must
    // still be removed from report-producing and ordinary jobs alike.
    let unset = r#"test -z "${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}""#;
    let p = project(&format!(
        r#"
[[jobs]]
name = "reporter"
condition = {{ expression = 'env.GOODFLOWS_PARTITION == null && env.GOODFLOWS_PARTITION_COUNT == null' }}
command = '''{unset} && test -n "$GOODFLOWS_REPORT_DIR" && {copy} && touch reporter'''
reports = ["exunit-timings"]
[[jobs]]
name = "plain"
condition = {{ shell = '''{unset}''' }}
command = '''{unset} && touch plain'''
[[jobs]]
name = "direct"
shell = false
command = "sh -c '{direct} && touch direct'"
[[jobs]]
name = "stepped"
reports = ["exunit-timings"]
[[jobs.steps]]
id = "a"
condition = {{ shell = '''{unset}''' }}
command = '''{unset} && touch stepped'''
"#,
        copy = copy(&["r1"]),
        direct = unset.replace('"', "\\\""),
    ));
    let output = p
        .command()
        .env("GOODFLOWS_PARTITION", "2")
        .env("GOODFLOWS_PARTITION_COUNT", "3")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    for file in ["reporter", "plain", "direct", "stepped"] {
        assert!(p.dir.join(file).exists(), "{file}: {}", stdout(&output));
    }
    let events = events(&output);
    let collected = by_type(&events, "reports_collected");
    let reporter = collected.iter().find(|e| e["job"] == "reporter").unwrap();
    assert_eq!(reporter["report_ids"], json!(["r1"]), "{collected:?}");
}

// ---- partitioned report collection ----

/// A 3-partition reporting job whose instance `i` writes the fixture claiming
/// partition `4 - i` (so Goodflows' identity must win), then runs `tail`.
///
/// `fail_fast` defaults to true: when `tail` makes an instance fail, any
/// sibling not yet started (always the case at `parallelism` 1 or 2) is
/// cancelled and produces nothing. Tests with an intentionally failing instance
/// use [`partitioned_without_fail_fast`].
fn partitioned(tail: &str) -> Project {
    partitioned_with("", tail)
}

/// [`partitioned`] with `fail_fast = false`, so every instance runs whatever
/// the machine's parallelism.
fn partitioned_without_fail_fast(tail: &str) -> Project {
    partitioned_with("[settings]\nfail_fast = false\n", tail)
}

fn partitioned_with(settings: &str, tail: &str) -> Project {
    project(&format!(
        "{settings}[[jobs]]\nname = \"test\"\npartitions = 3\ncommand = 'printf \"%s %s\" \"$GOODFLOWS_RUN_ID\" \"$GOODFLOWS_REPORT_DIR\" > seen-$GOODFLOWS_PARTITION && cp fixtures/r$((4 - GOODFLOWS_PARTITION)).jsonl \"$GOODFLOWS_REPORT_DIR/\"{tail}'\nreports = [\"exunit-timings\"]\n"
    ))
}

#[test]
fn partitioned_instances_get_their_own_directories_and_share_one_run_id() {
    let p = partitioned("");
    let output = p.run(&["--json"]);
    success(&output);
    let dir = run_dirs(&p).pop().unwrap();
    let run_id = dir.file_name().unwrap().to_str().unwrap().to_owned();
    let canonical = dir.canonicalize().unwrap();
    let mut seen_dirs = Vec::new();
    for i in 1..=3 {
        let seen = p.read(&format!("seen-{i}"));
        let (id, report_dir) = seen.split_once(' ').unwrap();
        assert_eq!(id, run_id);
        assert_eq!(
            Path::new(report_dir).canonicalize().unwrap(),
            canonical.join(format!("test/p{i}/reports"))
        );
        seen_dirs.push(report_dir.to_owned());
    }
    seen_dirs.dedup();
    assert_eq!(seen_dirs.len(), 3);
    // Every instance numbers its snapshots from 0 in its own pending/ directory.
    for (i, report_id) in [(1, "r3"), (2, "r2"), (3, "r1")] {
        let base = dir.join(format!("test/p{i}"));
        assert_eq!(dir_entries(&base.join("pending")), 1);
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(base.join("pending/000.json")).unwrap()).unwrap();
        assert_eq!(snapshot["report"]["report_id"], report_id);
        // Goodflows' identity replaces the producer's claim in both places.
        let partition = i.to_string();
        assert_eq!(snapshot["report"]["partition"], partition.as_str());
        assert_eq!(snapshot["report"]["partition_count"], 3);
        assert_eq!(
            snapshot["report"]["records"][0]["partition"],
            partition.as_str()
        );
        assert_eq!(snapshot["report"]["records"][0]["partition_count"], 3);
    }
    assert!(!dir.join("test/reports").exists());
    assert!(!dir.join("test/pending").exists());
    use std::os::unix::fs::PermissionsExt;
    for sub in ["", "reports", "pending", "acks"] {
        let path = dir.join("test/p2").join(sub);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", path.display());
    }
}

#[test]
fn partitioned_instances_never_see_inherited_reporting_values() {
    let p = partitioned("");
    // As in a nested invocation: an enclosing goodflows exported its own values.
    let output = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_REPORT_DIR", p.dir.join("inherited"))
        .env("GOODFLOWS_RUN_ID", "inherited-run-id")
        .output()
        .unwrap();
    success(&output);
    let dir = run_dirs(&p).pop().unwrap();
    let run_id = dir.file_name().unwrap().to_str().unwrap().to_owned();
    let canonical = dir.canonicalize().unwrap();
    for i in 1..=3 {
        let seen = p.read(&format!("seen-{i}"));
        assert!(!seen.contains("inherited"), "{i}: {seen}");
        let (id, report_dir) = seen.split_once(' ').unwrap();
        assert_eq!(id, run_id, "{i}");
        assert_eq!(
            Path::new(report_dir).canonicalize().unwrap(),
            canonical.join(format!("test/p{i}/reports"))
        );
    }
    assert!(!p.dir.join("inherited").exists());
}

#[test]
fn instance_state_directories_replaced_by_symlinks_are_refused() {
    let mock = Mock::store();
    // Instance 2 points pending/ outside; instance 3 replaces acks/ with a link.
    let p = project(
        "[[jobs]]\nname = \"test\"\npartitions = 3\ncommand = 'cp fixtures/r$GOODFLOWS_PARTITION.jsonl \"$GOODFLOWS_REPORT_DIR/\"; d=$(dirname \"$GOODFLOWS_REPORT_DIR\"); mkdir -p \"$PWD/outside-$GOODFLOWS_PARTITION\"; case $GOODFLOWS_PARTITION in 2) rmdir \"$d/pending\"; ln -s \"$PWD/outside-2\" \"$d/pending\";; 3) rmdir \"$d/acks\"; ln -s \"$PWD/outside-3\" \"$d/acks\";; esac'\nreports = [\"exunit-timings\"]\n",
    );
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let events = events(&output);
    let instance = |kind: &str, index: u64| {
        by_type(&events, kind)
            .into_iter()
            .find(|e| e["partition"]["index"] == index)
    };
    let blocked = instance("reports_collected", 2).unwrap();
    assert_eq!(blocked["reports"], 0);
    assert_eq!(blocked["rejected"], json!({"snapshot_write_failed": 1}));
    assert_eq!(dir_entries(&p.dir.join("outside-2")), 0);
    // Instance 3's snapshot is sent, but its acknowledgement is not written through the link.
    let pending = instance("report_upload_pending", 3).unwrap();
    assert_eq!(pending["reason"], "state_write_failed");
    assert_eq!(pending["report_id"], "r3");
    assert_eq!(dir_entries(&p.dir.join("outside-3")), 0);
    let dir = run_dirs(&p).pop().unwrap();
    assert!(dir.join("test/p3/pending/000.json").exists());
    // Instance 1 is unaffected.
    assert_eq!(instance("report_uploaded", 1).unwrap()["report_id"], "r1");
    assert!(dir.join("test/p1/acks/000.json").exists());
    assert_eq!(by_type(&events, "report_uploaded").len(), 1);
    assert_eq!(manifest(&dir)["state"], "finalized");
}

#[test]
fn unpartitioned_reporting_layout_and_manifest_are_unchanged() {
    let p = project(&format!(
        "[[jobs]]\nname = \"test\"\ncommand = 'printf \"%s\" \"$GOODFLOWS_REPORT_DIR\" > seen && {}'\nreports = [\"exunit-timings\"]\n",
        copy(&["r1"])
    ));
    let output = p.run(&["--json"]);
    success(&output);
    let dir = run_dirs(&p).pop().unwrap();
    assert_eq!(
        Path::new(&p.read("seen")).canonicalize().unwrap(),
        dir.canonicalize().unwrap().join("test/reports")
    );
    let job = &manifest(&dir)["jobs"][0];
    let mut keys: Vec<_> = job.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["collection", "dir", "executed", "name", "status"]);
    for kind in ["reports_collected", "report_upload_pending"] {
        for event in by_type(&events(&output), kind) {
            assert!(event.get("partition").is_none(), "{event}");
        }
    }
}

#[test]
fn partitioned_manifest_has_one_entry_per_instance_with_its_own_status() {
    let mock = Mock::store();
    // Instance 2 fails; without fail_fast its siblings still run at any parallelism.
    let p = partitioned_without_fail_fast(" && [ $GOODFLOWS_PARTITION != 2 ]");
    let output = run_with(&p, &["--json"], Some(&mock.url));
    assert_eq!(output.status.code(), Some(1));
    let dir = run_dirs(&p).pop().unwrap();
    let jobs = manifest(&dir)["jobs"].as_array().unwrap().clone();
    assert_eq!(jobs.len(), 3);
    for (i, job) in jobs.iter().enumerate() {
        assert_eq!(job["name"], "test");
        assert_eq!(job["dir"], "test");
        assert_eq!(job["partition"], json!({"index": i + 1, "count": 3}));
        assert_eq!(job["executed"], true);
        assert_eq!(job["status"], if i == 1 { "failed" } else { "passed" });
        assert_eq!(job["collection"]["reports"].as_array().unwrap().len(), 1);
    }
    let mut requests: Vec<_> = mock.requests().iter().map(|r| r.json()).collect();
    requests.sort_by_key(|b| b["report"]["partition"].as_str().unwrap().to_owned());
    let seen: Vec<_> = requests
        .iter()
        .map(|b| {
            (
                b["report"]["partition"].clone(),
                b["report"]["partition_count"].clone(),
                b["job_status"].clone(),
                b["report"]["records"][0]["partition"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (json!("1"), json!(3), json!("passed"), json!("1")),
            (json!("2"), json!(3), json!("failed"), json!("2")),
            (json!("3"), json!(3), json!("passed"), json!("3")),
        ]
    );
    assert!(requests.iter().all(|b| b["job"] == "test"));
    // Acknowledgements land in each instance's own acks/ directory.
    for i in 1..=3 {
        assert!(dir.join(format!("test/p{i}/acks/000.json")).exists());
    }
}

#[test]
fn skipped_instances_are_recorded_as_not_executed() {
    let p = project(
        "[settings]\nparallelism = 1\n[[jobs]]\nname = \"test\"\npartitions = 3\ncondition = { expression = \"env.GOODFLOWS_PARTITION != '2'\" }\ncommand = 'cp fixtures/r$GOODFLOWS_PARTITION.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n",
    );
    let output = p.run(&["--json"]);
    success(&output);
    let dir = run_dirs(&p).pop().unwrap();
    let jobs = manifest(&dir)["jobs"].as_array().unwrap().clone();
    let executed: Vec<_> = jobs.iter().map(|j| j["executed"].clone()).collect();
    assert_eq!(executed, [json!(true), json!(false), json!(true)]);
    assert_eq!(jobs[1]["status"], "skipped");
    assert_eq!(jobs[1]["collection"]["outcome"], "not_executed");
}

#[test]
fn partitioned_reporting_events_identify_each_instance() {
    let mock = Mock::store();
    let p = partitioned("");
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let events = events(&output);
    for kind in ["reports_collected", "report_uploaded"] {
        let mut partitions: Vec<_> = by_type(&events, kind)
            .iter()
            .map(|e| {
                assert_eq!(e["job"], "test");
                e["partition"].clone()
            })
            .collect();
        partitions.sort_by_key(|p| p["index"].as_u64());
        assert_eq!(
            partitions,
            (1..=3)
                .map(|i| json!({"index": i, "count": 3}))
                .collect::<Vec<_>>(),
            "{kind}"
        );
    }
    let collected = by_type(&events, "reports_collected")
        .into_iter()
        .find(|e| e["partition"]["index"] == 2)
        .unwrap();
    assert_eq!(collected["report_ids"], json!(["r2"]));

    // Pending events carry the partition too, and text names the instance.
    let p = partitioned("");
    let conflict = Mock::start(|_, _| Reply::Status(409, vec![], String::new()));
    let output = run_with(&p, &["--json"], Some(&conflict.url));
    let pending = by_type(&events_of(&output), "report_upload_pending");
    assert_eq!(pending.len(), 3);
    assert!(pending.iter().all(|e| e["partition"]["count"] == 3));
    let p = partitioned("");
    let output = run_with(&p, &[], Some(&mock.url));
    success(&output);
    let text = stdout(&output);
    for (i, id) in [(1, "r3"), (2, "r2"), (3, "r1")] {
        assert!(
            text.contains(&format!("reports test ({i}/3): 1 collected (collected)")),
            "{text}"
        );
        assert!(
            text.contains(&format!("report test ({i}/3)/{id} uploaded")),
            "{text}"
        );
    }
    let text = stdout(&run_with(&partitioned(""), &[], Some(&conflict.url)));
    assert!(
        text.contains("report test (2/3)/r2 pending: conflict (HTTP 409)"),
        "{text}"
    );
}

#[test]
fn duplicate_report_ids_across_instances_reject_every_copy() {
    let mock = Mock::store();
    // Instances 1 and 3 both produce r1; instance 2 produces r2.
    let p = project(
        "[[jobs]]\nname = \"test\"\npartitions = 3\ncommand = 'if [ $GOODFLOWS_PARTITION = 2 ]; then id=r2; else id=r1; fi; cp fixtures/$id.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n",
    );
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    let events = events(&output);
    let collected = by_type(&events, "reports_collected");
    for event in &collected {
        if event["partition"]["index"] == 2 {
            assert_eq!(event["report_ids"], json!(["r2"]));
            assert_eq!(event["rejected"], json!({}));
        } else {
            assert_eq!(event["report_ids"], json!([]));
            assert_eq!(event["rejected"], json!({"duplicate_report_id": 1}));
        }
    }
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].json()["report"]["report_id"], "r2");
    let dir = run_dirs(&p).pop().unwrap();
    for i in [1, 3] {
        let base = dir.join(format!("test/p{i}"));
        assert_eq!(dir_entries(&base.join("pending")), 0);
        assert_eq!(
            std::fs::read_to_string(base.join("reports/r1.jsonl")).unwrap(),
            timing_report("r1", Some("1"))
        );
    }
    let jobs = manifest(&dir)["jobs"].clone();
    assert_eq!(jobs[0]["collection"]["reports"], json!([]));
    assert_eq!(jobs[2]["collection"]["reports"], json!([]));
}

#[test]
fn instance_directory_replaced_by_a_symlink_is_refused() {
    let p = project(
        "[[jobs]]\nname = \"test\"\npartitions = 2\ncommand = 'cp fixtures/r$GOODFLOWS_PARTITION.jsonl \"$GOODFLOWS_REPORT_DIR/\"; if [ $GOODFLOWS_PARTITION = 2 ]; then d=$(dirname \"$GOODFLOWS_REPORT_DIR\"); mv \"$d\" \"$PWD/moved\"; ln -s \"$PWD/moved\" \"$d\"; fi'\nreports = [\"exunit-timings\"]\n",
    );
    let output = p.run(&["--json"]);
    success(&output);
    let collected = by_type(&events(&output), "reports_collected");
    let second = collected
        .iter()
        .find(|e| e["partition"]["index"] == 2)
        .unwrap();
    assert_eq!(second["outcome"], "missing_directory");
    assert_eq!(second["rejected"], json!({"not_directory": 1}));
    let first = collected
        .iter()
        .find(|e| e["partition"]["index"] == 1)
        .unwrap();
    assert_eq!(first["report_ids"], json!(["r1"]));
    assert_eq!(dir_entries(&p.dir.join("moved/pending")), 0);
    assert!(p.dir.join("moved/reports/r2.jsonl").exists());
}

#[test]
fn partitioned_instances_share_the_job_report_cap() {
    let p = project(
        "[[jobs]]\nname = \"test\"\npartitions = 2\ncommand = 'cp fixtures/p$GOODFLOWS_PARTITION/*.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n",
    );
    for i in 1..=2 {
        let dir = p.dir.join(format!("fixtures/p{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..130 {
            let id = format!("p{i}-r{n:03}");
            std::fs::write(dir.join(format!("{id}.jsonl")), timing_report(&id, None)).unwrap();
        }
    }
    let output = p.run(&["--json"]);
    success(&output);
    let collected = by_type(&events(&output), "reports_collected");
    let instance = |index: u64| {
        collected
            .iter()
            .find(|e| e["partition"]["index"] == index)
            .unwrap()
    };
    // The 256-report cap is consumed in partition order, so instance 2 loses reports.
    assert_eq!(instance(1)["reports"], 130);
    assert_eq!(instance(1)["rejected"], json!({}));
    assert_eq!(instance(2)["reports"], 126);
    assert_eq!(instance(2)["rejected"], json!({"report_limit": 4}));
    let dir = run_dirs(&p).pop().unwrap();
    let listed: usize = manifest(&dir)["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["collection"]["reports"].as_array().unwrap().len())
        .sum();
    assert_eq!(listed, 256);
    assert_eq!(dir_entries(&dir.join("test/p2/pending")), 126);
}

#[test]
fn partitioned_steps_receive_their_instance_report_dir() {
    // Each step records its report directory; each step condition records
    // whether it could see one (it must not: conditions are not producers).
    let p = project(
        r#"
[[jobs]]
name = "test"
partitions = 2
reports = ["exunit-timings"]
[[jobs.steps]]
id = "a"
condition = { shell = 'printf "%s" "${GOODFLOWS_REPORT_DIR-unset}" > cond-a-$GOODFLOWS_PARTITION' }
command = 'printf "%s" "$GOODFLOWS_REPORT_DIR" > step-a-$GOODFLOWS_PARTITION'
[[jobs.steps]]
id = "b"
condition = { shell = 'printf "%s" "${GOODFLOWS_REPORT_DIR-unset}" > cond-b-$GOODFLOWS_PARTITION' }
command = 'printf "%s" "$GOODFLOWS_REPORT_DIR" > step-b-$GOODFLOWS_PARTITION && cp fixtures/r$GOODFLOWS_PARTITION.jsonl "$GOODFLOWS_REPORT_DIR/"'
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    let dir = run_dirs(&p).pop().unwrap().canonicalize().unwrap();
    for i in 1..=2 {
        for step in ["a", "b"] {
            let seen = p.read(&format!("step-{step}-{i}"));
            assert_eq!(
                Path::new(&seen).canonicalize().unwrap(),
                dir.join(format!("test/p{i}/reports")),
                "step {step} of instance {i}"
            );
            assert_eq!(p.read(&format!("cond-{step}-{i}")), "unset", "{step} {i}");
        }
    }
    let collected = by_type(&events(&output), "reports_collected");
    for i in 1..=2u64 {
        let event = collected
            .iter()
            .find(|e| e["partition"]["index"] == i)
            .unwrap();
        assert_eq!(event["report_ids"], json!([format!("r{i}")]));
    }
}

/// A finalized 3-partition run saved without credentials.
fn saved_partitioned_run() -> (Project, PathBuf) {
    let p = project_with(
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"SAVED_URL\"\ntoken_env = \"SAVED_KEY\"\n",
        "[[jobs]]\nname = \"test\"\npartitions = 3\ncommand = 'cp fixtures/r$GOODFLOWS_PARTITION.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n",
    );
    success(&p.run(&["--json"]));
    let dir = run_dirs(&p).pop().unwrap();
    (p, dir)
}

#[test]
fn replay_sends_each_instance_with_its_partition() {
    let (p, dir) = saved_partitioned_run();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let mut partitions: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| {
            let body = r.json();
            assert_eq!(body["report"]["partition_count"], 3);
            assert_eq!(
                body["report"]["records"][0]["partition"],
                body["report"]["partition"]
            );
            body["report"]["partition"].as_str().unwrap().to_owned()
        })
        .collect();
    partitions.sort();
    assert_eq!(partitions, ["1", "2", "3"]);
    let events = events(&output);
    let uploaded = by_type(&events, "report_uploaded");
    assert_eq!(uploaded.len(), 3);
    assert!(uploaded.iter().all(|e| e["partition"]["count"] == 3));
    assert_eq!(events.last().unwrap()["acknowledged"], 3);
    for i in 1..=3 {
        assert!(dir.join(format!("test/p{i}/acks/000.json")).exists());
    }
    // A second replay finds every instance acknowledged.
    success(&replay(&p.dir, &dir, Some(&mock.url), &["--json"]));
    assert_eq!(mock.requests().len(), 3);
}

#[test]
fn replay_refuses_an_instance_snapshot_edited_to_another_partition() {
    let (p, dir) = saved_partitioned_run();
    // Consistently rewritten, so only the manifest identity can catch it.
    let path = dir.join("test/p2/pending/000.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["report"]["partition"] = json!("3");
    value["report"]["records"][0]["partition"] = json!("3");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["reason"], "invalid_snapshot");
    assert_eq!(pending[0]["partition"], json!({"index": 2, "count": 3}));
    assert_eq!(events.last().unwrap()["invalid"], 1);
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn replay_refuses_inconsistent_partition_entries() {
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|m| m["jobs"][1]["partition"]["index"] = json!(1)),
            "duplicate partition index",
        ),
        (
            Box::new(|m| m["jobs"][2]["partition"]["index"] = json!(4)),
            "partition index must be from 1 to its count",
        ),
        (
            Box::new(|m| m["jobs"][0]["partition"]["index"] = json!(0)),
            "partition index must be from 1 to its count",
        ),
        (
            Box::new(|m| {
                for job in m["jobs"].as_array_mut().unwrap() {
                    job["partition"]["count"] = json!(65);
                }
            }),
            "partition count must be from 1 to 64",
        ),
        (
            Box::new(|m| m["jobs"][1]["partition"]["count"] = json!(4)),
            "disagree on the partition count",
        ),
        (
            Box::new(|m| {
                m["jobs"][1].as_object_mut().unwrap().remove("partition");
            }),
            "both partitioned and unpartitioned entries",
        ),
        (
            Box::new(|m| {
                m["jobs"][0].as_object_mut().unwrap().remove("partition");
            }),
            "both partitioned and unpartitioned entries",
        ),
        (
            Box::new(|m| m["jobs"][0]["partition"]["count"] = json!(0)),
            "partition count must be from 1 to 64",
        ),
        (
            Box::new(|m| m["jobs"][0]["partition"]["extra"] = json!(1)),
            "invalid manifest",
        ),
        (
            Box::new(|m| m["version"] = json!(1)),
            "manifest version 1 requires no partition entries",
        ),
        (
            Box::new(|m| m["version"] = json!(3)),
            "unsupported manifest version 3",
        ),
    ];
    let mock = Mock::store();
    for (edit, message) in cases {
        let (p, dir) = saved_partitioned_run();
        edit_manifest(&dir, edit);
        let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
        assert_eq!(output.status.code(), Some(1), "{message}");
        let event = events(&output).pop().unwrap();
        assert_eq!(event["type"], "error");
        let text = event["message"].as_str().unwrap();
        assert!(text.contains(message), "{message}: {text}");
    }
    assert_eq!(mock.connections(), 0);
}

#[test]
fn replay_accepts_reordered_and_incomplete_instance_entries() {
    // Entries listed 3, 1, 2: order is not part of the manifest's consistency.
    let (p, dir) = saved_partitioned_run();
    assert_eq!(manifest(&dir)["version"], 2);
    edit_manifest(&dir, |m| {
        let jobs = m["jobs"].as_array_mut().unwrap();
        jobs.rotate_right(1);
    });
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let mut sent: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| {
            let report = &r.json()["report"];
            (
                report["partition"].as_str().unwrap().to_owned(),
                report["report_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    sent.sort();
    assert_eq!(
        sent,
        [("1", "r1"), ("2", "r2"), ("3", "r3")].map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(events(&output).last().unwrap()["acknowledged"], 3);

    // A manifest missing an instance replays the instances it lists.
    let (p, dir) = saved_partitioned_run();
    edit_manifest(&dir, |m| {
        m["jobs"].as_array_mut().unwrap().remove(1);
    });
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let mut sent: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| r.json()["report"]["report_id"].as_str().unwrap().to_owned())
        .collect();
    sent.sort();
    assert_eq!(sent, ["r1", "r3"]);
}

#[test]
fn replay_counts_a_leftover_duplicate_snapshot_once_and_never_sends_it() {
    let (p, dir) = saved_partitioned_run();
    // As if instance 2's duplicate could not be discarded: its snapshot stays
    // in pending/ but is not listed.
    assert!(dir.join("test/p2/pending/000.json").exists());
    edit_manifest(&dir, |m| {
        let collection = &mut m["jobs"][1]["collection"];
        collection["reports"] = json!([]);
        collection["rejected"] = json!({"duplicate_report_id": 1, "snapshot_discard_failed": 1});
    });
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "reports_upload_summary");
    assert_eq!(summary["invalid"], 1);
    assert_eq!(summary["acknowledged"], 2);
    let mut sent: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| r.json()["report"]["report_id"].as_str().unwrap().to_owned())
        .collect();
    sent.sort();
    assert_eq!(sent, ["r1", "r3"]);
    assert!(by_type(&events, "report_upload_pending").is_empty());
    assert!(!dir.join("test/p2/acks/000.json").exists());
    assert!(dir.join("test/p2/pending/000.json").exists());
}

#[test]
fn replay_checks_report_ids_across_a_jobs_instances() {
    let (p, dir) = saved_partitioned_run();
    // Instance 3's manifest entry claims instance 1's report id.
    edit_manifest(&dir, |m| {
        m["jobs"][2]["collection"]["reports"][0]["report_id"] = json!("r1")
    });
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    // Like collection, replay refuses every copy, not only the later one.
    let pending = by_type(&events, "report_upload_pending");
    assert_eq!(pending.len(), 2);
    for (event, index) in pending.iter().zip([1, 3]) {
        assert_eq!(event["reason"], "invalid_snapshot");
        assert_eq!(event["report_id"], "r1");
        assert_eq!(event["partition"]["index"], index);
    }
    assert_eq!(events.last().unwrap()["invalid"], 2);
    assert_eq!(events.last().unwrap()["acknowledged"], 1);
    let sent: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| r.json()["report"]["report_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sent, ["r2"]);
    for i in [1, 3] {
        assert!(!dir.join(format!("test/p{i}/acks/000.json")).exists());
    }

    // The same id listed twice within one unpartitioned entry refuses both too.
    let (p, dir) = saved_run(None);
    edit_manifest(&dir, |m| {
        let reports = &mut m["jobs"][0]["collection"]["reports"];
        reports[1]["report_id"] = reports[0]["report_id"].clone();
    });
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let replayed = events_of(&output);
    assert_eq!(by_type(&replayed, "report_upload_pending").len(), 2);
    assert_eq!(replayed.last().unwrap()["invalid"], 2);
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn replay_of_a_manifest_written_before_partitions_is_unchanged() {
    let (p, dir) = saved_run(None);
    // The older format: job entries without any partition key.
    let saved = manifest(&dir);
    assert_eq!(saved["version"], 1);
    for job in saved["jobs"].as_array().unwrap() {
        assert!(job.get("partition").is_none());
    }
    let before: Vec<_> = (0..2)
        .map(|i| std::fs::read(dir.join(format!("test/pending/{i:03}.json"))).unwrap())
        .collect();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for (request, bytes) in requests.iter().zip(&before) {
        assert_eq!(&request.body, bytes);
    }
    for event in by_type(&events(&output), "report_uploaded") {
        assert!(event.get("partition").is_none(), "{event}");
    }
    assert_eq!(manifest(&dir), saved);
}

// ---- settings env ----

#[test]
fn settings_env_never_reveals_inherited_token_and_keeps_reporting_identity() {
    let p = project(&format!(
        r#"
[settings.env]
SHARED = "settings"
[environments.local.settings.env]
PROFILE = "profile"
[[jobs]]
name = "shell"
condition = {{ expression = 'env.GOODFLOWS_API_KEY == null && env.SHARED == "settings"' }}
command = 'env > shell-env.txt; {copy}'
reports = ["exunit-timings"]
[[jobs]]
name = "stepped"
outputs = {{ token = "env.GOODFLOWS_API_KEY ?? \"absent\"" }}
[[jobs.steps]]
id = "probe"
condition = {{ shell = "env > step-condition-env.txt" }}
command = 'env > step-env.txt'
"#,
        copy = copy(&["r1"])
    ));
    let output = p
        .command()
        .args(["--env", "local", "--json"])
        .env("GOODFLOWS_URL", "http://flows.example.com")
        .env("GOODFLOWS_API_KEY", SECRET)
        .env("GOODFLOWS_RUN_ID", "inherited-run-id")
        .output()
        .unwrap();
    success(&output);
    for file in ["shell-env.txt", "step-condition-env.txt", "step-env.txt"] {
        let env = p.read(file);
        assert!(env.contains("SHARED=settings"), "{file}: {env}");
        assert!(env.contains("PROFILE=profile"), "{file}: {env}");
        assert!(!env.contains("GOODFLOWS_API_KEY="), "{file}: {env}");
        assert!(!env.contains("inherited"), "{file}: {env}");
    }
    let run_id = manifest(&run_dirs(&p)[0])["workflow_run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(p
        .read("shell-env.txt")
        .contains(&format!("GOODFLOWS_RUN_ID={run_id}")));
    let summary = events(&output).last().unwrap().clone();
    let stepped = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "stepped")
        .unwrap()
        .clone();
    assert_eq!(stepped["outputs"]["token"], "absent");
    assert_no_secret(&p, &output);
}

#[test]
fn settings_env_never_supplies_the_publisher_endpoint() {
    let mock = Mock::store();
    let decoy = Mock::store();
    let jobs = |url: &str| {
        format!(
            "[settings.env]\nGOODFLOWS_URL = \"{url}\"\n[[jobs]]\nname = \"test\"\ncommand = 'printf %s \"$GOODFLOWS_URL\" > seen-url; {}'\nreports = [\"exunit-timings\"]\n",
            copy(&["r1"])
        )
    };
    // The command sees the settings value while the publisher uses the caller's endpoint.
    let p = project(&jobs(&decoy.url));
    let output = run_with(&p, &["--json"], Some(&mock.url));
    success(&output);
    assert_eq!(p.read("seen-url"), decoy.url);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(decoy.connections(), 0);
    assert_no_secret(&p, &output);
    // Without a caller endpoint, settings do not make one available to the publisher.
    let p = project(&jobs(&decoy.url));
    let output = p
        .command()
        .arg("--json")
        .env("GOODFLOWS_API_KEY", SECRET)
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("seen-url"), decoy.url);
    let skipped = by_type(&events(&output), "reporting_skipped");
    assert_eq!(skipped[0]["reason"], "credentials_unavailable");
    assert_eq!(decoy.connections(), 0);
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn settings_env_never_supplies_a_custom_url_env_endpoint() {
    let mock = Mock::store();
    let decoy = Mock::store();
    let p = project_with(
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"CUSTOM_URL\"\n",
        &format!(
            "[settings.env]\nCUSTOM_URL = \"{}\"\n[[jobs]]\nname = \"test\"\ncommand = 'printf %s \"$CUSTOM_URL\" > seen-url; {}'\nreports = [\"exunit-timings\"]\n",
            decoy.url,
            copy(&["r1"])
        ),
    );
    let output = p
        .command()
        .arg("--json")
        .env("CUSTOM_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", SECRET)
        .output()
        .unwrap();
    success(&output);
    // Commands see the settings value; the publisher uses the caller's endpoint.
    assert_eq!(p.read("seen-url"), decoy.url);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(decoy.connections(), 0);
    assert_no_secret(&p, &output);
}

// ---- profile partition counts ----

/// Declared count 3; the selected `local` profile raises it to 6. Saved without credentials.
fn saved_profile_count_run() -> (Project, PathBuf, std::process::Output) {
    let p = project_with(
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"SAVED_URL\"\ntoken_env = \"SAVED_KEY\"\n",
        "[[jobs]]\nname = \"test\"\npartitions = 3\ncommand = 'cp fixtures/q$GOODFLOWS_PARTITION.jsonl \"$GOODFLOWS_REPORT_DIR/\"'\nreports = [\"exunit-timings\"]\n[environments.local.partitions]\ntest = 6\n",
    );
    for i in 1..=6 {
        std::fs::write(
            p.dir.join(format!("fixtures/q{i}.jsonl")),
            timing_report(&format!("q{i}"), Some(&i.to_string())),
        )
        .unwrap();
    }
    let output = p.run(&["--json", "--env", "local"]);
    success(&output);
    let dir = run_dirs(&p).pop().unwrap();
    (p, dir, output)
}

#[test]
fn profile_partition_count_drives_report_collection_and_replay() {
    let (p, dir, output) = saved_profile_count_run();
    let events = events(&output);
    assert_eq!(events[0]["jobs"], 6);
    let collected = by_type(&events, "reports_collected");
    assert_eq!(collected.len(), 6);
    let mut indexes: Vec<_> = collected
        .iter()
        .map(|e| {
            assert_eq!(e["partition"]["count"], 6, "{e}");
            assert_eq!(e["reports"], 1, "{e}");
            e["partition"]["index"].as_u64().unwrap()
        })
        .collect();
    indexes.sort();
    assert_eq!(indexes, [1, 2, 3, 4, 5, 6]);
    let summary = events.last().unwrap();
    assert_eq!(summary["passed"], 6);
    assert_eq!(summary["results"].as_array().unwrap().len(), 6);

    let jobs = manifest(&dir)["jobs"].as_array().unwrap().clone();
    assert_eq!(jobs.len(), 6);
    for (i, job) in jobs.iter().enumerate() {
        assert_eq!(job["partition"], json!({"index": i + 1, "count": 6}));
        assert_eq!(job["status"], "passed");
        // The saved report envelope carries the effective count.
        let snapshot: Value = serde_json::from_slice(
            &std::fs::read(dir.join(format!("test/p{}/pending/000.json", i + 1))).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot["report"]["partition"], (i + 1).to_string());
        assert_eq!(snapshot["report"]["partition_count"], 6);
    }

    // Replay reads the saved run only: change the config so a fresh resolution
    // would give a different count, then check the original count is sent.
    std::fs::write(
        p.dir.join("goodflows.toml"),
        "[reporting]\nproject = \"goodcases\"\nurl_env = \"SAVED_URL\"\ntoken_env = \"SAVED_KEY\"\n[[jobs]]\nname = \"test\"\npartitions = 2\ncommand = 'true'\n[environments.local.partitions]\ntest = 1\n",
    )
    .unwrap();
    let mock = Mock::store();
    let output = replay(&p.dir, &dir, Some(&mock.url), &["--json"]);
    success(&output);
    let mut partitions: Vec<_> = mock
        .requests()
        .iter()
        .map(|r| {
            let body = r.json();
            assert_eq!(body["environment"], "local");
            assert_eq!(body["report"]["partition_count"], 6);
            assert_eq!(body["report"]["records"][0]["partition_count"], 6);
            body["report"]["partition"].as_str().unwrap().to_owned()
        })
        .collect();
    partitions.sort();
    assert_eq!(partitions, ["1", "2", "3", "4", "5", "6"]);
    let uploaded = by_type(&events_of(&output), "report_uploaded");
    assert_eq!(uploaded.len(), 6);
    assert!(uploaded.iter().all(|e| e["partition"]["count"] == 6));
}
