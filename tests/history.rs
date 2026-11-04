mod common;
use common::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn summary_of(output: &std::process::Output) -> Value {
    let summary: Value = serde_json::from_str(stdout(output).lines().last().unwrap()).unwrap();
    assert_eq!(summary["type"], "summary", "{}", stdout(output));
    summary
}

/// History files in a workflow directory, oldest first.
fn history_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .is_some_and(is_stem)
        })
        .collect();
    files.sort();
    files
}

fn record(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap();
    // One compact line with a single trailing LF.
    assert!(
        text.ends_with('\n') && !text[..text.len() - 1].contains('\n'),
        "{text}"
    );
    assert!(!text.contains("\r"));
    serde_json::from_str(&text).unwrap()
}

const CI: &str = "[[workflows.ci.jobs]]\nname='build'\ncommand='true'\n[[workflows.ci.jobs]]\nname='test'\ndepends_on=['build']\npartitions=2\ncommand='true'\n";

#[test]
fn default_history_file_records_the_run() {
    let p = Project::new(CI);
    let output = p.run(&["run", "ci", "--json", "--history", "--log-dir", "logs"]);
    success(&output);
    let summary = summary_of(&output);
    let files = history_files(&p.dir.join("goodflows-history/ci"));
    assert_eq!(files.len(), 1);
    let path = &files[0];
    assert_eq!(summary["history_file"], path.to_str().unwrap());
    let record = record(path);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    // The key set is the allowlist; `source` appears only inside a git
    // checkout, and `environment` and `requested_jobs` only when given.
    let mut keys: Vec<_> = record
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| *k != "source")
        .cloned()
        .collect();
    keys.sort();
    let mut expected = vec![
        "schema_version",
        "workflow",
        "run_id",
        "started_at",
        "outcome",
        "duration_ms",
        "parallelism",
        "graph_sha256",
        "max_in_flight",
        "host",
        "results",
    ];
    expected.sort();
    assert_eq!(keys, expected);
    assert_eq!(record["schema_version"], 1);
    assert_eq!(record["workflow"], "ci");
    assert_eq!(record["outcome"], "passed");
    for field in [
        "run_id",
        "started_at",
        "outcome",
        "parallelism",
        "graph_sha256",
        "max_in_flight",
        "host",
    ] {
        assert_eq!(record[field], summary[field], "{field}");
    }
    // History duration covers the jobs only; the summary's also covers
    // reporting after them.
    assert_eq!(record["duration_ms"], summary["execute_ms"]);
    assert!(summary["duration_ms"].as_u64() >= summary["execute_ms"].as_u64());
    assert_eq!(record["host"]["os"], std::env::consts::OS);
    assert_eq!(record["host"]["arch"], std::env::consts::ARCH);
    assert!(record.get("history_file").is_none());
    // The stem is the start second and the run id; logs use the same stem.
    let started = record["started_at"].as_str().unwrap();
    let compact: String = started
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .collect();
    assert_eq!(
        stem,
        format!("{compact}-{}", record["run_id"].as_str().unwrap())
    );
    assert_eq!(summary["log_dir"], format!("logs/ci/{stem}"));
    assert!(record["host"]["logical_cpus"].as_u64().unwrap() >= 1);
    let results = record["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results[1]["partition"],
        serde_json::json!({"index": 1, "count": 2})
    );
    assert!(results[0].get("partition").is_none());
    for result in results {
        let mut keys: Vec<_> = result.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let mut expected: Vec<&str> = [
            "name",
            "partition",
            "status",
            "started",
            "exit_code",
            "command",
            "error",
            "blocked_ms",
            "queued_ms",
            "duration_ms",
            "commands",
        ]
        .into_iter()
        .filter(|k| *k != "partition" || result.get("partition").is_some())
        .collect();
        expected.sort();
        assert_eq!(keys, expected, "{result}");
        assert_eq!(result["commands"][0]["kind"], "command");
    }
    // Text mode shows the history file after the log directory.
    let output = p.run(&["run", "ci", "--history", "--log-dir", "logs"]);
    success(&output);
    let text = stdout(&output);
    let files = history_files(&p.dir.join("goodflows-history/ci"));
    let newest = files.iter().rev().find(|f| f != &path).unwrap();
    let logs = text.find("\nLogs: logs/ci/").expect("logs line");
    let history = text
        .find(&format!("\nHistory: {}\n", newest.display()))
        .expect("history line");
    assert!(logs < history, "{text}");
    // Without --log-dir the history line is the only path; without history, none.
    let text = stdout(&p.run(&["run", "ci", "--history"]));
    assert!(text.contains("\n\nHistory: "), "{text}");
    assert!(!text.contains("Logs:"), "{text}");
    let output = p.run(&["run", "ci", "--json"]);
    let later = summary_of(&output);
    assert!(later.get("history_file").is_none());
    // The same selection hashes the same on every run; another selection differs.
    assert_eq!(later["graph_sha256"], record["graph_sha256"]);
    let only_build = summary_of(&p.run(&["run", "ci", "--json", "--jobs", "build"]));
    assert_ne!(only_build["graph_sha256"], record["graph_sha256"]);
    assert!(!stdout(&p.run(&["run", "ci"])).contains("History:"));
}

#[test]
fn history_is_enabled_and_disabled_by_settings_and_flags() {
    let p = Project::new(
        "[settings]\nhistory = true\n[[jobs]]\nname='a'\ncommand='true'\n\
         [environments.quiet.settings]\nhistory = false\n",
    );
    let dir = p.dir.join("goodflows-history/default");
    success(&p.run(&["--json"]));
    // Legacy top-level jobs record under `default`.
    assert_eq!(history_files(&dir).len(), 1);
    success(&p.run(&["--env", "quiet"]));
    success(&p.run(&["--no-history"]));
    assert_eq!(history_files(&dir).len(), 1);
    success(&p.run(&["--env", "quiet", "--history"]));
    assert_eq!(history_files(&dir).len(), 2);
}

#[test]
fn history_directories_resolve_from_config_and_invocation() {
    let p = Project::new(
        "[settings]\nhistory_dir = 'shared'\n[[jobs]]\nname='a'\ncommand='true'\n\
         [environments.ci.settings]\nhistory_dir = 'profile'\n",
    );
    let sub = p.dir.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let run = |args: &[&str]| {
        let output = p
            .command()
            .current_dir(&sub)
            .args(["--config", "../goodflows.toml", "--json"])
            .args(args)
            .output()
            .unwrap();
        success(&output);
        summary_of(&output)
    };
    // Config directories resolve from the config file, not the invocation.
    run(&["--history", "--env", "ci"]);
    assert_eq!(history_files(&p.dir.join("profile/default")).len(), 1);
    run(&["--history"]);
    assert_eq!(history_files(&p.dir.join("shared/default")).len(), 1);
    // --history-dir enables history and resolves from the invocation directory.
    let summary = run(&["--history-dir", "runs"]);
    let files = history_files(&sub.join("runs/default"));
    assert_eq!(files.len(), 1);
    assert_eq!(summary["history_file"], files[0].to_str().unwrap());
    assert!(!sub.join("goodflows-history").exists());
}

#[test]
fn retention_caps_keep_the_newest_files_per_workflow() {
    let p = Project::new(&format!(
        "{CI}[[workflows.other.jobs]]\nname='a'\ncommand='true'\n"
    ));
    let ci = p.dir.join("goodflows-history/ci");
    let other = p.dir.join("goodflows-history/other");
    success(&p.run(&["run", "other", "--history"]));
    let other_before = std::fs::read(&history_files(&other)[0]).unwrap();
    success(&p.run(&["run", "ci", "--history"]));
    let first = history_files(&ci)[0].clone();
    let first_bytes = std::fs::read(&first).unwrap();
    std::fs::write(ci.join("notes.txt"), "keep").unwrap();
    // A second run leaves the first file's bytes untouched.
    success(&p.run(&["run", "ci", "--history"]));
    assert_eq!(history_files(&ci).len(), 2);
    assert_eq!(std::fs::read(&first).unwrap(), first_bytes);
    // A count cap of 2 keeps two files, always including the new one. (Runs
    // in the same second order by their random run id.)
    let run = |args: &[&str]| {
        let output = p.run(&[&["run", "ci", "--json", "--history"], args].concat());
        success(&output);
        PathBuf::from(summary_of(&output)["history_file"].as_str().unwrap())
    };
    let newest = run(&["--history-runs", "2"]);
    let files = history_files(&ci);
    assert_eq!(files.len(), 2);
    assert!(files.contains(&newest));
    let files = &[newest];
    // A byte cap smaller than two records keeps only the newest.
    let size = std::fs::metadata(&files[0]).unwrap().len();
    let cap = (size + size / 2).to_string();
    success(&p.run(&["run", "ci", "--history", "--history-bytes", &cap]));
    assert_eq!(history_files(&ci).len(), 1);
    // A 1-byte cap keeps the oversized newest file and deletes every older one.
    success(&p.run(&["run", "ci", "--history"]));
    assert_eq!(history_files(&ci).len(), 2);
    let output = p.run(&["run", "ci", "--json", "--history", "--history-bytes", "1"]);
    success(&output);
    let files = history_files(&ci);
    assert_eq!(files.len(), 1);
    assert_eq!(
        summary_of(&output)["history_file"],
        files[0].to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(ci.join("notes.txt")).unwrap(),
        "keep"
    );
    // The other workflow is untouched.
    let others = history_files(&other);
    assert_eq!(others.len(), 1);
    assert_eq!(std::fs::read(&others[0]).unwrap(), other_before);
    for flag in ["--history-runs", "--history-bytes"] {
        assert_eq!(p.run(&["run", "ci", flag, "0"]).status.code(), Some(2));
    }
}

#[test]
fn history_omits_output_secrets_and_paths() {
    let p = Project::new(
        "[settings]\nfail_fast = false\n\
         [settings.env]\nS1 = 'stdout-secret-1'\nS2 = 'stderr-secret-2'\nS3 = 'env-secret-3'\nS4 = 'output-secret-4'\n\
         [[jobs]]\nname='print'\ncommand='echo \"$S1\"'\n\
         [[jobs]]\nname='publish'\ncommand='printf \"{\\\"k\\\":\\\"%s\\\"}\" \"$S4\" > \"$GOODFLOWS_OUTPUT\"'\n\
         [[jobs]]\nname='fail'\ncommand='echo \"$S2\" >&2; exit 3'\n",
    );
    let output = p.run(&["--json", "--history", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    // The secrets reached the summary, so their absence below is meaningful.
    let summary_text = summary.to_string();
    for secret in ["stdout-secret-1", "stderr-secret-2", "output-secret-4"] {
        assert!(stdout(&output).contains(secret), "{secret}");
    }
    assert!(summary_text.contains("stderr-secret-2"));
    let path = summary["history_file"].as_str().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    for absent in [
        "stdout-secret-1",
        "stderr-secret-2",
        "env-secret-3",
        "output-secret-4",
        ".log",
        "stdout_tail",
        "outputs",
        p.dir.to_str().unwrap(),
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    let record = record(Path::new(path));
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["results"][2]["exit_code"], 3);
    assert_eq!(record["results"][2]["error"], "exited with 3");
}

#[test]
fn long_commands_and_timeouts_are_recorded() {
    let long = format!("true {}é; exit 3", "x".repeat(1100));
    let p = Project::new(&format!(
        "[settings]\nfail_fast = false\n[[jobs]]\nname='long'\ncommand='{long}'\n\
         [[jobs]]\nname='slow'\ntimeout_secs=1\ncommand='sleep 5'\n"
    ));
    let output = p.run(&["--json", "--history"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    // The summary keeps the full command.
    assert_eq!(summary["results"][0]["command"], long);
    let record = record(Path::new(summary["history_file"].as_str().unwrap()));
    assert_eq!(record["outcome"], "failed");
    let entry = &record["results"][0];
    assert_eq!(entry["exit_code"], 3);
    let command = entry["command"].as_str().unwrap();
    assert!(
        command.len() <= 1024 && long.starts_with(command),
        "{}",
        command.len()
    );
    assert_eq!(record["results"][1]["status"], "timed_out");
}

#[test]
fn failed_log_open_is_redacted_in_history() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // Permissions do not stop root.
    }
    let p = Project::new(
        "[settings]\nfail_fast = false\n[[jobs]]\nname='lock'\ncommand='chmod 500 logs/*/'\n\
         [[jobs]]\nname='b'\ndepends_on=['lock']\ncommand='true'\n",
    );
    let output = p.run(&["--json", "--history", "--log-dir", "logs"]);
    let run = run_dir(&p.dir.join("logs"));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    let raw = summary["results"][1]["error"].as_str().unwrap();
    assert!(
        raw.contains(run.file_name().unwrap().to_str().unwrap()),
        "{raw}"
    );
    let record = record(Path::new(summary["history_file"].as_str().unwrap()));
    let error = record["results"][1]["error"].as_str().unwrap();
    // The whole log path, basename included, is redacted.
    assert!(error.contains("<log>"), "{error}");
    assert!(!error.contains("<log>/"), "{error}");
    assert!(!error.contains(".stdout.log"), "{error}");
    assert!(!error.contains("002-b"), "{error}");
    assert!(!error.contains("logs/"), "{error}");
    let raw = std::fs::read_to_string(summary["history_file"].as_str().unwrap()).unwrap();
    assert!(!raw.contains(".log"), "{raw}");
}

#[test]
fn history_write_failures_keep_the_exit_code() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='true'\n");
    std::fs::write(p.dir.join("blocked"), "").unwrap();
    let output = p.run(&["--json", "--history-dir", "blocked/runs"]);
    success(&output);
    assert!(summary_of(&output).get("history_file").is_none());
    assert!(
        stderr(&output).contains("run history"),
        "{}",
        stderr(&output)
    );
    // A symlinked history or workflow directory is refused.
    let real = p.dir.join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, p.dir.join("linked")).unwrap();
    let output = p.run(&["--json", "--history-dir", "linked"]);
    success(&output);
    assert!(summary_of(&output).get("history_file").is_none());
    assert!(stderr(&output).contains("symlink"), "{}", stderr(&output));
    std::fs::create_dir(p.dir.join("wf")).unwrap();
    std::os::unix::fs::symlink(&real, p.dir.join("wf/default")).unwrap();
    let output = p.run(&["--json", "--history-dir", "wf"]);
    success(&output);
    assert!(summary_of(&output).get("history_file").is_none());
    assert!(std::fs::read_dir(&real).unwrap().next().is_none());
    // A failing job keeps exit code 1 when history cannot be written.
    let p = Project::new("[[jobs]]\nname='a'\ncommand='exit 1'\n");
    std::fs::write(p.dir.join("blocked"), "").unwrap();
    let output = p.run(&["--history-dir", "blocked/runs"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_run_that_errors_before_the_summary_writes_no_history() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='touch ran'\n");
    std::fs::write(p.dir.join("logs"), "not a directory").unwrap();
    let output = p.run(&["--json", "--history", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    assert!(events.iter().all(|e| e["type"] != "summary"));
    assert_eq!(events.last().unwrap()["type"], "error");
    assert!(!p.dir.join("ran").exists());
    assert!(!p.dir.join("goodflows-history").exists());
}

#[test]
fn a_cancelled_run_writes_a_cancelled_record() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let p = Project::new("[[jobs]]\nname='a'\ncommand='touch started; sleep 5'\n");
    let child = p
        .command()
        .args(["--json", "--history"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !p.dir.join("started").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "job did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // SAFETY: the child PID identifies the CLI spawned above.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let summary = summary_of(&output);
    assert_eq!(summary["outcome"], "cancelled");
    let record = record(Path::new(summary["history_file"].as_str().unwrap()));
    assert_eq!(record["outcome"], "cancelled");
    let cancelled = &record["results"][0];
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["started"], true);
    // The cancelled command was spawned, so it still has a cost entry.
    assert_eq!(cancelled["commands"][0]["kind"], "command");
    assert!(cancelled["commands"][0]["wall_ms"].is_u64(), "{cancelled}");
}

#[test]
fn non_run_commands_never_write_history() {
    let p = Project::new("[settings]\nhistory = true\n[[jobs]]\nname='a'\ncommand='true'\n");
    for args in [
        vec!["validate", "--history"],
        vec!["list", "--history"],
        vec!["list", "--workflows"],
        vec!["init", "--force", "--history"],
    ] {
        let output = p.run(&args);
        success(&output);
    }
    assert!(!p.dir.join("goodflows-history").exists());
    // Reports upload rejects every history and retention flag, naming it.
    std::fs::create_dir(p.dir.join("spool")).unwrap();
    for (flag, value) in [
        ("--history", None),
        ("--no-history", None),
        ("--history-dir", Some("x")),
        ("--log-runs", Some("1")),
        ("--history-runs", Some("1")),
        ("--history-bytes", Some("1")),
    ] {
        let mut args = vec!["reports", "upload", "spool", flag];
        args.extend(value);
        let output = p.run(&args);
        assert_eq!(output.status.code(), Some(1), "{flag}");
        assert!(
            stderr(&output).contains(&format!("{flag} cannot be used with reports upload")),
            "{flag}: {}",
            stderr(&output)
        );
    }
    assert!(!p.dir.join("goodflows-history").exists());
}

#[test]
fn history_records_the_run_context() {
    let p = Project::new(&format!(
        "{CI}[environments.fast.settings]\nparallelism = 2\n"
    ));
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(&p.dir)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let in_git = git(&["init", "-q"])
        && git(&["add", "goodflows.toml"])
        && git(&["commit", "-q", "-m", "init"]);
    let output = p.run(&[
        "run",
        "ci",
        "--json",
        "--history",
        "--env",
        "fast",
        "--jobs",
        "test",
    ]);
    success(&output);
    let first = record(Path::new(
        summary_of(&output)["history_file"].as_str().unwrap(),
    ));
    assert_eq!(first["environment"], "fast");
    assert_eq!(first["requested_jobs"], serde_json::json!(["test"]));
    if in_git {
        let commit = first["source"]["commit"].as_str().unwrap();
        assert_eq!(commit.len(), 40, "{first}");
        assert_eq!(first["source"]["dirty"], false);
        // Only commit and dirty: never the branch or remotes.
        assert_eq!(first["source"].as_object().unwrap().len(), 2);
        std::fs::write(p.dir.join("goodflows.toml"), format!("{CI}# edited\n")).unwrap();
        let output = p.run(&["run", "ci", "--json", "--history"]);
        let edited = record(Path::new(
            summary_of(&output)["history_file"].as_str().unwrap(),
        ));
        assert_eq!(edited["source"]["dirty"], true);
        assert!(edited.get("environment").is_none());
        assert!(edited.get("requested_jobs").is_none());
    }
    // Outside a checkout, `source` is omitted.
    let outside = Project::new(CI);
    let output = outside
        .command()
        .env("GIT_CEILING_DIRECTORIES", outside.dir.parent().unwrap())
        .args(["run", "ci", "--json", "--history"])
        .output()
        .unwrap();
    let bare = record(Path::new(
        summary_of(&output)["history_file"].as_str().unwrap(),
    ));
    assert!(bare.get("source").is_none(), "{bare}");
}

#[test]
fn workflow_directories_are_encoded_and_never_escape() {
    let p = Project::new(
        "[[workflows.ci.jobs]]\nname='a'\ncommand='true'\n\
         [[workflows.CI.jobs]]\nname='a'\ncommand='true'\n\
         [[workflows.\"%43%49\".jobs]]\nname='a'\ncommand='true'\n\
         [[workflows.\"../escape\".jobs]]\nname='a'\ncommand='true'\n",
    );
    let base = p.dir.join("goodflows-history");
    for (workflow, component) in [
        ("ci", "ci"),
        ("CI", "%43%49"),
        ("%43%49", "%2543%2549"),
        ("../escape", "%2E%2E%2Fescape"),
    ] {
        let output = p.run(&["run", workflow, "--json", "--history"]);
        success(&output);
        let files = history_files(&base.join(component));
        assert_eq!(files.len(), 1, "{workflow}");
        assert_eq!(
            summary_of(&output)["history_file"],
            files[0].to_str().unwrap()
        );
        assert_eq!(record(&files[0])["workflow"], workflow);
    }
    // Distinct names never share a directory, and nothing lands outside.
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    dirs.sort();
    assert_eq!(dirs, ["%2543%2549", "%2E%2E%2Fescape", "%43%49", "ci"]);
    assert!(!p.dir.join("escape").exists());
    let mut top: Vec<_> = std::fs::read_dir(&p.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(top, ["goodflows-history", "goodflows.toml"]);
}

#[test]
fn profile_partition_counts_change_the_graph_hash() {
    let p = Project::new(&format!(
        "{CI}[environments.wide.partitions]\ntest = 4\n[environments.same.settings]\nparallelism = 1\n"
    ));
    let hash = |args: &[&str]| {
        let output = p.run(&[&["run", "ci", "--json"], args].concat());
        success(&output);
        summary_of(&output)["graph_sha256"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let base = hash(&[]);
    assert_ne!(hash(&["--env", "wide"]), base);
    // A profile that changes only settings leaves the graph alone.
    assert_eq!(hash(&["--env", "same"]), base);
}

#[test]
fn zero_caps_in_workflow_settings_fail_before_jobs_run() {
    for cap in ["log_runs", "history_runs", "history_bytes"] {
        let p = Project::new(&format!(
            "[workflows.ci.settings]\n{cap} = 0\n[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'\n"
        ));
        let output = p.run(&["run", "ci", "--history", "--log-dir", "logs"]);
        assert_eq!(output.status.code(), Some(1), "{cap}");
        assert!(
            stderr(&output).contains(&format!("{cap} must be at least 1")),
            "{cap}: {}",
            stderr(&output)
        );
        assert!(!p.dir.join("ran").exists(), "{cap}");
        assert!(!p.dir.join("logs").exists(), "{cap}");
        assert!(!p.dir.join("goodflows-history").exists(), "{cap}");
    }
}

#[test]
fn skipped_instances_are_recorded_without_commands() {
    let p = Project::new(
        "[settings]\nfail_fast = false\n\
         [[jobs]]\nname='broken'\ncommand='exit 2'\n\
         [[jobs]]\nname='dependent'\ndepends_on=['broken']\ncommand='true'\n",
    );
    let output = p.run(&["--json", "--history"]);
    assert_eq!(output.status.code(), Some(1));
    let record = record(Path::new(
        summary_of(&output)["history_file"].as_str().unwrap(),
    ));
    let skipped = &record["results"][1];
    assert_eq!(skipped["status"], "skipped");
    assert_eq!(skipped["started"], false);
    assert_eq!(skipped["queued_ms"], 0);
    assert!(skipped["exit_code"].is_null(), "{skipped}");
    assert_eq!(skipped["error"], "dependency did not pass");
    assert!(skipped.get("commands").is_none(), "{skipped}");
    assert_eq!(record["results"][0]["commands"][0]["kind"], "command");
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_history_dir_is_reported_lossily() {
    use std::os::unix::ffi::OsStrExt;
    let p = Project::new("[[jobs]]\nname='a'\ncommand='true'\n");
    let name = std::ffi::OsStr::from_bytes(b"hist\xff");
    let output = p
        .command()
        .args(["--json", "--history-dir"])
        .arg(name)
        .output()
        .unwrap();
    success(&output);
    let files = history_files(&p.dir.join(name).join("default"));
    assert_eq!(files.len(), 1);
    let reported = summary_of(&output)["history_file"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(reported, files[0].to_string_lossy());
    assert!(reported.contains("hist\u{fffd}/default/"), "{reported}");
}
