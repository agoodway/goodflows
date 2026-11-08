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
    // The shell condition and the command share one raw stdout file in the run directory.
    let run = run_dir(&p.dir.join("logs"));
    assert_eq!(
        std::fs::read_to_string(run.join("001-conditional.stdout.log")).unwrap(),
        "checked"
    );

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
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    for fail_fast in [true, false] {
        let args = if fail_fast {
            vec!["--json"]
        } else {
            vec!["--json", "--no-fail-fast"]
        };
        // Siblings wait for `release` instead of sleeping, so a slow runner cannot
        // finish them before instance 2's failure is handled. The poll is bounded
        // so a scheduling regression fails the assertions below rather than hangs.
        let p = Project::new(
            r#"
[settings]
parallelism = 4
[[jobs]]
name = "test"
command = "if [ $GOODFLOWS_PARTITION = 2 ]; then exit 1; fi; i=0; while [ $i -lt 3000 ] && ! test -f release; do sleep 0.01; i=$((i+1)); done; touch done.$GOODFLOWS_PARTITION"
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
        let release = Release(p.dir.join("release"));
        let mut child = p
            .command()
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // With fail-fast, release the siblings once `leaf` has been skipped;
        // without it, once instance 2 has failed, since `after` and `leaf`
        // cannot finish until the siblings do.
        let mut events = Vec::new();
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let event: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
            let trigger = event["type"] == "job_finished"
                && if fail_fast {
                    event["result"]["name"] == "leaf"
                } else {
                    event["result"]["partition"]["index"] == 2
                };
            events.push(event);
            if trigger {
                std::fs::write(&release.0, "").unwrap();
            }
        }
        assert_eq!(child.wait().unwrap().code(), Some(1));
        drop(release);
        assert!(!p.dir.join("after").exists() && !p.dir.join("leaf").exists());
        assert!(p.dir.join("done.1").exists() && p.dir.join("done.3").exists());
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

fn summary_of(output: &std::process::Output) -> serde_json::Value {
    let summary: serde_json::Value =
        serde_json::from_str(stdout(output).lines().last().unwrap()).unwrap();
    assert_eq!(summary["type"], "summary");
    summary
}

#[test]
fn log_runs_keep_the_newest_ten_and_leave_unrelated_entries() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='echo hi'\n");
    let logs = p.dir.join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::write(logs.join("notes.txt"), "keep").unwrap();
    // A matching name that is a symlink is never followed or removed.
    let target = p.dir.join("target-dir");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("precious"), "keep").unwrap();
    let link = logs.join("20000101T000000Z-00000000-0000-4000-8000-000000000000");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut last = None;
    for _ in 0..11 {
        let output = p.run(&["--json", "--log-dir", "logs"]);
        success(&output);
        last = Some(summary_of(&output)["log_dir"].as_str().unwrap().to_owned());
    }
    let runs = run_dirs(&logs);
    assert_eq!(runs.len(), 10, "{runs:?}");
    assert!(p.dir.join(last.unwrap()).join("001-a.stdout.log").exists());
    assert_eq!(
        std::fs::read_to_string(logs.join("notes.txt")).unwrap(),
        "keep"
    );
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(target.join("precious").exists());
    // Each kept run directory holds its logs and its lock file.
    for run in &runs {
        assert_eq!(
            log_names(run),
            ["001-a.stderr.log", "001-a.stdout.log"],
            "{}",
            run.display()
        );
        assert!(run.join(".goodflows.lock").exists());
    }
}

#[test]
fn log_runs_follow_profile_cli_and_workflow_settings() {
    let p = Project::new(
        "[settings]\nlog_runs = 2\n\
         [[workflows.ci.jobs]]\nname='a'\ncommand='true'\n\
         [workflows.other.settings]\nlog_runs = 5\n\
         [[workflows.other.jobs]]\nname='a'\ncommand='true'\n\
         [environments.tight.settings]\nlog_runs = 1\n",
    );
    for _ in 0..3 {
        success(&p.run(&["run", "ci", "--log-dir", "logs"]));
        success(&p.run(&["run", "other", "--log-dir", "logs"]));
    }
    assert_eq!(run_dirs(&p.dir.join("logs/ci")).len(), 2);
    // The workflow setting applies to its workflow only.
    assert_eq!(run_dirs(&p.dir.join("logs/other")).len(), 3);
    for _ in 0..5 {
        success(&p.run(&["run", "ci", "--log-dir", "logs", "--log-runs", "4"]));
    }
    assert_eq!(run_dirs(&p.dir.join("logs/ci")).len(), 4);
    success(&p.run(&["run", "ci", "--env", "tight", "--log-dir", "logs"]));
    assert_eq!(run_dirs(&p.dir.join("logs/ci")).len(), 1);
    // Without --log-dir the setting has no effect and nothing is created.
    success(&p.run(&["run", "ci", "--log-runs", "1"]));
    assert_eq!(run_dirs(&p.dir.join("logs/ci")).len(), 1);
    let output = p.run(&["run", "ci", "--log-dir", "logs", "--log-runs", "0"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn log_runs_profile_cap_of_two() {
    let p = Project::new(
        "[[jobs]]\nname='a'\ncommand='true'\n[environments.ci.settings]\nlog_runs = 2\n",
    );
    for _ in 0..3 {
        success(&p.run(&["--env", "ci", "--log-dir", "logs"]));
    }
    assert_eq!(run_dirs(&p.dir.join("logs")).len(), 2);
}

#[test]
fn overlapping_runs_never_prune_a_live_log_directory() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let p = Project::new(
        "[[jobs]]\nname='a'\ncommand='echo first; touch started; i=0; while [ $i -lt 500 ] && ! test -f release; do sleep 0.01; i=$((i+1)); done; echo after'\n",
    );
    let release = Release(p.dir.join("release"));
    let first = p
        .command()
        .args(["--json", "--log-dir", "logs", "--log-runs", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !p.dir.join("started").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "first run did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let live = run_dir(&p.dir.join("logs"));
    // The second run must not wait for the first, so it gets its own job.
    std::fs::write(
        p.dir.join("second.toml"),
        "[[jobs]]\nname='a'\ncommand='echo second'\n",
    )
    .unwrap();
    let second = p.run(&[
        "--json",
        "--config",
        "second.toml",
        "--log-dir",
        "logs",
        "--log-runs",
        "1",
    ]);
    success(&second);
    assert!(live.exists(), "live run directory was pruned");
    drop(release);
    let first = first.wait_with_output().unwrap();
    success(&first);
    let log = std::fs::read_to_string(live.join("001-a.stdout.log")).unwrap();
    assert_eq!(log, "first\nafter\n");
    assert_eq!(
        summary_of(&first)["log_dir"],
        format!("logs/{}", live.file_name().unwrap().to_str().unwrap())
    );
}

#[test]
fn queue_and_dependency_waits_are_measured_per_instance() {
    let p = Project::new(
        "[settings]\nparallelism = 1\nfail_fast = false\n\
         [[jobs]]\nname='first'\ncommand='sleep 0.3'\n\
         [[jobs]]\nname='second'\ncommand='true'\n\
         [[jobs]]\nname='broken'\ncommand='exit 1'\n\
         [[jobs]]\nname='dependent'\ndepends_on=['broken']\ncommand='true'\n",
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    let result = |name: &str| {
        summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(summary["max_in_flight"], 1);
    // Ready from the start, then waited for the single slot: its blocked
    // time ends before `first` finishes, and the queue wait covers the rest.
    let first = result("first");
    let second = result("second");
    let first_ms = first["duration_ms"].as_u64().unwrap();
    assert!(
        second["blocked_ms"].as_u64().unwrap() < first_ms,
        "{first} {second}"
    );
    assert!(second["queued_ms"].as_u64().unwrap() >= 250, "{second}");
    // Skipped for a failed dependency: blocked until the skip, never queued.
    let dependent = result("dependent");
    assert_eq!(dependent["status"], "skipped");
    assert_eq!(dependent["started"], false);
    assert_eq!(dependent["queued_ms"], 0);
    assert!(
        dependent["blocked_ms"].as_u64().unwrap() >= 250,
        "{dependent}"
    );
    assert!(dependent.get("commands").is_none(), "{dependent}");
}

#[test]
fn max_in_flight_counts_concurrent_instances() {
    let p = Project::new(
        "[settings]\nparallelism = 3\n[[jobs]]\nname='t'\npartitions=3\ncommand='sleep 0.2'\n",
    );
    let output = p.run(&["--json"]);
    success(&output);
    assert_eq!(summary_of(&output)["max_in_flight"], 3);
}

#[test]
fn a_ready_job_skipped_by_fail_fast_keeps_its_queue_wait() {
    let p = Project::new(
        "[settings]\nparallelism = 1\nfail_fast = true\n\
         [[jobs]]\nname='first'\ncommand='sleep 0.3; exit 1'\n\
         [[jobs]]\nname='second'\ncommand='touch ran'\n",
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    let first = &summary["results"][0];
    let second = &summary["results"][1];
    assert_eq!(second["status"], "skipped");
    assert_eq!(second["error"], "fail_fast: previous job failed");
    assert_eq!(second["started"], false);
    // Ready at once, then waited for the slot until the skip.
    assert!(
        second["blocked_ms"].as_u64().unwrap() < first["duration_ms"].as_u64().unwrap(),
        "{first} {second}"
    );
    assert!(second["queued_ms"].as_u64().unwrap() >= 250, "{second}");
    assert!(second.get("commands").is_none(), "{second}");
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn a_plain_chain_listed_before_its_prerequisites_keeps_the_fail_fast_reason() {
    // Without workflow calls, skips follow configuration order: `p3` is
    // visited while `p2` is still pending, so fail-fast skips it first.
    let p = Project::new(
        "[settings]\nparallelism = 1\nfail_fast = true\n\
         [[jobs]]\nname='p3'\ncommand='touch p3'\ndepends_on=['p2']\n\
         [[jobs]]\nname='a'\ncommand='exit 1'\n\
         [[jobs]]\nname='p1'\ncommand='touch p1'\ndepends_on=['a']\n\
         [[jobs]]\nname='p2'\ncommand='touch p2'\ndepends_on=['p1']\n",
    );
    let output = p.run(&["--json", "--no-history"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = summary_of(&output);
    let reasons: Vec<_> = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r["error"].as_str().unwrap()))
        .collect();
    assert_eq!(
        reasons,
        [
            ("p3", "fail_fast: previous job failed"),
            ("a", "exited with 1"),
            ("p1", "dependency did not pass"),
            ("p2", "dependency did not pass"),
        ],
        "{summary}"
    );
    let output = p.run(&["--no-history"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).ends_with("Rerun failed: goodflows run default --jobs='a,p1,p2'\n"),
        "{}",
        stdout(&output)
    );
    for absent in ["p1", "p2", "p3"] {
        assert!(!p.dir.join(absent).exists(), "{absent}");
    }
}

#[test]
fn a_dependent_of_a_failed_job_keeps_the_dependency_reason_under_fail_fast() {
    // The failure must not stop the dependent before the failed result is
    // recorded. The window is narrow, so repeat it on concurrent threads.
    let config = "[settings]\nparallelism = 1\nfail_fast = true\n\
         [[jobs]]\nname='a'\ncommand='exit 1'\n\
         [[jobs]]\nname='b'\ncommand='touch b'\ndepends_on=['a']\n\
         [[jobs]]\nname='c'\ncommand='touch c'\n";
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || {
                let p = Project::new(config);
                for _ in 0..30 {
                    let output = p.run(&["--json", "--no-history"]);
                    assert_eq!(output.status.code(), Some(1));
                    let summary = summary_of(&output);
                    let reason = |name: &str| {
                        summary["results"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|r| r["name"] == name)
                            .unwrap()["error"]
                            .clone()
                    };
                    assert_eq!(reason("b"), "dependency did not pass", "{summary}");
                    assert_eq!(reason("c"), "fail_fast: previous job failed", "{summary}");
                }
                assert!(!p.dir.join("b").exists() && !p.dir.join("c").exists());
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn workflow_log_runs_isolate_workflows() {
    let p = Project::new(
        "[workflows.ci.settings]\nlog_runs = 2\n\
         [[workflows.ci.jobs]]\nname='a'\ncommand='true'\n\
         [[workflows.other.jobs]]\nname='a'\ncommand='true'\n",
    );
    for _ in 0..3 {
        success(&p.run(&["run", "ci", "--log-dir", "logs"]));
    }
    for _ in 0..11 {
        success(&p.run(&["run", "other", "--log-dir", "logs"]));
    }
    assert_eq!(run_dirs(&p.dir.join("logs/ci")).len(), 2);
    // `other` sets nothing, so it keeps the default of ten.
    assert_eq!(run_dirs(&p.dir.join("logs/other")).len(), 10);
}

#[test]
fn a_log_prune_failure_warns_and_keeps_the_exit_code() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // Permissions do not stop root.
    }
    use std::os::unix::fs::PermissionsExt;
    let p = Project::new("[[jobs]]\nname='a'\ncommand='true'\n");
    let old = p
        .dir
        .join("logs/20000101T000000Z-00000000-0000-4000-8000-000000000000");
    std::fs::create_dir_all(&old).unwrap();
    let lock = old.join(".goodflows.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
    let output = p.run(&["--json", "--log-dir", "logs", "--log-runs", "1"]);
    success(&output);
    assert!(
        stderr(&output).contains("failed to prune log runs"),
        "{}",
        stderr(&output)
    );
    // The unopenable lock keeps its directory, beside the new run.
    assert!(old.exists());
    assert_eq!(run_dirs(&p.dir.join("logs")).len(), 2);
    assert!(summary_of(&output)["log_dir"].is_string());
}

// ---- workflow-call capacity and failure policy ----

/// A command that records its start and end in `trace` around `sleep`.
fn traced(label: &str, sleep: &str) -> String {
    format!("echo + {label} >> trace; sleep {sleep}; echo - {label} >> trace")
}

/// The most traced commands running at once whose label starts with `prefix`.
fn peak(p: &Project, prefix: &str) -> usize {
    let (mut now, mut max) = (0usize, 0);
    for line in p.read("trace").lines() {
        let (sign, label) = line.split_once(' ').unwrap();
        if !label.starts_with(prefix) {
            continue;
        }
        if sign == "+" {
            now += 1;
            max = max.max(now);
        } else {
            now -= 1;
        }
    }
    max
}

fn summaries(output: &std::process::Output) -> Vec<serde_json::Value> {
    events(output)
        .into_iter()
        .filter(|e| e["type"] == "summary")
        .collect()
}

#[test]
fn nested_calls_complete_with_a_single_slot() {
    let p = Project::new(&format!(
        r#"
[settings]
parallelism = 1
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
[[workflows.ci.jobs]]
name = "root"
command = "{}"
[[workflows.suite.jobs]]
name = "nested"
workflow = "leaf"
[[workflows.suite.jobs]]
name = "cmd"
command = "{}"
[[workflows.leaf.jobs]]
name = "cmd"
command = "{}"
"#,
        traced("root", "0.05"),
        traced("suite", "0.05"),
        traced("leaf", "0.05")
    ));
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(peak(&p, ""), 1);
    assert_eq!(p.read("trace").lines().count(), 6);
    // Waiting calls never count: every invocation saw one unit at most.
    for summary in summaries(&output) {
        assert_eq!(summary["max_in_flight"], 1, "{summary}");
    }
}

#[test]
fn sibling_calls_share_the_root_ceiling() {
    let suite: String = (0..3)
        .map(|i| {
            format!(
                "[[workflows.suite.jobs]]\nname='s{i}'\ncommand='{}'\n",
                traced("s", "0.2")
            )
        })
        .collect();
    let p = Project::new(&format!(
        "[settings]\nparallelism = 3\n\
         [[workflows.ci.jobs]]\nname='a'\nworkflow='suite'\n\
         [[workflows.ci.jobs]]\nname='b'\nworkflow='suite'\n\
         [[workflows.ci.jobs]]\nname='root'\ncommand='{}'\n{suite}",
        traced("r", "0.2")
    ));
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(peak(&p, ""), 3);
    let summaries = summaries(&output);
    assert_eq!(summaries.last().unwrap()["max_in_flight"], 3);
    assert!(summaries
        .iter()
        .all(|s| s["max_in_flight"].as_u64().unwrap() <= 3));
}

#[test]
fn a_lower_child_limit_caps_its_subtree_only() {
    let suite: String = (0..3)
        .map(|i| {
            format!(
                "[[workflows.suite.jobs]]\nname='s{i}'\ncommand='{}'\n",
                traced("child", "0.15")
            )
        })
        .collect();
    let root: String = (0..3)
        .map(|i| {
            format!(
                "[[workflows.ci.jobs]]\nname='r{i}'\ncommand='{}'\n",
                traced("root", "0.3")
            )
        })
        .collect();
    let p = Project::new(&format!(
        "[workflows.ci.settings]\nparallelism = 4\n[workflows.suite.settings]\nparallelism = 1\n\
         [[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\n{root}{suite}"
    ));
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(peak(&p, "child"), 1);
    // Root work used the remaining slots alongside the child.
    assert_eq!(peak(&p, ""), 4);
    let summaries = summaries(&output);
    assert_eq!(summaries[0]["workflow"], "suite");
    assert_eq!(summaries[0]["max_in_flight"], 1);
    assert_eq!(summaries[1]["max_in_flight"], 4);
}

#[test]
fn child_partitions_take_one_slot_each() {
    let p = Project::new(&format!(
        "[settings]\nparallelism = 2\n\
         [[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\n\
         [[workflows.suite.jobs]]\nname='test'\npartitions=4\ncommand='{}'\n",
        traced("p", "0.15")
    ));
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(peak(&p, ""), 2);
    assert_eq!(p.read("trace").lines().count(), 8);
    let summaries = summaries(&output);
    assert_eq!(summaries[0]["results"].as_array().unwrap().len(), 4);
    // The root counts its one call, which held no slot itself.
    assert_eq!(summaries[1]["results"].as_array().unwrap().len(), 1);
}

#[test]
fn a_stepped_child_job_and_a_call_condition_shell_hold_one_slot() {
    let p = Project::new(
        r#"
[settings]
parallelism = 1
[[workflows.ci.jobs]]
name = "root"
command = "mkdir exclusive && sleep 0.05 && rmdir exclusive"
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
condition = { shell = "mkdir exclusive && sleep 0.05 && rmdir exclusive" }
[[workflows.suite.jobs]]
name = "stepped"
[[workflows.suite.jobs.steps]]
id = "acquire"
command = "mkdir exclusive && sleep 0.05"
[[workflows.suite.jobs.steps]]
id = "release"
command = "rmdir exclusive"
[[workflows.suite.jobs]]
name = "other"
command = "mkdir exclusive && sleep 0.05 && rmdir exclusive"
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    let summaries = summaries(&output);
    let (child, root) = (&summaries[0], &summaries[1]);
    assert_eq!(root["max_in_flight"], 1);
    assert_eq!(child["max_in_flight"], 1);
    // The call's own cost is its condition shell; child commands stay in the child.
    let call = &root["results"][1];
    let commands = call["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["kind"], "condition");
    assert_eq!(child["results"][0]["commands"].as_array().unwrap().len(), 2);
    assert_eq!(child["results"][1]["commands"][0]["kind"], "command");
}

/// A failing child job and a long-running sibling under nested fail-fast
/// policies; `bad-done` is the barrier the other jobs wait on.
fn policy_config(root_fail_fast: bool, child_fail_fast: bool) -> String {
    let wait = "i=0; while [ $i -lt 500 ] && ! test -f bad-done; do sleep 0.01; i=$((i+1)); done";
    format!(
        r#"
[workflows.ci.settings]
parallelism = 6
fail_fast = {root_fail_fast}
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
[[workflows.ci.jobs]]
name = "root-slow"
command = "{wait}; sleep 0.3"
[[workflows.ci.jobs]]
name = "root-later"
command = "touch root-later"
depends_on = ["root-slow"]
[[workflows.ci.jobs]]
name = "sibling"
workflow = "independent"
depends_on = ["root-slow"]
[workflows.suite.settings]
fail_fast = {child_fail_fast}
[[workflows.suite.jobs]]
name = "running"
command = "{wait}; sleep 0.3; touch settled"
[[workflows.suite.jobs]]
name = "bad"
command = "touch bad-done; exit 1"
[[workflows.suite.jobs]]
name = "after-bad"
command = "touch after-bad"
depends_on = ["bad"]
[[workflows.suite.jobs]]
name = "later"
command = "touch later"
depends_on = ["running"]
[workflows.independent.settings]
fail_fast = true
[[workflows.independent.jobs]]
name = "work"
command = "touch sibling-work"
"#
    )
}

fn status(summary: &serde_json::Value, name: &str) -> (String, String) {
    let result = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap();
    (
        result["status"].as_str().unwrap().to_owned(),
        result["error"].as_str().unwrap_or_default().to_owned(),
    )
}

fn workflow<'a>(summaries: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    summaries.iter().find(|s| s["workflow"] == name).unwrap()
}

fn pair(status: &str, error: &str) -> (String, String) {
    (status.to_owned(), error.to_owned())
}

#[test]
fn a_child_cannot_bypass_an_enabled_root_fail_fast() {
    let p = Project::new(&policy_config(true, false));
    let output = p.run(&["run", "ci", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summaries = summaries(&output);
    let (child, root) = (workflow(&summaries, "suite"), summaries.last().unwrap());
    // Running jobs settle; nothing new starts anywhere in the tree.
    assert!(p.dir.join("settled").exists());
    assert_eq!(status(child, "running"), pair("passed", ""));
    assert_eq!(
        status(child, "after-bad"),
        pair("skipped", "dependency did not pass")
    );
    assert_eq!(
        status(child, "later"),
        pair("skipped", "fail_fast: previous job failed")
    );
    assert_eq!(status(root, "root-slow"), pair("passed", ""));
    assert_eq!(
        status(root, "root-later"),
        pair("skipped", "fail_fast: previous job failed")
    );
    assert_eq!(
        status(root, "sibling"),
        pair("skipped", "fail_fast: previous job failed")
    );
    assert_eq!(
        status(root, "call"),
        pair("failed", "workflow 'suite' failed")
    );
    for absent in ["later", "after-bad", "root-later", "sibling-work"] {
        assert!(!p.dir.join(absent).exists(), "{absent}");
    }
    assert_eq!(summaries.len(), 2, "no sibling child started");
}

#[test]
fn a_child_fail_fast_does_not_stop_independent_root_work() {
    let p = Project::new(&policy_config(false, true));
    let output = p.run(&["run", "ci", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summaries = summaries(&output);
    let (child, root) = (workflow(&summaries, "suite"), summaries.last().unwrap());
    assert_eq!(root["workflow"], "ci");
    assert_eq!(status(child, "running"), pair("passed", ""));
    assert_eq!(
        status(child, "later"),
        pair("skipped", "fail_fast: previous job failed")
    );
    assert_eq!(
        status(child, "after-bad"),
        pair("skipped", "dependency did not pass")
    );
    // Independent root work and another child with fail-fast enabled continue.
    assert_eq!(status(root, "root-later"), pair("passed", ""));
    assert_eq!(status(root, "sibling"), pair("passed", ""));
    assert!(p.dir.join("root-later").exists() && p.dir.join("sibling-work").exists());
    assert!(!p.dir.join("later").exists());
    // With every policy disabled, the child's independent work continues too.
    let p = Project::new(&policy_config(false, false));
    let output = p.run(&["run", "ci", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summaries = self::summaries(&output);
    assert_eq!(
        status(workflow(&summaries, "suite"), "later"),
        pair("passed", "")
    );
    assert!(p.dir.join("later").exists());
}

#[test]
fn a_root_waiting_for_capacity_keeps_the_dependency_reason_after_a_child_fails() {
    // `prepare` is first, so its shell condition takes the only slot and
    // holds it until the child is waiting; the child's `test` gets it next.
    // `prepare`'s own child needs no slot (its only job is a skipped call),
    // so `other` becomes ready while `test` runs and waits for capacity
    // until `test` fails. `announce` depends on the call only through
    // `publish`, and the chain `p4` -> `p3` -> `p2` -> `p1` -> call is listed
    // in reverse dependency order.
    let p = Project::new(
        r#"
[workflows.ci.settings]
parallelism = 1
fail_fast = true
[[workflows.ci.jobs]]
name = "prepare"
workflow = "noop"
condition = "sleep 0.3"
[[workflows.ci.jobs]]
name = "other"
command = "touch other"
depends_on = ["prepare"]
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
[[workflows.ci.jobs]]
name = "publish"
command = "touch published"
depends_on = ["integration"]
[[workflows.ci.jobs]]
name = "announce"
command = "touch announced"
depends_on = ["publish"]
[[workflows.ci.jobs]]
name = "p4"
command = "touch p4"
depends_on = ["p3"]
[[workflows.ci.jobs]]
name = "p3"
command = "touch p3"
depends_on = ["p2"]
[[workflows.ci.jobs]]
name = "p2"
command = "touch p2"
depends_on = ["p1"]
[[workflows.ci.jobs]]
name = "p1"
command = "touch p1"
depends_on = ["integration"]
[[workflows.suite.jobs]]
name = "test"
command = "sleep 0.5; exit 1"
[[workflows.noop.jobs]]
name = "never"
workflow = "suite"
condition = { expression = "false" }
"#,
    );
    let output = p.run(&["run", "ci", "--json", "--no-history"]);
    assert_eq!(output.status.code(), Some(1));
    let summaries = summaries(&output);
    let root = summaries.last().unwrap();
    assert_eq!(root["workflow"], "ci");
    assert_eq!(status(root, "prepare"), pair("passed", ""));
    assert_eq!(
        status(root, "integration"),
        pair("failed", "workflow 'suite' failed")
    );
    for dependent in ["publish", "announce", "p4", "p3", "p2", "p1"] {
        assert_eq!(
            status(root, dependent),
            pair("skipped", "dependency did not pass"),
            "{dependent}"
        );
    }
    // Fail-fast still reaches the root and stops its unrelated pending work.
    assert_eq!(
        status(root, "other"),
        pair("skipped", "fail_fast: previous job failed")
    );
    for absent in ["published", "announced", "other", "p4", "p3", "p2", "p1"] {
        assert!(!p.dir.join(absent).exists(), "{absent}");
    }

    let output = p.run(&["run", "ci", "--no-history"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).ends_with(
            "Rerun failed: goodflows run ci --jobs='integration,publish,announce,p4,p3,p2,p1'\n"
        ),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_root_failure_during_a_call_condition_activates_no_child() {
    // The call's shell condition passes only once `boom-done` exists. The
    // test creates it when it reads `boom`'s `job_finished` event, which the
    // scheduler emits only after recording the failure, so the root is
    // stopped by the time the condition passes.
    let p = Project::new(
        r#"
[workflows.ci.settings]
parallelism = 2
fail_fast = true
[[workflows.ci.jobs]]
name = "boom"
command = "exit 1"
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
condition = "i=0; while [ $i -lt 500 ] && ! test -f boom-done; do sleep 0.01; i=$((i+1)); done; test -f boom-done"
[[workflows.suite.jobs]]
name = "test"
command = "touch ran"
"#,
    );
    let mut child = p
        .command()
        .args(["run", "ci", "--json", "--no-history"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut events = Vec::new();
    for line in std::io::BufRead::lines(std::io::BufReader::new(child.stdout.take().unwrap())) {
        let parsed = line
            .map_err(|e| e.to_string())
            .and_then(|line| serde_json::from_str(&line).map_err(|e| format!("{e}: {line}")));
        let event: serde_json::Value = match parsed {
            Ok(event) => event,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("bad event line: {error}");
            }
        };
        if event["type"] == "job_finished" && event["result"]["name"] == "boom" {
            std::fs::write(p.dir.join("boom-done"), "").unwrap();
        }
        events.push(event);
    }
    assert_eq!(child.wait().unwrap().code(), Some(1));
    // No child run started: no child events, summary, or job.
    assert!(events.iter().all(|e| e["workflow"] == "ci"), "{events:?}");
    let summaries: Vec<_> = events.iter().filter(|e| e["type"] == "summary").collect();
    assert_eq!(summaries.len(), 1);
    let root = summaries[0];
    assert_eq!(status(root, "boom"), pair("failed", "exited with 1"));
    assert_eq!(
        status(root, "integration"),
        pair("skipped", "fail_fast: previous job failed")
    );
    let call = root["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "integration")
        .unwrap();
    assert!(call.get("child_run_id").is_none(), "{call}");
    assert!(!p.dir.join("ran").exists());
}

/// Waits up to five seconds for the file `$1` in a job command.
const AWAIT: &str = "i=0; while [ $i -lt 500 ] && ! test -f";

/// Runs goodflows and creates `marker` as soon as a stdout line satisfies
/// `trigger`, so jobs waiting on that file run only after the event that
/// line reports. Returns the exit code and the stdout lines.
fn run_with_marker(
    p: &Project,
    args: &[&str],
    marker: &str,
    trigger: impl Fn(&str) -> bool,
) -> (Option<i32>, Vec<String>) {
    let mut child = p
        .command()
        .args(args)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = Vec::new();
    for line in std::io::BufRead::lines(std::io::BufReader::new(child.stdout.take().unwrap())) {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("unreadable output: {error}");
            }
        };
        if trigger(&line) {
            if let Err(error) = std::fs::write(p.dir.join(marker), "") {
                let _ = child.kill();
                let _ = child.wait();
                panic!("cannot write {marker}: {error}");
            }
        }
        lines.push(line);
    }
    (child.wait().unwrap().code(), lines)
}

/// Whether a JSON line is the `job_finished` event of the root job `name`,
/// which the scheduler emits only after recording its result.
fn finished_json(line: &str, workflow: &str, name: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|e| {
        e["type"] == "job_finished" && e["workflow"] == workflow && e["result"]["name"] == name
    })
}

fn summaries_of(lines: &[String]) -> Vec<serde_json::Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|e| e["type"] == "summary")
        .collect()
}

fn result_named<'a>(summary: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap()
}

#[test]
fn a_root_failure_mid_child_skips_the_call_with_the_fail_fast_reason() {
    // `first` runs until `boom`'s failure is recorded: the test creates
    // `boom-done` only after reading `boom`'s finished line. The child's
    // remaining work stops with the fail-fast reason, so the call is skipped
    // with that reason even though no child job failed.
    let p = Project::new(&format!(
        r#"
[workflows.ci.settings]
parallelism = 4
fail_fast = true
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
[[workflows.ci.jobs]]
name = "publish"
command = "touch published"
depends_on = ["integration"]
[[workflows.ci.jobs]]
name = "boom"
command = "{AWAIT} first-started; do sleep 0.01; i=$((i+1)); done; exit 1"
[[workflows.suite.jobs]]
name = "first"
command = "touch first-started; {AWAIT} boom-done; do sleep 0.01; i=$((i+1)); done"
[[workflows.suite.jobs]]
name = "second"
command = "touch second"
depends_on = ["first"]
"#
    ));
    let (code, lines) = run_with_marker(
        &p,
        &["run", "ci", "--json", "--no-history"],
        "boom-done",
        |line| finished_json(line, "ci", "boom"),
    );
    assert_eq!(code, Some(1));
    let summaries = summaries_of(&lines);
    let (child, root) = (workflow(&summaries, "suite"), summaries.last().unwrap());
    assert_eq!(root["workflow"], "ci");
    assert_eq!(status(child, "first"), pair("passed", ""));
    assert_eq!(
        status(child, "second"),
        pair("skipped", "fail_fast: previous job failed")
    );
    // The child's own outcome still follows the existing rule.
    assert_eq!(child["outcome"], "passed");
    assert_eq!(
        status(root, "integration"),
        pair("skipped", "fail_fast: previous job failed")
    );
    let call = result_named(root, "integration");
    assert_eq!(call["child_run_id"], child["run_id"]);
    assert_eq!(call["workflow"], "suite");
    assert!(call["duration_ms"].is_u64(), "{call}");
    assert_eq!(
        status(root, "publish"),
        pair("skipped", "dependency did not pass")
    );
    assert_eq!(status(root, "boom"), pair("failed", "exited with 1"));
    assert_eq!(root["outcome"], "failed");
    assert!(!p.dir.join("second").exists() && !p.dir.join("published").exists());

    // The text hint reruns the failed job and the call's dependent, which
    // brings the call back in as its prerequisite.
    for marker in ["first-started", "boom-done"] {
        std::fs::remove_file(p.dir.join(marker)).unwrap();
    }
    let (code, lines) = run_with_marker(&p, &["run", "ci", "--no-history"], "boom-done", |line| {
        line.contains("] boom FAILED")
    });
    assert_eq!(code, Some(1));
    let text = lines.join("\n");
    assert!(
        text.contains("  SKIP  integration")
            && text.ends_with("Rerun failed: goodflows run ci --jobs='publish,boom'"),
        "{text}"
    );
}

#[test]
fn the_rerun_hint_omits_an_interrupted_call_without_dependents() {
    // Like a plain job skipped by fail-fast, an interrupted call is not a
    // failure the hint reruns; selecting the call reruns its workflow.
    let p = Project::new(&format!(
        r#"
[workflows.ci.settings]
parallelism = 4
fail_fast = true
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
[[workflows.ci.jobs]]
name = "boom"
command = "{AWAIT} first-started; do sleep 0.01; i=$((i+1)); done; exit 1"
[[workflows.suite.jobs]]
name = "first"
command = "touch first-started; {AWAIT} boom-done; do sleep 0.01; i=$((i+1)); done"
[[workflows.suite.jobs]]
name = "second"
command = "touch second"
depends_on = ["first"]
"#
    ));
    let (code, lines) = run_with_marker(&p, &["run", "ci", "--no-history"], "boom-done", |line| {
        line.contains("] boom FAILED")
    });
    assert_eq!(code, Some(1));
    let text = lines.join("\n");
    assert!(
        text.contains("  SKIP  integration")
            && text.ends_with("Rerun failed: goodflows run ci --jobs=boom"),
        "{text}"
    );
    assert!(!p.dir.join("second").exists());
}

#[test]
fn a_child_that_completed_before_the_root_failure_passes() {
    // `boom` fails only after the child's summary, so every child job had
    // finished: one passed, one skipped by a false condition and one skipped
    // because of that dependency.
    let p = Project::new(&format!(
        r#"
[workflows.ci.settings]
parallelism = 4
fail_fast = true
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
[[workflows.ci.jobs]]
name = "boom"
command = "{AWAIT} child-done; do sleep 0.01; i=$((i+1)); done; exit 1"
[[workflows.suite.jobs]]
name = "test"
command = "true"
[[workflows.suite.jobs]]
name = "optional"
condition = {{ expression = "false" }}
command = "touch optional"
[[workflows.suite.jobs]]
name = "after-optional"
command = "touch after-optional"
depends_on = ["optional"]
"#
    ));
    let (code, lines) = run_with_marker(
        &p,
        &["run", "ci", "--json", "--no-history"],
        "child-done",
        |line| {
            serde_json::from_str::<serde_json::Value>(line)
                .is_ok_and(|e| e["type"] == "summary" && e["workflow"] == "suite")
        },
    );
    assert_eq!(code, Some(1));
    let summaries = summaries_of(&lines);
    let (child, root) = (workflow(&summaries, "suite"), summaries.last().unwrap());
    assert_eq!(status(child, "test"), pair("passed", ""));
    assert_eq!(status(child, "optional").0, "skipped");
    assert_eq!(
        status(child, "after-optional"),
        pair("skipped", "dependency did not pass")
    );
    assert_eq!(child["outcome"], "passed");
    assert_eq!(status(root, "integration"), pair("passed", ""));
    assert_eq!(status(root, "boom"), pair("failed", "exited with 1"));
    assert_eq!(root["outcome"], "failed");
}

#[test]
fn a_call_stopped_at_its_condition_inside_a_child_interrupts_the_caller() {
    // root `ci` -> `mid` -> `to-leaf`. `to-leaf`'s shell condition passes
    // only after the test reads `boom`'s finished line, so `mid`'s call is
    // skipped by its stop check, which interrupts `mid` and skips `to-mid`.
    let p = Project::new(&format!(
        r#"
[workflows.ci.settings]
parallelism = 4
fail_fast = true
[[workflows.ci.jobs]]
name = "to-mid"
workflow = "mid"
[[workflows.ci.jobs]]
name = "boom"
command = "{AWAIT} mid-started; do sleep 0.01; i=$((i+1)); done; exit 1"
[[workflows.mid.jobs]]
name = "to-leaf"
workflow = "leaf"
condition = "touch mid-started; {AWAIT} boom-done; do sleep 0.01; i=$((i+1)); done; test -f boom-done"
[[workflows.leaf.jobs]]
name = "run"
command = "touch leaf-ran"
"#
    ));
    let (code, lines) = run_with_marker(
        &p,
        &["run", "ci", "--json", "--no-history"],
        "boom-done",
        |line| finished_json(line, "ci", "boom"),
    );
    assert_eq!(code, Some(1));
    let summaries = summaries_of(&lines);
    let (mid, root) = (workflow(&summaries, "mid"), summaries.last().unwrap());
    let fail_fast = pair("skipped", "fail_fast: previous job failed");
    assert_eq!(status(mid, "to-leaf"), fail_fast);
    assert!(result_named(mid, "to-leaf").get("child_run_id").is_none());
    assert_eq!(mid["outcome"], "passed");
    assert_eq!(status(root, "to-mid"), fail_fast);
    assert_eq!(result_named(root, "to-mid")["child_run_id"], mid["run_id"]);
    assert_eq!(status(root, "boom"), pair("failed", "exited with 1"));
    assert_eq!(root["outcome"], "failed");
    assert!(summaries.iter().all(|s| s["workflow"] != "leaf"));
    assert!(!p.dir.join("leaf-ran").exists());
}

#[test]
fn an_interrupted_grandchild_skips_both_calls_with_the_fail_fast_reason() {
    // root `ci` -> `mid` -> `leaf`. The root's `boom` fails while `leaf`'s
    // `first` runs; `first` finishes only after that failure is recorded.
    let p = Project::new(&format!(
        r#"
[workflows.ci.settings]
parallelism = 4
fail_fast = true
[[workflows.ci.jobs]]
name = "to-mid"
workflow = "mid"
[[workflows.ci.jobs]]
name = "boom"
command = "{AWAIT} first-started; do sleep 0.01; i=$((i+1)); done; exit 1"
[[workflows.mid.jobs]]
name = "to-leaf"
workflow = "leaf"
[[workflows.mid.jobs]]
name = "after-leaf"
command = "touch after-leaf"
depends_on = ["to-leaf"]
[[workflows.leaf.jobs]]
name = "first"
command = "touch first-started; {AWAIT} boom-done; do sleep 0.01; i=$((i+1)); done"
[[workflows.leaf.jobs]]
name = "second"
command = "touch second"
depends_on = ["first"]
"#
    ));
    let (code, lines) = run_with_marker(
        &p,
        &["run", "ci", "--json", "--no-history"],
        "boom-done",
        |line| finished_json(line, "ci", "boom"),
    );
    assert_eq!(code, Some(1));
    let summaries = summaries_of(&lines);
    let (leaf, mid, root) = (
        workflow(&summaries, "leaf"),
        workflow(&summaries, "mid"),
        summaries.last().unwrap(),
    );
    let fail_fast = pair("skipped", "fail_fast: previous job failed");
    assert_eq!(status(leaf, "first"), pair("passed", ""));
    assert_eq!(status(leaf, "second"), fail_fast);
    assert_eq!(leaf["outcome"], "passed");
    assert_eq!(status(mid, "to-leaf"), fail_fast);
    assert_eq!(result_named(mid, "to-leaf")["child_run_id"], leaf["run_id"]);
    assert_eq!(
        status(mid, "after-leaf"),
        pair("skipped", "dependency did not pass")
    );
    assert_eq!(mid["outcome"], "passed");
    assert_eq!(status(root, "to-mid"), fail_fast);
    assert_eq!(result_named(root, "to-mid")["child_run_id"], mid["run_id"]);
    assert_eq!(status(root, "boom"), pair("failed", "exited with 1"));
    assert_eq!(root["outcome"], "failed");
    assert!(!p.dir.join("second").exists() && !p.dir.join("after-leaf").exists());
}
