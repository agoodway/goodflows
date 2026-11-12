mod common;
use common::*;
use serde_json::Value;

fn events(output: &std::process::Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn summaries(output: &std::process::Output) -> Vec<Value> {
    events(output)
        .into_iter()
        .filter(|event| event["type"] == "summary")
        .collect()
}

fn error_code(output: &std::process::Output) -> Value {
    let events = events(output);
    let error = events.last().unwrap();
    assert_eq!(error["type"], "error", "{}", stdout(output));
    error["code"].clone()
}

/// `bad` fails until the file `fixed` exists; `after` depends on it; `ok`
/// passes; `late` is skipped by a false condition and never rerun.
const CI: &str = "[settings]\nfail_fast = false\n\
[environments.staging.settings]\nparallelism = 2\n\
[[workflows.ci.jobs]]\nname = 'prep'\ncommand = 'true'\n\
[[workflows.ci.jobs]]\nname = 'bad'\ncommand = 'echo out-$GOODFLOWS_PARTITION; echo err >&2; test -f fixed'\npartitions = 2\ndepends_on = ['prep']\n\
[[workflows.ci.jobs]]\nname = 'after'\ncommand = 'true'\ndepends_on = ['bad']\n\
[[workflows.ci.jobs]]\nname = 'ok'\ncommand = 'true'\n\
[[workflows.ci.jobs]]\nname = 'late'\ncommand = 'true'\ncondition = { shell = 'false' }\n\
[[workflows.other.jobs]]\nname = 'x'\ncommand = 'true'\n";

#[test]
fn both_commands_report_when_there_is_no_previous_run() {
    let p = Project::new(CI);
    for args in [
        &["last"][..],
        &["run", "--failed"],
        &["run", "ci", "--failed"],
    ] {
        let output = p.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("no previous local run"),
            "{args:?}: {}",
            stderr(&output)
        );
        let json = p.run(&[args, &["--json"]].concat());
        assert_eq!(error_code(&json), "no_last_run", "{args:?}");
    }
}

#[test]
fn last_prints_the_failed_runs_report_again() {
    let p = Project::new(CI);
    let run = p.run(&[
        "run",
        "ci",
        "--env",
        "staging",
        "--output",
        "failures",
        "--log-dir",
        "logs",
    ]);
    assert_eq!(run.status.code(), Some(1), "{}", stderr(&run));
    // `last` exits with the recorded run's exit code.
    let last = p.run(&["last"]);
    assert_eq!(last.status.code(), Some(1), "{}", stderr(&last));
    let (run, last) = (stdout(&run), stdout(&last));
    // The whole closing report, failures, logs and rerun hint included,
    // without the run's live status lines.
    let report = &run[run.find("\nci  FAILED").expect(&run)..];
    assert_eq!(last, report);
    assert!(
        last.contains("  stdout:\n    out-2\n  stderr:\n    err\n"),
        "{last}"
    );
    assert!(last.contains("stderr log: logs/ci/"), "{last}");
    assert!(last.ends_with("Rerun failed: goodflows run ci --env=staging --jobs='bad,after'\n"));

    // `--json` repeats the run's summary event unchanged.
    let run = p.run(&["run", "ci", "--json"]);
    let last = p.run(&["last", "--json"]);
    assert_eq!(last.status.code(), Some(1));
    assert_eq!(summaries(&last), summaries(&run));
    assert_eq!(events(&last).len(), 1);

    // A run of another workflow replaces the record, and it passed.
    success(&p.run(&["run", "other"]));
    let last = p.run(&["last", "--json"]);
    success(&last);
    assert_eq!(summaries(&last)[0]["workflow"], "other");
    // Run-only flags do not apply.
    assert_eq!(
        error_code(&p.run(&["last", "--json", "--jobs", "ok"])),
        "usage"
    );
}

#[test]
fn last_shows_called_workflow_reports_before_the_root() {
    let p = Project::new(
        "[[workflows.ci.jobs]]\nname = 'call'\nworkflow = 'suite'\n\
         [[workflows.suite.jobs]]\nname = 'work'\ncommand = 'false'\n",
    );
    let run = p.run(&["run", "ci", "--json"]);
    assert_eq!(run.status.code(), Some(1));
    let last = p.run(&["last", "--json"]);
    let shown = summaries(&last);
    assert_eq!(shown, summaries(&run));
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[0]["workflow"], "suite");
    assert_eq!(shown[0]["parent_run_id"], shown[1]["invocation_id"]);
}

#[test]
fn run_failed_reruns_the_rerun_hint_jobs() {
    let p = Project::new(CI);
    let first = p.run(&[
        "run",
        "ci",
        "--env",
        "staging",
        "--config",
        "goodflows.toml",
    ]);
    assert_eq!(first.status.code(), Some(1));
    // The hint the failed run printed.
    let hint = stdout(&first)
        .lines()
        .find_map(|line| line.strip_prefix("Rerun failed: "))
        .expect("rerun hint")
        .to_owned();
    assert_eq!(
        hint,
        "goodflows run ci --config=goodflows.toml --env=staging --jobs='bad,after'"
    );

    // Still failing: the same jobs, with their dependency, in the same
    // workflow and environment, keeping the explicit --config.
    let again = p.run(&["run", "--failed", "--json"]);
    assert_eq!(again.status.code(), Some(1));
    let summary = summaries(&again).pop().unwrap();
    assert_eq!(summary["workflow"], "ci");
    assert_eq!(summary["environment"], "staging");
    assert_eq!(summary["parallelism"], 2);
    let names: Vec<_> = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["prep", "bad", "bad", "after"]);
    // Its own hint names the same jobs; it was not given --config.
    let text = stdout(&p.run(&["last"]));
    assert!(
        text.ends_with("Rerun failed: goodflows run ci --env=staging --jobs='bad,after'\n"),
        "{text}"
    );

    // A named workflow or environment must match the last run's.
    let other = p.run(&["run", "other", "--failed", "--json"]);
    assert_eq!(error_code(&other), "no_last_run");
    let env = p.run(&["run", "--failed", "--env", "prod", "--json"]);
    assert_eq!(error_code(&env), "no_last_run");
    assert_eq!(
        error_code(&p.run(&["run", "--failed", "--jobs", "ok", "--json"])),
        "usage"
    );

    // Fixed: the rerun passes, and then nothing is left to rerun.
    std::fs::write(p.dir.join("fixed"), "").unwrap();
    success(&p.run(&["run", "ci", "--failed", "--env", "staging"]));
    let nothing = p.run(&["run", "--failed", "--json"]);
    assert_eq!(nothing.status.code(), Some(2));
    assert_eq!(error_code(&nothing), "no_failed_jobs");
}

#[test]
fn a_run_leaves_git_status_clean_and_keeps_a_users_gitignore() {
    let p = Project::new("[[jobs]]\nname = 'a'\ncommand = 'true'\n");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&p.dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    git(&["init", "-q"]);
    success(&p.run(&[]));
    assert_eq!(p.read(".goodflows/.gitignore"), "*\n");
    assert_eq!(git(&["status", "--short"]), "?? goodflows.toml\n");

    // A user's own rules stay.
    std::fs::write(p.dir.join(".goodflows/.gitignore"), "last/\n").unwrap();
    success(&p.run(&[]));
    assert_eq!(p.read(".goodflows/.gitignore"), "last/\n");
}

#[test]
fn last_exits_with_the_recorded_runs_exit_code() {
    let p = Project::new(
        "[[workflows.pass.jobs]]\nname = 'a'\ncommand = 'true'\n\
         [[workflows.fail.jobs]]\nname = 'a'\ncommand = 'false'\n\
         [[workflows.slow.jobs]]\nname = 'a'\ncommand = 'touch started; sleep 30'\n",
    );
    assert_eq!(p.run(&["last"]).status.code(), Some(2));
    for (workflow, code) in [("fail", 1), ("pass", 0)] {
        assert_eq!(p.run(&["run", workflow]).status.code(), Some(code));
        assert_eq!(p.run(&["last"]).status.code(), Some(code), "{workflow}");
        assert_eq!(p.run(&["last", "--json"]).status.code(), Some(code));
    }

    // A run stopped by SIGTERM exits 143, and so does `last`.
    let mut run = p
        .command()
        .args(["run", "slow"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !p.dir.join("started").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the job never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // SAFETY: kill only sends a signal to the child this test spawned.
    unsafe { libc::kill(run.id() as i32, libc::SIGTERM) };
    assert_eq!(run.wait().unwrap().code(), Some(143));
    assert_eq!(p.run(&["last"]).status.code(), Some(143));

    // A record written before the exit code was kept falls back to the
    // summary: a failed run exits 1.
    let path = p.dir.join(".goodflows/last/goodflows.toml.json");
    assert_eq!(p.run(&["run", "fail"]).status.code(), Some(1));
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(record["exit_code"], 1);
    record.as_object_mut().unwrap().remove("exit_code");
    std::fs::write(&path, record.to_string()).unwrap();
    assert_eq!(p.run(&["last"]).status.code(), Some(1));
}
