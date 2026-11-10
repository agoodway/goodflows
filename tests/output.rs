mod common;
use common::*;

fn events(output: &std::process::Output) -> Vec<serde_json::Value> {
    stdout(output)
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(event["schema_version"], 1);
            event
        })
        .collect()
}

#[test]
fn output_is_prefixed_and_optional_logs_preserve_raw_streams() {
    let p = Project::new("[[jobs]]\nname='../unsafe/name'\ncommand=\"printf 'hello\\\\nlast'; printf 'error\\\\n' >&2\"");
    let output = p.run(&["--log-dir", "logs"]);
    success(&output);
    assert!(stdout(&output).contains("[../unsafe/name stdout] hello"));
    assert!(stdout(&output).contains("[../unsafe/name stderr] error"));
    let run = run_dir(&p.dir.join("logs"));
    let files: Vec<_> = log_names(&run).iter().map(|f| run.join(f)).collect();
    assert_eq!(files.len(), 2);
    let out = files
        .iter()
        .find(|f| f.to_string_lossy().ends_with(".stdout.log"))
        .unwrap();
    let err = files
        .iter()
        .find(|f| f.to_string_lossy().ends_with(".stderr.log"))
        .unwrap();
    assert_eq!(std::fs::read(out).unwrap(), b"hello\nlast");
    assert_eq!(std::fs::read(err).unwrap(), b"error\n");
}

#[test]
fn json_is_clean_for_output_failed_exit_codes_and_summary() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand=\"printf 'hi'; exit 9\"");
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let events = events(&output);
    assert!(events
        .iter()
        .any(|e| e["type"] == "job_output" && e["data"] == "hi"));
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["results"][0]["status"], "failed");
    assert_eq!(summary["results"][0]["exit_code"], 9);
    assert_eq!(summary["failed"], 1);
}

#[test]
fn json_works_for_validation_list_and_errors() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='true'");
    for (args, kind) in [
        (vec!["validate", "--json"], "config_valid"),
        (vec!["list", "--json"], "jobs"),
        (vec!["--jobs", "missing", "--json"], "error"),
        (vec!["--json", "--unknown"], "error"),
    ] {
        let output = p.run(&args);
        assert_eq!(events(&output)[0]["type"], kind, "{}", stderr(&output));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn non_utf8_and_large_partial_lines_are_streamed_without_corrupting_logs() {
    let p = Project::new("[[jobs]]\nname='bytes'\ncommand=\"printf '\\\\377'; head -c 100000 /dev/zero | tr '\\\\000' x\"");
    let output = p.run(&["--json", "--log-dir", "logs"]);
    success(&output);
    let events = events(&output);
    let data: String = events
        .iter()
        .filter(|e| e["type"] == "job_output")
        .map(|e| e["data"].as_str().unwrap())
        .collect();
    assert!(data.starts_with('\u{fffd}'));
    assert_eq!(data.chars().count(), 100001);
    let raw = std::fs::read(run_dir(&p.dir.join("logs")).join("001-bytes.stdout.log")).unwrap();
    assert_eq!(raw.len(), 100001);
    assert_eq!(raw[0], 255);
}

#[test]
fn invalid_byte_does_not_corrupt_a_later_split_utf8_character() {
    let p = Project::new(
        r#"[[jobs]]
name = "bytes"
command = """printf '\\377'; head -c 8190 /dev/zero | tr '\\000' x; printf '\\342'; sleep 0.1; printf '\\202\\254'"""
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    let events = events(&output);
    let data: String = events
        .iter()
        .filter(|e| e["type"] == "job_output")
        .map(|e| e["data"].as_str().unwrap())
        .collect();
    assert_eq!(data, format!("\u{fffd}{}€", "x".repeat(8190)));
}

#[cfg(target_os = "linux")]
#[test]
fn log_write_failure_stops_the_process_without_waiting_for_its_command() {
    use std::time::{Duration, Instant};
    // Log files are created fresh and never follow a symlink, so the write
    // error comes from a file size limit: with SIGXFSZ ignored, a write past
    // `ulimit -f` fails with EFBIG. Pipes are not limited, so only the log
    // write fails.
    let p = Project::new(
        "[[jobs]]\nname='io'\ncommand='head -c 200000 /dev/zero; sleep 3; touch escaped'",
    );
    let start = Instant::now();
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f 50; exec \"$0\" --log-dir logs --json",
            env!("CARGO_BIN_EXE_goodflows"),
        ])
        .current_dir(&p.dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "did not stop after log write failure"
    );
    assert!(!p.dir.join("escaped").exists());
    let events = events(&output);
    let summary = events.last().unwrap();
    assert_eq!(summary["results"][0]["status"], "failed");
    assert!(summary["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("output"));
}

#[test]
fn log_files_are_private_and_never_follow_a_planted_symlink() {
    use std::os::unix::fs::PermissionsExt;
    // A first job plants a symlink where the second job's stdout log goes.
    let p = Project::new(
        "[settings]\nfail_fast = false\n\
         [[jobs]]\nname='seed'\ncommand='for d in logs/*/; do ln -s ../../target \"${d}002-io.stdout.log\"; done'\n\
         [[jobs]]\nname='io'\ndepends_on=['seed']\ncommand='echo written; touch ran'",
    );
    std::fs::write(p.dir.join("target"), "untouched").unwrap();
    let output = p.run(&["--log-dir", "logs", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary = events(&output).pop().unwrap();
    assert_eq!(summary["results"][0]["status"], "passed");
    assert_eq!(summary["results"][1]["status"], "failed");
    let error = summary["results"][1]["error"].as_str().unwrap();
    assert!(error.contains("failed to create log"), "{error}");
    assert!(!p.dir.join("ran").exists());
    assert_eq!(p.read("target"), "untouched");
    // The run directory and the logs it holds are private to the user.
    let run = run_dir(&p.dir.join("logs"));
    let mode = |path: &std::path::Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&run.join("001-seed.stdout.log")), 0o600);
    assert_eq!(mode(&run.join("001-seed.stderr.log")), 0o600);
}

#[test]
fn failed_results_include_command_and_separate_bounded_output_tails() {
    let p = Project::new(
        r#"[[jobs]]
name = "fail"
command = """printf 'EARLY'; head -c 10000 /dev/zero | tr '\\000' x; printf 'OUT-END'; head -c 10000 /dev/zero | tr '\\000' y >&2; printf 'ERR-END' >&2; exit 7"""
"#,
    );
    let output = p.run(&["--json", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let result = &events.last().unwrap()["results"][0];
    assert!(result["command"].as_str().unwrap().contains("exit 7"));
    assert_eq!(
        result["stdout_tail"].as_str().unwrap(),
        format!("{}OUT-END", "x".repeat(4089))
    );
    assert_eq!(
        result["stderr_tail"].as_str().unwrap(),
        format!("{}ERR-END", "y".repeat(4089))
    );
    assert_eq!(result["stdout_truncated"], true);
    assert_eq!(result["stderr_truncated"], true);
    let finished = events.iter().find(|e| e["type"] == "job_finished").unwrap();
    // The summary result adds only `started` and the optimizer fields; the
    // live event never carries them.
    let mut projected = result.clone();
    let projected_fields = projected.as_object_mut().unwrap();
    assert_eq!(projected_fields.remove("started"), Some(true.into()));
    for field in ["blocked_ms", "queued_ms", "commands"] {
        assert!(
            projected_fields.remove(field).is_some(),
            "{field}: {result}"
        );
    }
    assert_eq!(finished["result"], projected);
    assert!(finished["result"].get("stdout_tail").is_some());
    assert!(finished["result"].get("stderr_tail").is_some());
    assert!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs")).join("001-fail.stdout.log"))
            .unwrap()
            .starts_with("EARLY")
    );
    let full: String = events
        .iter()
        .filter(|e| e["type"] == "job_output" && e["stream"] == "stdout")
        .map(|e| e["data"].as_str().unwrap())
        .collect();
    assert_eq!(full.len(), 10012);
    assert!(full.starts_with("EARLY"));
    let output = p.run(&[]);
    let text = stdout(&output);
    let header = text.find("\ndefault  FAILED  in ").expect("report header");
    let failures = text.find("\nFailures\n").unwrap();
    assert!(header < failures, "{text}");
    // The live job_finished block keeps only the status and error lines.
    let live = text.find("fail FAILED").unwrap();
    assert!(text[live..header].starts_with("fail FAILED"), "{text}");
    assert_eq!(
        &text[live..header].lines().nth(1).unwrap(),
        &"  exited with 7"
    );
    assert!(!text[..header].contains("  command: "), "{text}");
    assert_eq!(text[failures..].matches("  command: ").count(), 1, "{text}");
    // Output tails are JSON-only; the text report never repeats output.
    assert!(!text[header..].contains(" tail"), "{text}");
    for tail in ["OUT-END\n", "ERR-END\n"] {
        assert!(!text[failures..].contains(tail), "{text}");
    }
}

#[test]
fn commands_are_in_all_results_and_timeout_tails_keep_short_output() {
    let p = Project::new(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "fail"
command = "exit 1"
[[jobs]]
name = "skip"
command = "touch skipped"
depends_on = ["fail"]
[[jobs]]
name = "pass"
command = "true"
[[jobs]]
name = "timeout"
command = "printf before; printf error >&2; sleep 5"
timeout_secs = 1
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let results = events.last().unwrap()["results"].as_array().unwrap();
    for (result, command) in results.iter().zip([
        "exit 1",
        "touch skipped",
        "true",
        "printf before; printf error >&2; sleep 5",
    ]) {
        assert_eq!(result["command"], command);
    }
    assert_eq!(results[0]["stdout_tail"], "");
    assert_eq!(results[0]["stdout_truncated"], false);
    for index in [1, 2] {
        assert!(results[index].get("stdout_tail").is_none());
    }
    assert_eq!(results[3]["status"], "timed_out");
    assert_eq!(results[3]["stdout_tail"], "before");
    assert_eq!(results[3]["stderr_tail"], "error");
    assert_eq!(results[3]["stdout_truncated"], false);
    assert_eq!(results[3]["stderr_truncated"], false);
}

#[test]
fn tails_bound_lossy_utf8_bytes_without_cutting_valid_characters() {
    // macOS tr is locale-aware; force byte translation rather than U+00FF encoding.
    let p = Project::new(
        r#"[[jobs]]
name = "bytes"
command = """head -c 6000 /dev/zero | LC_ALL=C tr '\\000' '\\377'; i=0; while [ $i -lt 2000 ]; do printf '€' >&2; i=$((i+1)); done; exit 1"""
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let result = &events.last().unwrap()["results"][0];
    for key in ["stdout_tail", "stderr_tail"] {
        assert!(result[key].as_str().unwrap().len() <= 4096);
    }
    assert_eq!(
        result["stdout_tail"].as_str().unwrap(),
        "\u{fffd}".repeat(1365)
    );
    assert_eq!(result["stderr_tail"].as_str().unwrap(), "€".repeat(1365));
    assert_eq!(result["stdout_truncated"], true);
    assert_eq!(result["stderr_truncated"], true);
}

const PARTITIONED: &str = r#"
[settings]
parallelism = 4
[[jobs]]
name = "compile"
command = "echo compiled"
[[jobs]]
name = "test"
depends_on = ["compile"]
partitions = 3
command = "echo $GOODFLOWS_PARTITION/$GOODFLOWS_PARTITION_COUNT"
[[jobs]]
name = "stepped"
partitions = 2
[[jobs.steps]]
id = "unit"
command = "echo step $GOODFLOWS_PARTITION"
[[jobs]]
name = "plainsteps"
[[jobs.steps]]
id = "only"
command = "echo plain step"
"#;

#[test]
fn json_events_identify_each_instance_and_leave_unpartitioned_events_unchanged() {
    let p = Project::new(PARTITIONED);
    let output = p.run(&["--json"]);
    success(&output);
    // `events` asserts every line is a valid versioned event.
    let events = events(&output);
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[0]["jobs"], 7);
    for event in &events {
        let (name, partition) = match event["type"].as_str().unwrap() {
            "job_started" | "step_started" | "job_output" | "step_finished" => {
                (&event["name"], &event["partition"])
            }
            "job_finished" => (&event["result"]["name"], &event["result"]["partition"]),
            _ => continue,
        };
        match name.as_str().unwrap() {
            "compile" | "plainsteps" => {
                assert!(event.get("partition").is_none(), "{event}");
                assert!(event["result"].get("partition").is_none(), "{event}");
                if name == "plainsteps" && event["type"] == "job_output" {
                    assert_eq!(event["step"], "only");
                    assert_eq!(event["data"], "plain step\n");
                }
            }
            "test" => {
                let index = partition["index"].as_u64().unwrap();
                assert!((1..=3).contains(&index), "{event}");
                assert_eq!(partition["count"], 3, "{event}");
                assert_eq!(partition.as_object().unwrap().len(), 2, "{event}");
                if event["type"] == "job_output" {
                    assert_eq!(event["data"], format!("{index}/3\n"));
                }
            }
            "stepped" => {
                let index = partition["index"].as_u64().unwrap();
                assert_eq!(partition["count"], 2, "{event}");
                if event["type"] == "job_output" {
                    assert_eq!(event["step"], "unit");
                    assert_eq!(event["data"], format!("step {index}\n"));
                }
            }
            other => panic!("unexpected job {other}"),
        }
    }
    for kind in ["job_started", "job_finished"] {
        assert_eq!(events.iter().filter(|e| e["type"] == kind).count(), 7);
    }
    for kind in ["step_started", "step_finished", "job_output"] {
        assert_eq!(
            events
                .iter()
                .filter(|e| e["type"] == kind && e["name"] == "plainsteps")
                .count(),
            1,
            "{kind}"
        );
    }
    for kind in ["step_started", "step_finished"] {
        assert_eq!(events.iter().filter(|e| e["type"] == kind).count(), 3);
    }
    let finished = events
        .iter()
        .find(|e| {
            e["type"] == "job_finished"
                && e["result"]["partition"]["index"] == 2
                && e["result"]["name"] == "test"
        })
        .unwrap();
    assert_eq!(
        finished["result"]["partition"],
        serde_json::json!({"index": 2, "count": 3})
    );
    // Jobs start concurrently, so find each event by name rather than position.
    let find = |kind: &str, name: &str| {
        events
            .iter()
            .find(|e| e["type"] == kind && e["name"] == name)
            .unwrap_or_else(|| panic!("no {kind} for {name}"))
    };
    let keys = |event: &serde_json::Value| {
        event
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(find("job_started", "compile")),
        ["name", "schema_version", "type", "workflow"]
    );
    assert_eq!(
        keys(find("step_started", "plainsteps")),
        ["name", "schema_version", "step", "type", "workflow"]
    );
    assert_eq!(
        keys(find("step_finished", "plainsteps")),
        ["name", "result", "schema_version", "type", "workflow"]
    );
    assert_eq!(
        keys(find("job_output", "plainsteps")),
        [
            "data",
            "name",
            "schema_version",
            "step",
            "stream",
            "type",
            "workflow"
        ]
    );
}

#[test]
fn steps_skipped_after_a_failed_step_keep_the_instance_identity() {
    let p = Project::new(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "test"
partitions = 2
[[jobs.steps]]
id = "a"
command = "test $GOODFLOWS_PARTITION != 2"
[[jobs.steps]]
id = "b"
command = "true"
[[jobs.steps]]
id = "c"
command = "true"
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let finished: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "step_finished" && e["partition"]["index"] == 2)
        .collect();
    assert_eq!(finished.len(), 3, "{finished:?}");
    assert_eq!(finished[0]["result"]["status"], "failed");
    for (event, step) in finished[1..].iter().zip(["b", "c"]) {
        assert_eq!(event["result"]["id"], step);
        assert_eq!(event["result"]["status"], "skipped");
        assert_eq!(event["result"]["error"], "previous step did not pass");
        assert_eq!(
            event["partition"],
            serde_json::json!({"index": 2, "count": 2})
        );
    }
    // Skipped steps emit no step_started.
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "step_started" && e["partition"]["index"] == 2)
            .count(),
        1
    );
    let summary = events.last().unwrap();
    assert_eq!(summary["results"][0]["status"], "passed");
    assert_eq!(summary["results"][1]["status"], "failed");
    assert_eq!(summary["results"][1]["error"], "step 'a': exited with 1");
}

#[test]
fn text_output_names_instances_and_the_summary_counts_them() {
    let p = Project::new(
        "[[jobs]]\nname='compile'\ncommand='true'\n[[jobs]]\nname='test'\npartitions=3\ndepends_on=['compile']\ncommand='echo $GOODFLOWS_PARTITION'\n",
    );
    let output = p.run(&[]);
    success(&output);
    let text = stdout(&output);
    assert!(text.contains("jobs=4"), "{text}");
    for index in 1..=3 {
        for line in [
            format!("→ test ({index}/3)\n"),
            format!("[test ({index}/3) stdout] {index}\n"),
            format!("test ({index}/3) PASSED"),
        ] {
            assert!(text.contains(&line), "missing {line:?}: {text}");
        }
    }
    assert!(text.contains("→ compile\n") && text.contains("compile PASSED"));
    // The report counts configured jobs: one row each for compile and test.
    let report = &text[text.find("\ndefault  PASSED  in ").expect("report header")..];
    let rows: Vec<_> = report.lines().filter(|l| l.starts_with("  ")).collect();
    assert_eq!(rows.len(), 2, "{report}");
    assert!(rows[0].starts_with("  PASS  compile "), "{report}");
    assert!(rows[1].starts_with("  PASS  test "), "{report}");
    assert!(!rows[1].contains("/3"), "{report}");
    assert!(report.contains("\n2 passed\n"), "{report}");
    assert!(!text.contains("Summary:") && !text.contains("All jobs passed"));

    let p = Project::new(
        "[settings]\nfail_fast=false\n[[jobs]]\nname='test'\npartitions=3\ncommand='if [ $GOODFLOWS_PARTITION = 2 ]; then echo boom-$GOODFLOWS_PARTITION >&2; exit 3; fi'\n",
    );
    let output = p.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    for line in [
        "test (2/3) FAILED",
        "test (1/3) PASSED",
        "test (3/3) PASSED",
    ] {
        assert!(text.contains(line), "missing {line:?}: {text}");
    }
    let header = text.find("\ndefault  FAILED  in ").expect("report header");
    // Neither the live failure block nor the report repeats output tails.
    assert!(!text.contains("stderr tail"), "{text}");
    let report = &text[header..];
    let rows: Vec<_> = report.lines().filter(|l| l.starts_with("  ")).collect();
    assert!(rows[0].starts_with("  FAIL  test "), "{report}");
    assert!(rows[0].ends_with("  1/3 failed"), "{report}");
    assert!(report.contains("\n1 failed\n"), "{report}");
    let entry = &report[report.find("\nFailures\n").unwrap()..];
    assert!(
        entry.starts_with("\nFailures\n--- test (2/3)  FAILED  exit 3\n"),
        "{report}"
    );
    assert!(!entry.contains("boom-2"), "{report}");
    assert!(!entry.contains("test (1/3)") && !entry.contains("test (3/3)"));
    // The report is the last stdout text and ends with the rerun hint.
    assert!(
        text.ends_with("\nRerun failed: goodflows run default --jobs=test\n"),
        "{text}"
    );

    let p = Project::new(PARTITIONED);
    let text = stdout(&p.run(&["--jobs", "stepped"]));
    for index in 1..=2 {
        for line in [
            format!("  → stepped ({index}/2)/unit\n"),
            format!("[stepped ({index}/2)/unit stdout] step {index}\n"),
            format!("  stepped ({index}/2)/unit PASSED"),
        ] {
            assert!(text.contains(&line), "missing {line:?}: {text}");
        }
    }
}

#[test]
fn json_summary_carries_duration_log_dir_and_instance_counts() {
    let config = "[[jobs]]\nname='compile'\ncommand='sleep 0.2'\n[[jobs]]\nname='test'\npartitions=3\ndepends_on=['compile']\ncommand='true'\n";
    let legacy = Project::new(config);
    let named = Project::new(&format!(
        "[workflows.ci]\n{}",
        config.replace("[[jobs]]", "[[workflows.ci.jobs]]")
    ));
    for (p, args, log_dir) in [
        (&legacy, vec!["--json", "--log-dir", "logs"], Some("logs")),
        (
            &named,
            vec!["run", "ci", "--json", "--log-dir", "logs"],
            Some("logs/ci"),
        ),
        (&legacy, vec!["--json"], None),
    ] {
        let output = p.run(&args);
        success(&output);
        let events = events(&output);
        assert_eq!(events[0]["jobs"], 4);
        let summary = events.last().unwrap();
        assert_eq!(summary["type"], "summary");
        // Wall time covers the whole run, so at least the longest job.
        let wall = summary["duration_ms"].as_u64().unwrap();
        let longest = summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["duration_ms"].as_u64().unwrap())
            .max()
            .unwrap();
        assert!(wall >= 200 && wall >= longest, "{summary}");
        assert_eq!(summary["passed"], 4);
        assert_eq!(summary["results"].as_array().unwrap().len(), 4);
        match log_dir {
            Some(dir) => {
                let run = run_dirs(&p.dir.join(dir)).pop().unwrap();
                let run_name = run.file_name().unwrap().to_str().unwrap();
                assert_eq!(summary["log_dir"], format!("{dir}/{run_name}"));
                assert!(run_name.ends_with(summary["run_id"].as_str().unwrap()));
                assert!(run.join("001-compile.stdout.log").exists());
            }
            None => {
                assert!(summary.get("log_dir").is_none(), "{summary}");
                // No new log run directory beside the earlier `--log-dir` run.
                assert_eq!(run_dirs(&p.dir.join("logs")).len(), 1);
            }
        }
        // Optimizer fields are present with history off; history_file is not.
        assert!(summary.get("history_file").is_none(), "{summary}");
        assert_eq!(summary["outcome"], "passed");
        assert_eq!(summary["parallelism"], events[0]["parallelism"]);
        assert!(summary["max_in_flight"].as_u64().unwrap() >= 1);
        assert_eq!(summary["graph_sha256"].as_str().unwrap().len(), 64);
        assert!(summary["host"]["logical_cpus"].as_u64().unwrap() >= 1);
        assert!(summary["started_at"].as_str().unwrap().ends_with('Z'));
        for result in summary["results"].as_array().unwrap() {
            assert!(result["blocked_ms"].is_u64(), "{result}");
            assert!(result["queued_ms"].is_u64(), "{result}");
            assert_eq!(result["commands"][0]["kind"], "command", "{result}");
        }
        // `test` waits for `compile` (sleep 0.2): that wait is blocked time.
        assert!(summary["results"][1]["blocked_ms"].as_u64().unwrap() >= 150);
        assert!(!p.dir.join("goodflows-history").exists());
        for event in events.iter().filter(|e| e["type"] == "job_finished") {
            for field in ["blocked_ms", "queued_ms", "commands", "started"] {
                assert!(event["result"].get(field).is_none(), "{event}");
            }
        }
    }
    // A second run gets its own directory and keeps the first.
    let output = named.run(&["run", "ci", "--json", "--log-dir", "logs"]);
    success(&output);
    let runs = run_dirs(&named.dir.join("logs/ci"));
    assert_eq!(runs.len(), 2);
    let summary = events(&output).pop().unwrap();
    let newest = runs.iter().find(|r| {
        summary["log_dir"]
            .as_str()
            .unwrap()
            .ends_with(r.file_name().unwrap().to_str().unwrap())
    });
    assert!(newest.is_some(), "{summary}");
}

#[test]
fn summary_results_mark_whether_each_instance_started() {
    let p = Project::new(
        r#"
[settings]
fail_fast = true
parallelism = 1
[[jobs]]
name = "ran"
command = "true"
[[jobs]]
name = "cond"
condition = "false"
command = "touch cond"
[[jobs]]
name = "boom"
command = "exit 1"
[[jobs]]
name = "dependent"
depends_on = ["boom"]
command = "true"
[[jobs]]
name = "late"
command = "true"
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let results = events.last().unwrap()["results"].as_array().unwrap();
    let started: Vec<_> = results
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["started"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        started,
        [
            ("ran", "passed", true),
            ("cond", "skipped", true),
            ("boom", "failed", true),
            ("dependent", "skipped", false),
            ("late", "skipped", false),
        ]
    );
    for event in events.iter().filter(|e| e["type"] == "job_finished") {
        assert!(event["result"].get("started").is_none(), "{event}");
    }
    // A started condition skip shows its duration; queued skips show none.
    let text = stdout(&p.run(&[]));
    let report = &text[text.find("\ndefault  FAILED  in ").unwrap()..];
    let row = |name: &str| {
        report
            .lines()
            .find(|l| l.split_whitespace().nth(1) == Some(name))
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let cond = row("cond");
    let tokens: Vec<_> = cond.split_whitespace().collect();
    assert_eq!(tokens[..2], ["SKIP", "cond"], "{report}");
    assert!(is_report_duration(tokens[2]), "{report}");
    assert_eq!(row("dependent"), "SKIP dependent dependency did not pass");
    assert_eq!(row("late"), "SKIP late fail_fast: previous job failed");
}

/// Runs a report's rerun hint through `sh`, with `goodflows` resolving to the
/// binary under test.
fn run_hint(p: &Project, hint: &str) -> std::process::Output {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_goodflows"))
        .parent()
        .unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    std::process::Command::new("sh")
        .current_dir(&p.dir)
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .args(["-c", hint])
        .output()
        .unwrap()
}

#[test]
fn rerun_hint_survives_the_shell_and_cli_parsing() {
    let p = Project::new(
        r#"
[environments."pr'od $(touch env-pwned) ;x".settings]
fail_fast = false

[workflows."-it's a $(touch wf-pwned) flow"]
[workflows."-it's a $(touch wf-pwned) flow".settings]
fail_fast = false
[[workflows."-it's a $(touch wf-pwned) flow".jobs]]
name = "base"
command = "true"
[[workflows."-it's a $(touch wf-pwned) flow".jobs]]
name = "other"
command = "true"
[[workflows."-it's a $(touch wf-pwned) flow".jobs]]
name = "a b's `touch job-pwned`"
depends_on = ["base"]
command = "echo ran-a >&2; exit 1"
[[workflows."-it's a $(touch wf-pwned) flow".jobs]]
name = "-x;y"
command = "exit 2"
"#,
    );
    let workflow = "-it's a $(touch wf-pwned) flow";
    let environment = "pr'od $(touch env-pwned) ;x";
    let output = p.run(&["run", "--env", environment, "--", workflow]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    let hint = text
        .lines()
        .last()
        .unwrap()
        .strip_prefix("Rerun failed: ")
        .unwrap_or_else(|| panic!("no rerun hint: {text}"));
    assert!(hint.contains(" -- "), "{hint}");
    let rerun = run_hint(&p, hint);
    assert_eq!(rerun.status.code(), Some(1), "{}", stderr(&rerun));
    let text = stdout(&rerun);
    assert!(
        text.contains(&format!("Environment: {environment}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nworkflow: {workflow}\n")),
        "{text}"
    );
    // Exactly the failed jobs plus their prerequisite were selected.
    assert!(text.contains("jobs=3"), "{text}");
    assert!(text.contains("  PASS  base"), "{text}");
    assert!(!text.contains("other"), "{text}");
    // The rerun failed the same way, so its own hint is identical.
    assert!(text.ends_with(&format!("Rerun failed: {hint}\n")), "{text}");
    for file in ["wf-pwned", "env-pwned", "job-pwned"] {
        assert!(!p.dir.join(file).exists(), "{file}");
    }
}

#[test]
fn comma_job_names_fall_back_to_a_full_workflow_rerun() {
    let p = Project::new(
        "[workflows.ci]\n[[workflows.ci.jobs]]\nname='test,unit'\ncommand='exit 1'\n[[workflows.ci.jobs]]\nname='ok'\ncommand='true'\n",
    );
    let output = p.run(&["run", "ci"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    assert!(
        text.ends_with("Selective rerun unavailable: job 'test,unit' contains a comma, which --jobs cannot select.\nRerun workflow: goodflows run ci\n"),
        "{text}"
    );
    let rerun = run_hint(&p, "goodflows run ci");
    assert!(stdout(&rerun).contains("  PASS  ok"));
}

/// Matches a report duration: `\d+ms`, `\d+.\ds` or `\d+m\d\ds`.
fn is_report_duration(token: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(ms) = token.strip_suffix("ms") {
        return digits(ms);
    }
    if let Some((minutes, seconds)) = token.strip_suffix('s').and_then(|t| t.split_once('m')) {
        return digits(minutes) && seconds.len() == 2 && digits(seconds);
    }
    match token.strip_suffix('s').and_then(|t| t.split_once('.')) {
        Some((whole, tenth)) => digits(whole) && tenth.len() == 1 && digits(tenth),
        None => false,
    }
}

#[test]
fn report_duration_tokens_are_recognized() {
    for token in ["0ms", "999ms", "1.2s", "1m05s"] {
        assert!(is_report_duration(token), "{token}");
    }
    for token in ["ms", "1.23s", "1m5s", "fail_fast:", "1s"] {
        assert!(!is_report_duration(token), "{token}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_log_dir_still_gets_a_summary() {
    use std::os::unix::ffi::OsStrExt;
    let p = Project::new("[[jobs]]\nname='a'\ncommand='echo hi'\n");
    let dir = std::ffi::OsStr::from_bytes(b"l\xff");
    let output = p
        .command()
        .arg("--json")
        .arg("--log-dir")
        .arg(dir)
        .output()
        .unwrap();
    success(&output);
    let summary = events(&output).pop().unwrap();
    assert_eq!(summary["type"], "summary");
    let run = run_dir(&p.dir.join(dir));
    let run_name = run.file_name().unwrap().to_str().unwrap().to_owned();
    assert_eq!(summary["log_dir"], format!("l\u{fffd}/{run_name}"));
    assert!(run.join("001-a.stdout.log").exists());
    let output = p.command().arg("--log-dir").arg(dir).output().unwrap();
    success(&output);
    let newest = run_dirs(&p.dir.join(dir))
        .into_iter()
        .find(|r| *r != run)
        .unwrap();
    let newest = newest.file_name().unwrap().to_str().unwrap();
    assert!(
        stdout(&output).ends_with(&format!("\n1 passed\n\nLogs: l\u{fffd}/{newest}\n")),
        "{}",
        stdout(&output)
    );
}

#[test]
fn timeout_live_line_is_short_and_the_report_has_no_tail() {
    let p = Project::new("[[jobs]]\nname='slow'\ntimeout_secs=1\ncommand='echo out; sleep 5'\n");
    let output = p.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    let header = text.find("\ndefault  FAILED  in ").expect("report header");
    let live = &text[text.find("slow TIMED OUT").expect("live line")..header];
    for absent in ["command:", "stdout tail", "stderr tail"] {
        assert!(!live.contains(absent), "{absent}: {text}");
    }
    let report = &text[header..];
    let entry = &report[report.find("\n--- slow  TIMED OUT").expect("entry")..];
    assert!(!entry.contains("tail"), "{text}");
    assert!(
        text.ends_with("\nRerun failed: goodflows run default --jobs=slow\n"),
        "{text}"
    );
}

#[test]
fn rerun_hint_keeps_an_explicit_config_and_skipped_dependents() {
    let p = Project::new("");
    std::fs::write(
        p.dir.join("my ci.toml"),
        "[settings]\nfail_fast=false\n[[jobs]]\nname='build'\ncommand='exit 1'\n[[jobs]]\nname='deploy'\ndepends_on=['build']\ncommand='true'\n[[jobs]]\nname='other'\ncommand='true'\n",
    )
    .unwrap();
    let output = p.run(&["--config", "my ci.toml"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    let hint = "goodflows run default --config='my ci.toml' --jobs='build,deploy'";
    assert!(
        text.ends_with(&format!("\nRerun failed: {hint}\n")),
        "{text}"
    );
    let rerun = run_hint(&p, hint);
    assert_eq!(rerun.status.code(), Some(1), "{}", stderr(&rerun));
    let text = stdout(&rerun);
    assert!(text.contains("jobs=2"), "{text}");
    assert!(!text.contains("other"), "{text}");
    assert!(text.ends_with(&format!("Rerun failed: {hint}\n")), "{text}");
    // Without --config, the hint has none.
    std::fs::copy(p.dir.join("my ci.toml"), p.dir.join("goodflows.toml")).unwrap();
    let text = stdout(&p.run(&[]));
    assert!(
        text.ends_with("\nRerun failed: goodflows run default --jobs='build,deploy'\n"),
        "{text}"
    );
}

#[test]
fn call_aware_events_add_only_invocation_identity() {
    let p = Project::new(
        "[[workflows.ci.jobs]]\nname='compile'\ncommand='echo hi'\n\
         [[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\n\
         [[workflows.suite.jobs]]\nname='compile'\ncommand='echo hi'\n",
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    let events: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let keys = |kind: &str, workflow: &str| {
        let event = events
            .iter()
            .find(|e| e["type"] == kind && e["workflow"] == workflow)
            .unwrap_or_else(|| panic!("no {kind} in {workflow}"));
        event
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys("job_started", "ci"),
        [
            "invocation_id",
            "name",
            "schema_version",
            "type",
            "workflow"
        ]
    );
    assert_eq!(
        keys("job_started", "suite"),
        [
            "caller_job",
            "invocation_id",
            "name",
            "parent_run_id",
            "schema_version",
            "type",
            "workflow"
        ]
    );
    assert_eq!(
        keys("job_output", "suite"),
        [
            "caller_job",
            "data",
            "invocation_id",
            "name",
            "parent_run_id",
            "schema_version",
            "stream",
            "type",
            "workflow"
        ]
    );
    // Each summary's run_id is its invocation_id; the root summary is last.
    for summary in events.iter().filter(|e| e["type"] == "summary") {
        assert_eq!(summary["run_id"], summary["invocation_id"]);
    }
    assert_eq!(events.last().unwrap()["workflow"], "ci");
}

const HISTORY: &str = "[reporting]\nproject = \"goodcases\"\nhistory = true\n";

/// A run of `config` without the caller's CI variables, with reporting
/// credentials for `url` when set.
fn run_with_history(config: &str, url: Option<&str>, args: &[&str]) -> std::process::Output {
    let p = Project::new(config);
    let mut command = p.command();
    command.env_remove("GITHUB_ACTIONS").args(args);
    if let Some(url) = url {
        command
            .env("GOODFLOWS_URL", url)
            .env("GOODFLOWS_API_KEY", "synthetic-key");
    }
    command.output().unwrap()
}

/// A mock that answers every request with `status` and an empty body.
fn status_mock(status: u16) -> mock::Mock {
    mock::Mock::start(move |_, _| {
        mock::Reply::Status(
            status,
            vec![("Retry-After".into(), "0".into())],
            String::new(),
        )
    })
}

#[test]
fn run_upload_events_have_one_text_line_each() {
    let plain = format!("{HISTORY}[[jobs]]\nname = 'a'\ncommand = 'true'\n");
    let stored = mock::Mock::store();
    // The first post is stored but its acknowledgement is lost, so the retry
    // is acknowledged as already stored.
    let rows = std::sync::Mutex::new(std::collections::HashMap::new());
    let next = std::sync::Mutex::new(0u32);
    let lost = mock::Mock::start(move |request, index| {
        let reply = mock::store_reply(&rows, &next, request);
        if index == 0 {
            mock::Reply::Status(503, vec![("Retry-After".into(), "0".into())], String::new())
        } else {
            reply
        }
    });
    let mut cases = vec![
        (plain.clone(), Some(stored.url.clone()), "uploaded".to_owned()),
        (
            plain.clone(),
            Some(lost.url.clone()),
            "uploaded (already stored)".into(),
        ),
        (
            plain.clone(),
            None,
            "upload skipped: credentials unavailable".into(),
        ),
        (
            plain.clone(),
            Some("http://example.com".into()),
            "upload skipped: invalid url".into(),
        ),
        (
            format!(
                "{HISTORY}spool_dir = 'goodflows.toml/spool'\n[[jobs]]\nname = 'a'\ncommand = 'true'\n"
            ),
            Some(stored.url.clone()),
            "upload skipped: spool unavailable".into(),
        ),
        (
            format!("{HISTORY}[[jobs]]\nname = '{}'\ncommand = 'true'\n", "j".repeat(65)),
            Some(stored.url.clone()),
            "upload skipped: invalid record at results[0].name".into(),
        ),
    ];
    let mocks: Vec<_> = [
        (503, "upload pending: service unavailable (HTTP 503)"),
        (401, "upload pending: unauthorized (HTTP 401)"),
        (403, "upload pending: forbidden (HTTP 403)"),
        (404, "upload pending: project not found (HTTP 404)"),
        (413, "upload pending: request too large (HTTP 413)"),
        (422, "upload pending: rejected (HTTP 422)"),
        (
            409,
            "upload pending: conflict (HTTP 409), not retried by reports upload",
        ),
        (
            400,
            "upload pending: http error (HTTP 400), not retried by reports upload",
        ),
        (
            415,
            "upload pending: http error (HTTP 415), not retried by reports upload",
        ),
    ]
    .into_iter()
    .map(|(status, line)| (status_mock(status), line))
    .collect();
    for (mock, line) in &mocks {
        cases.push((plain.clone(), Some(mock.url.clone()), (*line).into()));
    }
    for (config, url, line) in cases {
        let output = run_with_history(&config, url.as_deref(), &[]);
        success(&output);
        let text = stdout(&output);
        let lines: Vec<_> = text
            .lines()
            .filter(|l| l.starts_with("run history"))
            .collect();
        assert_eq!(lines, [format!("run history for default {line}")], "{text}");
        assert!(!text.contains("synthetic-key"));
    }
}

#[test]
fn run_upload_lines_name_each_invocation_of_a_call() {
    let mock = mock::Mock::store();
    let output = run_with_history(
        &format!(
            "{HISTORY}[[workflows.ci.jobs]]\nname = 'call'\nworkflow = 'suite'\n\
             [[workflows.suite.jobs]]\nname = 'work'\ncommand = 'true'\n"
        ),
        Some(&mock.url),
        &["run", "ci"],
    );
    success(&output);
    let text = stdout(&output);
    let lines: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("run history"))
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].starts_with("run history for suite [") && lines[0].ends_with("] uploaded"),
        "{text}"
    );
    assert!(
        lines[1].starts_with("run history for ci [") && lines[1].ends_with("] uploaded"),
        "{text}"
    );
}

#[test]
fn run_upload_events_are_versioned_json_lines() {
    let stored = mock::Mock::store();
    let conflict = status_mock(409);
    for (url, kind, keys) in [
        (
            Some(stored.url.as_str()),
            "run_uploaded",
            &[
                "duplicate",
                "run_id",
                "schema_version",
                "server_id",
                "type",
                "workflow",
            ][..],
        ),
        (
            Some(conflict.url.as_str()),
            "run_upload_pending",
            &[
                "http_status",
                "reason",
                "run_id",
                "schema_version",
                "terminal",
                "type",
                "workflow",
            ][..],
        ),
        (
            None,
            "run_upload_skipped",
            &["reason", "run_id", "schema_version", "type", "workflow"][..],
        ),
    ] {
        let output = run_with_history(
            &format!("{HISTORY}[[jobs]]\nname = 'a'\ncommand = 'true'\n"),
            url,
            &["--json"],
        );
        success(&output);
        // Every line parses and carries schema_version 1.
        let events = events(&output);
        let summary = events.last().unwrap();
        assert_eq!(summary["type"], "summary");
        let found: Vec<_> = events.iter().filter(|e| e["type"] == kind).collect();
        assert_eq!(found.len(), 1, "{kind}");
        let mut names: Vec<_> = found[0].as_object().unwrap().keys().cloned().collect();
        names.sort();
        assert_eq!(names, keys, "{kind}");
        assert_eq!(found[0]["run_id"], summary["run_id"]);
    }
}

// History read commands (design D9).

const RUN_ID: &str = "fb7d2711-6b67-4f5a-9104-492032a9e1e5";
const READER: &str =
    "[reporting]\nproject = \"goodcases\"\n[[jobs]]\nname = 'a'\ncommand = 'true'\n";

/// One stored run summary as the server lists it.
fn summary(run_id: &str, workflow: &str, source: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"id": "00000000-0000-4000-8000-000000000001", "run_id": run_id,
        "workflow": workflow, "environment": null, "outcome": "passed",
        "started_at": "2026-11-09T13:25:31Z", "duration_ms": 9450, "parallelism": 4,
        "max_in_flight": 2, "graph_sha256": "a".repeat(64), "source": source,
        "host": {"os": "linux", "arch": "x86_64", "logical_cpus": 8},
        "inserted_at": "2026-11-09T13:25:41Z"})
}

/// A `history show` response holding `full.json`, as a `pk_` key sees it
/// when `public`.
fn shown(public: bool) -> serde_json::Value {
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/workflow-runs/full.json")).unwrap();
    if public {
        for result in document["results"].as_array_mut().unwrap() {
            result["command"] = serde_json::Value::Null;
            result["error"] = serde_json::Value::Null;
        }
        for job in document["graph"]["jobs"].as_array_mut().unwrap() {
            job["command"] = serde_json::Value::Null;
            for step in job["steps"].as_array_mut().unwrap() {
                step["command"] = serde_json::Value::Null;
            }
        }
    }
    serde_json::json!({"data": {"id": "00000000-0000-4000-8000-000000000002", "run_id": RUN_ID,
        "inserted_at": "2026-11-09T13:25:41Z", "document": document,
        "test_reports": [{"id": "00000000-0000-4000-8000-000000000003", "job": "test",
            "job_status": "passed", "suite_id": "suite-1", "report_id": "r1",
            "partition": "1", "partition_count": 2, "status": "complete"}]}})
}

/// `history <args>` against a mock that answers 200 with `body`.
fn read_history(body: serde_json::Value, args: &[&str]) -> std::process::Output {
    let body = body.to_string();
    let mock = mock::Mock::start(move |_, _| mock::Reply::Status(200, vec![], body.clone()));
    let mut all = vec!["history"];
    all.extend_from_slice(args);
    run_with_history(READER, Some(&mock.url), &all)
}

#[test]
fn history_list_prints_one_row_per_run_and_the_next_cursor() {
    let commit = "5a4a9715f1cd0a9795766bcd9294a77549b5b474";
    let page = serde_json::json!({"data": [
        summary(RUN_ID, "ci", serde_json::json!({"commit": commit, "dirty": true})),
        summary("0e54fb55-a115-4ac7-80fe-8fde26e76afb", "suite", serde_json::json!({"commit": commit})),
        summary("1e54fb55-a115-4ac7-80fe-8fde26e76afb", "lint", serde_json::Value::Null),
    ], "next_cursor": "MjAyNi0xMS0wOVQxMzoyNTozMVp8ZmI3ZA"});
    let output = read_history(page.clone(), &["list"]);
    success(&output);
    let text = stdout(&output);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    for column in [
        "STARTED", "OUTCOME", "WORKFLOW", "ENV", "COMMIT", "BRANCH", "EVENT", "DURATION", "RUN ID",
    ] {
        assert!(lines[0].contains(column), "{text}");
    }
    let row = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        row(lines[1]),
        [
            "2026-11-09T13:25:31Z",
            "passed",
            "ci",
            "-",
            "5a4a9715f1cd*",
            "-",
            "-",
            "9.4s",
            RUN_ID
        ]
    );
    // A clean or unknown tree has no marker; no source is absent.
    assert_eq!(row(lines[2])[4], "5a4a9715f1cd");
    assert_eq!(row(lines[3])[4], "-");
    assert_eq!(
        lines[4],
        "next: --cursor MjAyNi0xMS0wOVQxMzoyNTozMVp8ZmI3ZA"
    );

    // The last page has no next line.
    let mut last = page;
    last["next_cursor"] = serde_json::Value::Null;
    let output = read_history(last, &["list"]);
    success(&output);
    assert!(!stdout(&output).contains("next:"), "{}", stdout(&output));
    let output = read_history(
        serde_json::json!({"data": [], "next_cursor": null}),
        &["list"],
    );
    success(&output);
    assert_eq!(stdout(&output), "No stored runs.\n");
}

#[test]
fn history_show_prints_fields_results_and_linked_reports() {
    let output = read_history(shown(false), &["show", RUN_ID]);
    success(&output);
    let text = stdout(&output);
    for expected in [
        &format!("Run {RUN_ID}")[..],
        "  workflow: ci\n",
        "  environment: staging\n",
        "  outcome: passed\n",
        "  started: 2026-11-09T13:25:31Z\n",
        "  duration: 9.4s\n",
        "  parallelism: 4 (max in flight 2)\n",
        "  graph: 4fad73ce6da5edfe83dfd6d50f0afa5a1587d54f96d20a1db47aae7f8dbe31e4\n",
        "  source: 5a4a9715f1cd0a9795766bcd9294a77549b5b474 (clean) on feature/login\n",
        "pull request 42",
        "  host: linux x86_64, 8 CPUs",
        "  build: make build\n",
        "  build: make build  if expression env.CI == \"true\"\n",
        "    unit: make test  if shell test -f Makefile\n",
        "  integration: calls suite  if expression true\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    let rows: Vec<Vec<&str>> = text
        .split("\nResults\n")
        .nth(1)
        .unwrap()
        .lines()
        .take_while(|l| !l.is_empty())
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows[1],
        ["build", "passed", "1.2s", "0ms", "0ms", "0", "980ms", "50.0", "MiB"]
    );
    // Partition instances, and a call result naming its target and child run.
    assert_eq!(
        rows[2][..6],
        ["test", "(1/2)", "passed", "4.1s", "1.2s", "0ms"]
    );
    assert_eq!(rows[3][..2], ["test", "(2/2)"]);
    assert_eq!(
        rows[4],
        [
            "integration",
            "passed",
            "4.1s",
            "5.3s",
            "0ms",
            "-",
            "-",
            "-",
            "suite",
            "run",
            "0e54fb55-a115-4ac7-80fe-8fde26e76afb"
        ]
    );
    let reports = text.split("\nTest reports\n").nth(1).unwrap();
    let report: Vec<_> = reports.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(
        report,
        ["test", "1/2", "passed", "suite-1", "r1", "complete"]
    );
}

#[test]
fn history_show_with_a_public_key_prints_redacted_text_as_absent() {
    let output = read_history(shown(true), &["show", RUN_ID]);
    success(&output);
    let text = stdout(&output);
    let commands = text.split("\nCommands\n").nth(1).unwrap();
    assert!(
        commands.starts_with("  build: -\n  test (1/2): -\n"),
        "{text}"
    );
    assert!(!text.contains("make "), "{text}");
    assert!(
        text.contains("  build: -  if expression env.CI == \"true\"\n"),
        "{text}"
    );
    assert!(text.contains("    lint: -\n"), "{text}");
    assert!(!text.contains("\nErrors\n"));
}

#[test]
fn history_text_escapes_server_strings() {
    let hostile = "ci\x1b[2J\rdone";
    let page = serde_json::json!({"data": [summary(RUN_ID, hostile, serde_json::Value::Null)],
        "next_cursor": null});
    let output = read_history(page, &["list"]);
    success(&output);
    assert!(
        stdout(&output).contains("ci\\u{1b}[2J\\rdone"),
        "{}",
        stdout(&output)
    );
    assert!(!output.stdout.contains(&0x1b));
    let mut run = shown(false);
    run["data"]["document"]["workflow"] = serde_json::json!(hostile);
    run["data"]["document"]["results"][0]["error"] = serde_json::json!("bad\x07");
    let output = read_history(run, &["show", RUN_ID]);
    success(&output);
    let text = stdout(&output);
    assert!(text.contains("  workflow: ci\\u{1b}[2J\\rdone\n"), "{text}");
    assert!(text.contains("\nErrors\n  build: bad\\u{7}\n"), "{text}");
    assert!(!output.stdout.contains(&0x1b) && !output.stdout.contains(&0x07));
}

#[test]
fn history_json_emits_exactly_one_event() {
    let page = serde_json::json!({"data": [summary(RUN_ID, "ci", serde_json::Value::Null)],
        "next_cursor": "abc"});
    let output = read_history(page.clone(), &["list", "--json"]);
    success(&output);
    let list = events(&output);
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0],
        serde_json::json!({"type": "history_runs", "schema_version": 1, "runs": page["data"],
            "next_cursor": "abc"})
    );
    let last = serde_json::json!({"data": [], "next_cursor": null});
    let list = events(&read_history(last, &["list", "--json"]));
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["next_cursor"], serde_json::Value::Null);

    // `show` passes the server's `data` through unchanged.
    let body = shown(true);
    let output = read_history(body.clone(), &["show", RUN_ID, "--json"]);
    success(&output);
    let show = events(&output);
    assert_eq!(show.len(), 1);
    assert_eq!(
        show[0],
        serde_json::json!({"type": "history_run", "schema_version": 1, "run": body["data"]})
    );
}
