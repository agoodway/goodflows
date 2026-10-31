mod common;
use common::*;

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
