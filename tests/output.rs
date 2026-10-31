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
