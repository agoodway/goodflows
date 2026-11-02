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
    let files: Vec<_> = std::fs::read_dir(p.dir.join("logs"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect();
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
    let raw = std::fs::read(p.dir.join("logs/001-bytes.stdout.log")).unwrap();
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
    use std::os::unix::fs::symlink;
    use std::time::{Duration, Instant};
    let p = Project::new(
        "[[jobs]]\nname='io'\ncommand='head -c 200000 /dev/zero; sleep 3; touch escaped'",
    );
    std::fs::create_dir(p.dir.join("logs")).unwrap();
    symlink("/dev/full", p.dir.join("logs/001-io.stdout.log")).unwrap();
    let start = Instant::now();
    let output = p.run(&["--log-dir", "logs", "--json"]);
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
    assert_eq!(&finished["result"], result);
    assert!(p.read("logs/001-fail.stdout.log").starts_with("EARLY"));
    let full: String = events
        .iter()
        .filter(|e| e["type"] == "job_output" && e["stream"] == "stdout")
        .map(|e| e["data"].as_str().unwrap())
        .collect();
    assert_eq!(full.len(), 10012);
    assert!(full.starts_with("EARLY"));
    let output = p.run(&[]);
    for label in [
        "command:",
        "stdout tail",
        "stderr tail",
        "OUT-END",
        "ERR-END",
    ] {
        assert!(stdout(&output).contains(label));
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
    assert!(text.contains("Summary: 4 passed, 0 failed"), "{text}");

    let p = Project::new(
        "[settings]\nfail_fast=false\n[[jobs]]\nname='test'\npartitions=3\ncommand='if [ $GOODFLOWS_PARTITION = 2 ]; then echo boom-$GOODFLOWS_PARTITION >&2; exit 3; fi'\n",
    );
    let output = p.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    for line in [
        "test (2/3) FAILED",
        "  stderr tail:\n",
        "boom-2",
        "test (1/3) PASSED",
        "test (3/3) PASSED",
    ] {
        assert!(text.contains(line), "missing {line:?}: {text}");
    }
    // The tail belongs to the failed instance's block.
    let failed = text.find("test (2/3) FAILED").unwrap();
    assert!(text[failed..].find("  stderr tail:").is_some(), "{text}");

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
