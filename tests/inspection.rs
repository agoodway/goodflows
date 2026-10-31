mod common;
use common::*;

#[test]
fn list_shows_all_job_fields_and_effective_settings() {
    let p = Project::new(
        r#"
[settings]
parallelism = 9
fail_fast = true
[[jobs]]
name = "setup"
command = "true"
[[jobs]]
name = "test"
command = "printf '%s' 'hello world'"
depends_on = ["setup"]
timeout_secs = 30
env = { FOO = "bar" }
shell = false
"#,
    );
    let output = p.run(&[
        "list",
        "--json",
        "--jobs",
        "test",
        "--parallelism",
        "2",
        "--no-fail-fast",
    ]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["settings"]["parallelism"], 2);
    assert_eq!(event["settings"]["fail_fast"], false);
    assert_eq!(
        event["settings"]["workdir"],
        p.dir.canonicalize().unwrap().to_str().unwrap()
    );
    let jobs = event["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[1]["timeout_secs"], 30);
    assert_eq!(jobs[1]["env"]["FOO"], "bar");
    assert_eq!(jobs[1]["shell"], false);
    for (key, value) in [
        ("timeout_secs", serde_json::Value::Null),
        ("env", serde_json::json!({})),
        ("shell", serde_json::json!(true)),
        ("depends_on", serde_json::json!([])),
    ] {
        assert_eq!(jobs[0].get(key), Some(&value));
    }
    let output = p.run(&["list", "--parallelism", "2", "--no-fail-fast"]);
    success(&output);
    let text = stdout(&output);
    for field in [
        "parallelism=2",
        "fail_fast=false",
        "workdir:",
        "timeout_secs=30",
        "shell=false",
        "FOO",
        "bar",
    ] {
        assert!(text.contains(field), "missing {field}: {text}");
    }
}

#[test]
fn automatic_parallelism_is_resolved_for_list_as_for_run() {
    let p = Project::new(
        "[settings]\nparallelism=0\nfail_fast=false\n[[jobs]]\nname='a'\ncommand='true'",
    );
    let list = p.run(&["list", "--json"]);
    let run = p.run(&["--json"]);
    success(&list);
    success(&run);
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let run: serde_json::Value =
        serde_json::from_str(stdout(&run).lines().next().unwrap()).unwrap();
    assert_eq!(
        list["settings"]["parallelism"],
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    );
    for field in ["parallelism", "fail_fast", "workdir"] {
        assert_eq!(list["settings"][field], run[field]);
    }
}

#[test]
fn validation_describes_requested_selected_and_total_jobs() {
    let p = Project::new("[[jobs]]\nname='fmt'\ncommand='true'\n[[jobs]]\nname='test'\ncommand='true'\ndepends_on=['fmt']\n[[jobs]]\nname='other'\ncommand='true'");
    for (args, requested, selected) in [
        (
            vec!["validate", "--json", "--jobs", "test"],
            serde_json::json!(["test"]),
            serde_json::json!(["fmt", "test"]),
        ),
        (
            vec!["validate", "--json"],
            serde_json::json!([]),
            serde_json::json!(["fmt", "test", "other"]),
        ),
    ] {
        let output = p.run(&args);
        success(&output);
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(event["requested_jobs"], requested);
        assert_eq!(event["selected_jobs"], selected);
        assert_eq!(event["jobs"], selected.as_array().unwrap().len());
        assert_eq!(event["total_defined_jobs"], 3);
    }
    let output = p.run(&["validate", "--jobs", "test"]);
    success(&output);
    for field in ["2 selected", "3 defined", "fmt", "test"] {
        assert!(stdout(&output).contains(field));
    }
}

#[test]
fn cycle_error_names_one_actual_cycle_without_downstream_jobs() {
    let p = Project::new("[[jobs]]\nname='innocent'\ncommand='true'\ndepends_on=['a']\n[[jobs]]\nname='a'\ncommand='true'\ndepends_on=['b']\n[[jobs]]\nname='b'\ncommand='true'\ndepends_on=['a']");
    let output = p.run(&["validate", "--json"]);
    assert!(!output.status.success());
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["type"], "error");
    assert!(event["message"]
        .as_str()
        .unwrap()
        .contains("dependency cycle detected: a -> b -> a"));
    assert!(!event["message"].as_str().unwrap().contains("innocent"));
}
