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
    let text = stdout(&output);
    assert!(!text.contains("All jobs passed") && !text.contains("All checks succeeded"));
    assert!(text.contains("\ndefault  PASSED  in "), "{text}");
    assert!(text.trim_end().ends_with("1 skipped"), "{text}");
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

fn finished(events: &[serde_json::Value]) -> Vec<(String, Option<u64>)> {
    events
        .iter()
        .filter(|e| e["type"] == "job_finished")
        .map(|e| {
            (
                e["result"]["name"].as_str().unwrap().to_owned(),
                e["result"]["partition"]["index"].as_u64(),
            )
        })
        .collect()
}

fn identities(summary: &serde_json::Value) -> Vec<(String, Option<u64>, String)> {
    summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_owned(),
                r["partition"]["index"].as_u64(),
                r["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn id(name: &str, index: Option<u64>, status: &str) -> (String, Option<u64>, String) {
    (name.to_owned(), index, status.to_owned())
}

#[test]
fn dependents_of_partitioned_jobs_wait_for_every_instance_and_results_are_in_unit_order() {
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[jobs]]
name = "after"
command = "test -f done.1 && test -f done.2 && test -f done.3 && touch after"
depends_on = ["test"]
[[jobs]]
name = "test"
command = "sleep 0.0$GOODFLOWS_PARTITION; touch done.$GOODFLOWS_PARTITION"
depends_on = ["compile"]
partitions = 3
[[jobs]]
name = "compile"
command = "true"
"#,
    );
    let output = p.run(&["--json", "--jobs", "after"]);
    success(&output);
    assert!(p.dir.join("after").exists());
    let events = events(&output);
    assert_eq!(
        identities(events.last().unwrap()),
        [
            id("after", None, "passed"),
            id("test", Some(1), "passed"),
            id("test", Some(2), "passed"),
            id("test", Some(3), "passed"),
            id("compile", None, "passed"),
        ]
    );
    for result in events.last().unwrap()["results"].as_array().unwrap() {
        if result["name"] == "test" {
            assert_eq!(result["partition"]["count"], 3);
        } else {
            assert!(result.get("partition").is_none());
        }
    }
}

#[test]
fn a_failed_instance_skips_dependents_only_after_its_siblings_finish() {
    for fail_fast in [true, false] {
        let args = if fail_fast {
            vec!["--json"]
        } else {
            vec!["--json", "--no-fail-fast"]
        };
        let p = Project::new(
            r#"
[settings]
parallelism = 4
[[jobs]]
name = "test"
command = "if [ $GOODFLOWS_PARTITION = 2 ]; then exit 1; fi; sleep 0.3; touch done.$GOODFLOWS_PARTITION"
partitions = 3
[[jobs]]
name = "after"
command = "touch after"
depends_on = ["test"]
[[jobs]]
name = "leaf"
command = "touch leaf"
depends_on = ["after"]
"#,
        );
        let output = p.run(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(!p.dir.join("after").exists() && !p.dir.join("leaf").exists());
        assert!(p.dir.join("done.1").exists() && p.dir.join("done.3").exists());
        let events = events(&output);
        let order = finished(&events);
        // The direct dependent is skipped only after every instance finished.
        let position = |name: &str| order.iter().position(|(n, _)| n == name).unwrap();
        assert!(
            order[..position("after")]
                .iter()
                .filter(|(name, _)| name == "test")
                .count()
                == 3,
            "{order:?}"
        );
        // Without fail-fast the skip propagates through the chain after that; with
        // fail-fast the unstarted `leaf` is skipped for fail-fast as soon as the
        // instance fails, like any unstarted job that is not directly blocked.
        let leaf_reason = if fail_fast {
            assert_eq!(position("leaf"), 1, "{order:?}");
            "fail_fast: previous job failed"
        } else {
            assert_eq!(position("leaf"), 4, "{order:?}");
            "dependency did not pass"
        };
        let summary = events.last().unwrap();
        assert_eq!(
            identities(summary),
            [
                id("test", Some(1), "passed"),
                id("test", Some(2), "failed"),
                id("test", Some(3), "passed"),
                id("after", None, "skipped"),
                id("leaf", None, "skipped"),
            ]
        );
        assert_eq!(summary["results"][3]["error"], "dependency did not pass");
        assert_eq!(summary["results"][4]["error"], leaf_reason);
    }
}

/// Writes `release` when dropped, so commands polling for it stop even if an
/// assertion fails first.
struct Release(std::path::PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, "");
    }
}

#[test]
fn a_signal_cancels_dependents_waiting_on_siblings_of_a_failed_instance() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let p = Project::new(
        r#"
[settings]
parallelism = 4
fail_fast = false
[[jobs]]
name = "test"
partitions = 3
command = "if [ $GOODFLOWS_PARTITION = 2 ]; then exit 1; fi; touch started.$GOODFLOWS_PARTITION; i=0; while [ $i -lt 500 ] && ! test -f release; do sleep 0.01; i=$((i+1)); done"
[[jobs]]
name = "after"
command = "touch after"
depends_on = ["test"]
[[jobs]]
name = "leaf"
command = "touch leaf"
depends_on = ["after"]
"#,
    );
    let _release = Release(p.dir.join("release"));
    let mut child = p
        .command()
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    // Wait until instance 2 has failed, so `after` is settling on running siblings.
    for line in lines.by_ref() {
        let event: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        if event["type"] == "job_finished" && event["result"]["partition"]["index"] == 2 {
            assert_eq!(event["result"]["status"], "failed");
            break;
        }
    }
    let start = std::time::Instant::now();
    while !(p.dir.join("started.1").exists() && p.dir.join("started.3").exists()) {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "instances did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // SAFETY: the child PID identifies the CLI spawned above.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let summary: serde_json::Value = serde_json::from_str(&lines.last().unwrap().unwrap()).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(!p.dir.join("after").exists() && !p.dir.join("leaf").exists());
    assert_eq!(
        identities(&summary),
        [
            id("test", Some(1), "cancelled"),
            id("test", Some(2), "failed"),
            id("test", Some(3), "cancelled"),
            id("after", None, "cancelled"),
            id("leaf", None, "cancelled"),
        ]
    );
    for index in [3, 4] {
        assert_eq!(summary["results"][index]["error"], "cancelled by signal");
    }
}

#[test]
fn an_instance_skipped_by_its_own_condition_skips_dependents() {
    let p = Project::new(
        r#"
[[jobs]]
name = "test"
partitions = 3
condition = { expression = 'env.GOODFLOWS_PARTITION == "1"' }
command = "touch ran.$GOODFLOWS_PARTITION"
[[jobs]]
name = "after"
command = "touch after"
depends_on = ["test"]
"#,
    );
    let output = p.run(&["--json"]);
    // Skips are successful, but a skipped instance still does not pass.
    success(&output);
    assert!(p.dir.join("ran.1").exists());
    assert!(!p.dir.join("after").exists());
    let summary = events(&output).pop().unwrap();
    assert_eq!(
        identities(&summary),
        [
            id("test", Some(1), "passed"),
            id("test", Some(2), "skipped"),
            id("test", Some(3), "skipped"),
            id("after", None, "skipped"),
        ]
    );
    assert_eq!(summary["results"][3]["error"], "dependency did not pass");
}

#[test]
fn instances_start_in_index_order_at_the_job_position() {
    let p = Project::new(
        r#"
[settings]
parallelism = 1
[[jobs]]
name = "a"
command = "true"
[[jobs]]
name = "test"
partitions = 3
command = "true"
[[jobs]]
name = "b"
command = "true"
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    let started: Vec<_> = events(&output)
        .iter()
        .filter(|e| e["type"] == "job_started")
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_owned(),
                e["partition"]["index"].as_u64(),
            )
        })
        .collect();
    let unit = |name: &str, index| (name.to_owned(), index);
    assert_eq!(
        started,
        [
            unit("a", None),
            unit("test", Some(1)),
            unit("test", Some(2)),
            unit("test", Some(3)),
            unit("b", None),
        ]
    );
}

#[test]
fn selecting_a_partitioned_job_runs_its_prerequisites_and_every_instance() {
    let p = Project::new(
        r#"
[[jobs]]
name = "compile"
command = "touch compiled"
[[jobs]]
name = "test"
partitions = 3
depends_on = ["compile"]
command = "test -f compiled && touch ran.$GOODFLOWS_PARTITION"
[[jobs]]
name = "other"
command = "touch other"
"#,
    );
    let output = p.run(&["--json", "--jobs", "test"]);
    success(&output);
    for index in 1..=3 {
        assert!(p.dir.join(format!("ran.{index}")).exists());
    }
    assert!(!p.dir.join("other").exists());
    let events = events(&output);
    assert_eq!(events[0]["jobs"], 4);
    assert_eq!(
        identities(events.last().unwrap()),
        [
            id("compile", None, "passed"),
            id("test", Some(1), "passed"),
            id("test", Some(2), "passed"),
            id("test", Some(3), "passed"),
        ]
    );
}

#[test]
fn an_upstream_failure_skips_every_instance_with_its_partition_identity() {
    let p = Project::new(
        "[settings]\nfail_fast=false\n[[jobs]]\nname='setup'\ncommand='exit 1'\n[[jobs]]\nname='test'\ncommand='touch ran'\ndepends_on=['setup']\npartitions=3\n",
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!p.dir.join("ran").exists());
    let events = events(&output);
    let summary = events.last().unwrap();
    assert_eq!(summary["skipped"], 3);
    for index in 1..=3 {
        let result = &summary["results"][index];
        assert_eq!(result["status"], "skipped");
        assert_eq!(result["error"], "dependency did not pass");
        assert_eq!(
            result["partition"],
            serde_json::json!({"index": index, "count": 3})
        );
    }
}

#[test]
fn instances_occupy_one_slot_each() {
    // Parallelism 4: three instances and an unrelated job must all run at once.
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[jobs]]
name = "test"
command = "touch started.$GOODFLOWS_PARTITION; i=0; while [ $i -lt 250 ] && ! (test -f started.1 && test -f started.2 && test -f started.3 && test -f other); do sleep 0.02; i=$((i+1)); done; test -f other"
partitions = 3
[[jobs]]
name = "other"
command = "touch other; i=0; while [ $i -lt 250 ] && ! (test -f started.1 && test -f started.2 && test -f started.3); do sleep 0.02; i=$((i+1)); done; test -f started.3"
"#,
    );
    success(&p.run(&[]));

    // Parallelism 2: the third instance waits until a slot frees.
    let p = Project::new(
        r#"
[settings]
parallelism = 2
[[jobs]]
name = "test"
partitions = 3
command = """
case $GOODFLOWS_PARTITION in
  3) test -f started.1 && test -f started.2 && (test -f done.1 || test -f done.2) ;;
  *) touch started.$GOODFLOWS_PARTITION
     i=0; while [ $i -lt 250 ] && ! (test -f started.1 && test -f started.2); do sleep 0.02; i=$((i+1)); done
     test -f started.1 && test -f started.2 && ! test -f started.3 && touch done.$GOODFLOWS_PARTITION ;;
esac
"""
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    assert_eq!(events(&output).last().unwrap()["passed"], 3);
}

#[test]
fn fail_fast_skips_unstarted_siblings_while_running_siblings_finish() {
    let p = Project::new(
        r#"
[settings]
parallelism = 2
fail_fast = true
[[jobs]]
name = "test"
partitions = 3
command = """
case $GOODFLOWS_PARTITION in
  1) i=0; while [ $i -lt 250 ] && ! test -f started.2; do sleep 0.02; i=$((i+1)); done; exit 1 ;;
  2) touch started.2; sleep 0.3; touch finished.2 ;;
  3) touch ran.3 ;;
esac
"""
[[jobs]]
name = "after"
command = "touch after"
depends_on = ["test"]
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(p.dir.join("finished.2").exists());
    assert!(!p.dir.join("ran.3").exists() && !p.dir.join("after").exists());
    let events = events(&output);
    let summary = events.last().unwrap();
    assert_eq!(
        identities(summary),
        [
            id("test", Some(1), "failed"),
            id("test", Some(2), "passed"),
            id("test", Some(3), "skipped"),
            id("after", None, "skipped"),
        ]
    );
    assert_eq!(
        summary["results"][2]["error"],
        "fail_fast: previous job failed"
    );
    // A dependent of the failed instance keeps the dependency reason under fail-fast.
    assert_eq!(summary["results"][3]["error"], "dependency did not pass");
}

#[test]
fn fail_fast_skips_unstarted_siblings_after_an_instance_times_out() {
    // Parallelism 1: a passing sibling running alongside the timed-out instance
    // would share its deadline, so only unstarted siblings can be asserted reliably.
    let p = Project::new(
        r#"
[settings]
parallelism = 1
fail_fast = true
[[jobs]]
name = "test"
partitions = 3
timeout_secs = 1
command = "if [ $GOODFLOWS_PARTITION = 1 ]; then sleep 5; fi; touch ran.$GOODFLOWS_PARTITION"
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    for index in 1..=3 {
        assert!(!p.dir.join(format!("ran.{index}")).exists());
    }
    let summary = events(&output).pop().unwrap();
    assert_eq!(
        identities(&summary),
        [
            id("test", Some(1), "timed_out"),
            id("test", Some(2), "skipped"),
            id("test", Some(3), "skipped"),
        ]
    );
    assert_eq!(summary["results"][0]["error"], "timed out after 1s");
    for index in [1, 2] {
        assert_eq!(
            summary["results"][index]["error"],
            "fail_fast: previous job failed"
        );
    }
}

#[test]
fn a_signal_cancels_running_and_unstarted_instances() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let p = Project::new(
            "[settings]\nparallelism=2\n[[jobs]]\nname='test'\npartitions=3\ncommand='touch started.$GOODFLOWS_PARTITION; i=0; while [ $i -lt 500 ] && ! test -f release; do sleep 0.01; i=$((i+1)); done'\n",
        );
        let _release = Release(p.dir.join("release"));
        let mut child = p
            .command()
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !(p.dir.join("started.1").exists() && p.dir.join("started.2").exists()) {
            if start.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                panic!("instances did not start");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: the child PID identifies the CLI spawned above.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(!p.dir.join("started.3").exists());
        let summary: serde_json::Value =
            serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
        assert_eq!(summary["cancelled"], 3);
        assert_eq!(
            identities(&summary),
            [
                id("test", Some(1), "cancelled"),
                id("test", Some(2), "cancelled"),
                id("test", Some(3), "cancelled"),
            ]
        );
        // Running instances entered execution; the queued one never did.
        let started: Vec<_> = summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["started"].as_bool().unwrap())
            .collect();
        assert_eq!(started, [true, true, false]);
        for line in stdout(&output).lines() {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["type"] == "job_finished" {
                assert!(event["result"].get("started").is_none(), "{event}");
            }
        }
    }
}

#[test]
fn a_signal_ends_the_text_report_cancelled() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let p = Project::new(
        "[settings]\nparallelism=2\n[[jobs]]\nname='test'\npartitions=3\ncommand='touch started.$GOODFLOWS_PARTITION; i=0; while [ $i -lt 500 ] && ! test -f release; do sleep 0.01; i=$((i+1)); done'\n",
    );
    let _release = Release(p.dir.join("release"));
    let mut child = p
        .command() // sets NO_COLOR=1
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !(p.dir.join("started.1").exists() && p.dir.join("started.2").exists()) {
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("instances did not start");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // SAFETY: the child PID identifies the CLI spawned above.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let text = stdout(&output);
    let report = &text[text
        .find("\ndefault  CANCELLED  in ")
        .expect("report header")..];
    let row = report
        .lines()
        .find(|l| l.starts_with("  CANCEL  test"))
        .unwrap_or_else(|| panic!("no cancelled row: {text}"));
    assert!(row.ends_with("3/3 cancelled"), "{text}");
    // Nothing follows the counts: no Failures section and no rerun hint.
    assert!(report.ends_with("\n1 cancelled\n"), "{text}");
    for absent in ["Failures", "Rerun"] {
        assert!(!report.contains(absent), "{absent}: {text}");
    }
}
