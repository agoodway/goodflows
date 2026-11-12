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
    let last = p.run(&["last"]);
    success(&last);
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
    success(&last);
    assert_eq!(summaries(&last), summaries(&run));
    assert_eq!(events(&last).len(), 1);

    // A run of another workflow replaces the record.
    success(&p.run(&["run", "other"]));
    let last = p.run(&["last", "--json"]);
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
