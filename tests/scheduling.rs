mod common;
use common::*;

#[test]
fn conditions_run_after_dependencies_and_skips_are_successful() {
    let p = Project::new(
        r#"
[settings]
parallelism = 1
[[jobs]]
name = "conditional"
command = "touch ran"
condition = "test -f ready && test \"$ENABLED\" = yes && printf checked"
env = { ENABLED = "yes" }
shell = false
depends_on = ["setup"]
[[jobs]]
name = "setup"
command = "touch ready"
[[jobs]]
name = "disabled"
command = "touch disabled"
condition = "exit 7"
[[jobs]]
name = "dependent"
command = "touch dependent"
depends_on = ["disabled"]
[[jobs]]
name = "transitive"
command = "touch transitive"
depends_on = ["dependent"]
[[jobs]]
name = "unrelated"
command = "touch unrelated"
"#,
    );
    let output = p.run(&["--json", "--log-dir", "logs"]);
    success(&output);
    assert!(p.dir.join("ran").exists());
    assert!(p.dir.join("unrelated").exists());
    for name in ["disabled", "dependent", "transitive"] {
        assert!(!p.dir.join(name).exists());
    }
    let events: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let summary = events.last().unwrap();
    assert_eq!(summary["success"], true);
    assert_eq!(summary["passed"], 3);
    assert_eq!(summary["skipped"], 3);
    assert_eq!(summary["failed"], 0);
    assert_eq!(summary["results"][2]["status"], "skipped");
    assert_eq!(summary["results"][2]["exit_code"], 7);
    assert!(summary["results"][2]["error"]
        .as_str()
        .unwrap()
        .contains("condition"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "job_started" && e["name"] == "conditional")
            .count(),
        1
    );
    assert_eq!(p.read("logs/001-conditional.stdout.log"), "checked");

    let output = p.run(&["--jobs", "disabled"]);
    success(&output);
    assert!(stdout(&output).contains("SKIPPED"));
    assert!(!stdout(&output).contains("All jobs passed"));
}

#[test]
fn blocked_or_fail_fast_jobs_do_not_evaluate_conditions() {
    for args in [
        vec!["--parallelism", "1"],
        vec!["--parallelism", "1", "--no-fail-fast"],
    ] {
        let p = Project::new(
            r#"
[[jobs]]
name = "fail"
command = "exit 1"
[[jobs]]
name = "blocked"
command = "touch blocked"
condition = "touch evaluated"
depends_on = ["fail"]
[[jobs]]
name = "queued"
command = "touch queued"
condition = "touch unrelated_condition"
"#,
        );
        assert_eq!(p.run(&args).status.code(), Some(1));
        assert!(!p.dir.join("evaluated").exists());
        assert!(!p.dir.join("blocked").exists());
        assert_eq!(
            p.dir.join("unrelated_condition").exists(),
            args.contains(&"--no-fail-fast")
        );
    }
}

#[test]
fn failed_dependencies_skip_transitively_but_unrelated_jobs_continue() {
    let p = Project::new(
        r#"
[settings]
parallelism = 1
fail_fast = false
[[jobs]]
name = "failure"
command = "exit 7"
[[jobs]]
name = "blocked"
command = "touch blocked"
depends_on = ["failure"]
[[jobs]]
name = "also_blocked"
command = "touch also_blocked"
depends_on = ["blocked"]
[[jobs]]
name = "unrelated"
command = "touch unrelated"
"#,
    );
    let output = p.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!p.dir.join("blocked").exists());
    assert!(!p.dir.join("also_blocked").exists());
    assert!(p.dir.join("unrelated").exists());
    assert!(stdout(&output).contains("SKIPPED"));
    assert!(stdout(&output).contains("exit=7"));
    assert!(stdout(&output).contains("1 failed"));
}

#[test]
fn fail_fast_stops_queued_work_and_defaults_consistently() {
    for settings in ["", "[settings]\n", "[settings]\nfail_fast=true\n"] {
        let p = Project::new(&format!("{settings}[[jobs]]\nname='fail'\ncommand='exit 1'\n[[jobs]]\nname='queued'\ncommand='touch queued'"));
        let output = p.run(&["--parallelism", "1"]);
        assert!(!output.status.success());
        assert!(!p.dir.join("queued").exists(), "settings: {settings}");
        assert!(stdout(&output).contains("SKIPPED"));
    }
}

#[test]
fn ready_dependent_starts_before_unrelated_slow_job_finishes() {
    let p = Project::new(
        r#"
[settings]
parallelism = 2
[[jobs]]
name = "slow"
command = "i=0; while [ ! -f released ] && [ $i -lt 100 ]; do sleep 0.02; i=$((i+1)); done; test -f released"
[[jobs]]
name = "quick"
command = "true"
[[jobs]]
name = "release"
command = "touch released"
depends_on = ["quick"]
"#,
    );
    success(&p.run(&[]));
}

#[test]
fn parallelism_is_bounded_and_running_jobs_finish_after_failure() {
    let p = Project::new(
        r#"
[settings]
parallelism = 2
[[jobs]]
name = "running"
command = "touch started; sleep 0.15; touch finished"
[[jobs]]
name = "fail"
command = "while [ ! -f started ]; do sleep 0.01; done; exit 1"
[[jobs]]
name = "queued"
command = "touch queued"
"#,
    );
    assert!(!p.run(&[]).status.success());
    assert!(p.dir.join("finished").exists());
    assert!(!p.dir.join("queued").exists());

    let mut config = "[settings]\nparallelism=1\n".to_owned();
    for name in ["a", "b", "c"] {
        config.push_str(&format!(
            "[[jobs]]\nname='{name}'\ncommand='mkdir exclusive && sleep 0.02 && rmdir exclusive'\n"
        ));
    }
    success(&Project::new(&config).run(&[]));

    let mut config = "[settings]\nparallelism=1\n".to_owned();
    for name in ["a", "b", "c"] {
        config.push_str(&format!(
            "[[jobs]]\nname='{name}'\n[[jobs.steps]]\nid='acquire'\ncommand='mkdir exclusive && sleep 0.02'\n[[jobs.steps]]\nid='release'\ncommand='rmdir exclusive'\n"
        ));
    }
    success(&Project::new(&config).run(&[]));
}

#[test]
fn joins_wait_for_all_prerequisites_and_summary_preserves_config_order() {
    let p = Project::new(
        r#"
[[jobs]]
name = "join"
command = "test -f left && test -f right && touch joined"
depends_on = ["left", "right"]
[[jobs]]
name = "left"
command = "touch left"
[[jobs]]
name = "right"
command = "sleep 0.05; touch right"
"#,
    );
    let output = p.run(&["--json", "--jobs", "join", "--parallelism", "2"]);
    success(&output);
    assert!(p.dir.join("joined").exists());
    let summary: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
    let names: Vec<_> = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["join", "left", "right"]);
}

#[test]
fn cli_can_override_fail_fast_and_parallelism_config() {
    let p = Project::new("[settings]\nparallelism=4\nfail_fast=true\n[[jobs]]\nname='fail'\ncommand='exit 1'\n[[jobs]]\nname='other'\ncommand='touch other'");
    let output = p.run(&["--no-fail-fast", "--parallelism", "1", "--json"]);
    assert!(!output.status.success());
    assert!(p.dir.join("other").exists());
    let start: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().next().unwrap()).unwrap();
    assert_eq!(start["parallelism"], 1);
    assert_eq!(start["fail_fast"], false);
}
