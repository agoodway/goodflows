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
condition = "touch evaluated"
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
    assert_eq!(jobs[1]["condition"], "touch evaluated");
    for (key, value) in [
        ("condition", serde_json::Value::Null),
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
        "condition: touch evaluated",
    ] {
        assert!(text.contains(field), "missing {field}: {text}");
    }
    success(&p.run(&["validate"]));
    assert!(!p.dir.join("evaluated").exists());
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
fn inspection_includes_steps_conditions_and_output_mappings_without_execution() {
    let p = Project::new(
        r#"
[[jobs]]
name = "checks"
condition = { expression = 'env.ENABLED == "1"' }
outputs = { flag = "steps.scan.outputs.flag" }
[[jobs.steps]]
id = "scan"
command = "touch evaluated"
env = { MODE = "scan" }
shell = false
condition = { shell = "touch evaluated" }
"#,
    );
    for args in [vec!["list", "--json"], vec!["list"], vec!["validate"]] {
        let output = p.run(&args);
        success(&output);
        if args.contains(&"--json") {
            let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(event["jobs"][0]["command"].is_null());
            assert_eq!(
                event["jobs"][0]["outputs"]["flag"],
                "steps.scan.outputs.flag"
            );
            assert_eq!(event["jobs"][0]["steps"][0]["env"]["MODE"], "scan");
            assert_eq!(event["jobs"][0]["steps"][0]["shell"], false);
        } else if args[0] == "list" {
            for field in [
                "step scan",
                "env.ENABLED",
                "steps.scan.outputs.flag",
                "MODE",
                "shell=false",
            ] {
                assert!(stdout(&output).contains(field));
            }
        }
        assert!(!p.dir.join("evaluated").exists());
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

/// Runs `validate`, `list` and a plain run for a config that must be rejected
/// before any command executes, and checks the error message.
fn assert_rejected(config: &str, message: &str) {
    let p = Project::new(config);
    for args in [&["validate"][..], &["list"][..], &[][..]] {
        let output = p.run(args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "accepted {config:?} {args:?}"
        );
        assert!(
            stderr(&output).contains(message),
            "expected {message:?} for {config:?}: {}",
            stderr(&output)
        );
        assert!(!p.dir.join("ran").exists());
    }
}

#[test]
fn invalid_partition_configs_fail_validation_before_execution() {
    const SAFE: &str = "name='safe'\ncommand='touch ran'\n";
    const REPORTING: &str = "[reporting]\nproject='goodcases'\n";
    let jobs = [
        ("name='test'\ncommand='true'\npartitions=0\n", "job 'test': partitions count must be from 1 to 64, got 0"),
        ("name='test'\ncommand='true'\npartitions=65\n", "job 'test': partitions count must be from 1 to 64, got 65"),
        ("name='test'\ncommand='true'\npartitions=-1\n", "job 'test': partitions count must be from 1 to 64, got -1"),
        ("name='test'\ncommand='true'\npartitions={count=-1}\n", "job 'test': partitions count must be from 1 to 64, got -1"),
        ("name='test'\ncommand='true'\npartitions=9223372036854775807\n", "job 'test': partitions count must be from 1 to 64, got 9223372036854775807"),
        ("name='test'\ncommand='true'\npartitions={count=4294967296}\n", "job 'test': partitions count must be from 1 to 64, got 4294967296"),
        ("name='test'\ncommand='true'\npartitions={count=3,strategy='timings'}\n", "partitions strategy 'timings' is not supported"),
        ("name='test'\ncommand='true'\npartitions={count=3,files=['a']}\n", "partitions key 'files' is not supported"),
        ("name='test'\ncommand='true'\npartitions='three'\n", "expected a partition count integer"),
        ("name='test'\npartitions=2\noutputs={x='steps.a.outputs.x'}\n[[JOBS.steps]]\nid='a'\ncommand='true'\n", "partitioned jobs publish no job outputs"),
        ("name='test'\ncommand='true'\npartitions=2\nenv={GOODFLOWS_PARTITION='1'}\n", "reserved variable GOODFLOWS_PARTITION"),
        ("name='test'\ncommand='true'\nenv={GOODFLOWS_PARTITION_COUNT='1'}\n", "reserved variable GOODFLOWS_PARTITION_COUNT"),
        ("name='test'\ncommand='true'\nenv={GOODFLOWS_ENVIRONMENT='local'}\n", "reserved variable GOODFLOWS_ENVIRONMENT"),
        ("name='test'\n[[JOBS.steps]]\nid='a'\ncommand='true'\nenv={GOODFLOWS_ENVIRONMENT='local'}\n", "reserved variable GOODFLOWS_ENVIRONMENT"),
        ("name='test'\n[[JOBS.steps]]\nid='a'\ncommand='true'\nenv={GOODFLOWS_PARTITION='1'}\n", "reserved variable GOODFLOWS_PARTITION"),
        ("name='test'\ncommand='true'\npartitions=2\n[[JOBS]]\nname='after'\ncommand='true'\ndepends_on=['test']\ncondition={expression='needs.test.outputs.x == 1'}\n", "partitioned job 'test' publishes no outputs"),
        ("name='test'\ncommand='true'\npartitions=2\n[[JOBS]]\nname='after'\ndepends_on=['test']\n[[JOBS.steps]]\nid='a'\ncommand='true'\ncondition={expression='needs.test.outputs.x'}\n", "partitioned job 'test' publishes no outputs"),
        ("name='test'\ncommand='true'\npartitions=2\n[[JOBS]]\nname='after'\ndepends_on=['test']\noutputs={x='needs.test.outputs.x'}\n[[JOBS.steps]]\nid='a'\ncommand='true'\n", "partitioned job 'test' publishes no outputs"),
    ];
    for (job, message) in jobs {
        let top = format!("[[jobs]]\n{SAFE}[[jobs]]\n{}", job.replace("JOBS", "jobs"));
        assert_rejected(&top, message);
        let named = format!(
            "[[workflows.ci.jobs]]\n{SAFE}[[workflows.ci.jobs]]\n{}",
            job.replace("JOBS", "workflows.ci.jobs")
        );
        // Parse errors carry a TOML location instead of workflow/job context.
        if message.starts_with("job ") {
            assert_rejected(&named, &format!("workflow 'ci': {message}"));
        } else {
            assert_rejected(&named, message);
        }
    }
    // Partitioned jobs may declare reports; each instance collects its own directory.
    for jobs in ["jobs", "workflows.ci.jobs"] {
        let p = Project::new(&format!(
            "{REPORTING}[[{jobs}]]\n{SAFE}[[{jobs}]]\nname='test'\ncommand='true'\npartitions=3\nreports=['test-timings']\n"
        ));
        let output = p.run(&["validate", "--json"]);
        success(&output);
        assert!(!p.dir.join("ran").exists());
        assert!(!p.dir.join(".goodflows").exists());
    }
    // Reporting credential variables must not name a partition variable.
    for (field, name) in [
        ("token_env", "GOODFLOWS_PARTITION"),
        ("url_env", "GOODFLOWS_PARTITION"),
        ("token_env", "GOODFLOWS_PARTITION_COUNT"),
        ("url_env", "GOODFLOWS_PARTITION_COUNT"),
        ("token_env", "GOODFLOWS_ENVIRONMENT"),
        ("url_env", "GOODFLOWS_ENVIRONMENT"),
    ] {
        for jobs in [
            format!("[[jobs]]\n{SAFE}"),
            format!("[[workflows.ci.jobs]]\n{SAFE}"),
        ] {
            assert_rejected(
                &format!("{REPORTING}{field}='{name}'\n{jobs}"),
                &format!("{field} must not name a reserved goodflows variable"),
            );
        }
    }
}

#[test]
fn list_shows_partition_configuration_without_execution() {
    let p = Project::new(
        r#"
[[jobs]]
name = "compile"
command = "touch ran"
[[jobs]]
name = "test"
command = "touch ran"
depends_on = ["compile"]
partitions = 3
[[jobs]]
name = "table"
command = "touch ran"
partitions = { count = 2, strategy = "index" }
"#,
    );
    let output = p.run(&["list", "--json"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let jobs = event["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 3);
    assert!(jobs[0].get("partitions").is_none());
    assert_eq!(
        jobs[1]["partitions"],
        serde_json::json!({"count": 3, "strategy": "index"})
    );
    assert_eq!(
        jobs[2]["partitions"],
        serde_json::json!({"count": 2, "strategy": "index"})
    );

    let output = p.run(&["list"]);
    success(&output);
    let text = stdout(&output);
    assert_eq!(text.matches("  test  depends_on").count(), 1, "{text}");
    assert!(text.contains("partitions: 3 (strategy=index)"), "{text}");
    assert!(text.contains("partitions: 2 (strategy=index)"), "{text}");
    assert_eq!(text.matches("partitions:").count(), 2, "{text}");

    let output = p.run(&["validate", "--json"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["total_defined_jobs"], 3);
    assert_eq!(event["jobs"], 3);
    assert_eq!(
        event["selected_jobs"],
        serde_json::json!(["compile", "test", "table"])
    );
    let output = p.run(&["validate", "--jobs", "test"]);
    success(&output);
    assert!(stdout(&output).contains("Config valid (2 selected / 3 defined jobs)"));
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn validate_counts_configured_jobs_not_instances() {
    let p = Project::new(
        "[[jobs]]\nname='compile'\ncommand='true'\n[[jobs]]\nname='test'\ncommand='true'\npartitions=3\n[[workflows.ci.jobs]]\nname='test'\ncommand='true'\npartitions=3\n",
    );
    let output = p.run(&["validate", "default", "--json"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["total_defined_jobs"], 2);
    let output = p.run(&["validate", "--json"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["total_defined_jobs"], 3);
    assert_eq!(event["workflows"][0]["jobs"], 1);
    assert_eq!(event["workflows"][1]["jobs"], 2);
}

#[test]
fn invalid_profile_partitions_fail_before_execution_even_when_unselected() {
    const JOBS: &str = "[[jobs]]\nname='safe'\ncommand='touch ran'\n[[jobs]]\nname='test'\ncommand='touch ran'\npartitions=3\n[[workflows.ci.jobs]]\nname='plain'\ncommand='touch ran'\n[environments.local.partitions]\n";
    for (entry, message) in [
        (
            "test = -1",
            "environment 'local': job 'test': partitions count must be from 1 to 64, got -1",
        ),
        (
            "test = 0",
            "environment 'local': job 'test': partitions count must be from 1 to 64, got 0",
        ),
        (
            "test = 65",
            "environment 'local': job 'test': partitions count must be from 1 to 64, got 65",
        ),
        (
            "missing = 2",
            "environment 'local': unknown job 'missing' (partitioned jobs: test)",
        ),
        (
            "plain = 2",
            "environment 'local': job 'plain' is not partitioned",
        ),
    ] {
        assert_rejected(&format!("{JOBS}{entry}\n"), message);
    }
    // Type errors keep native TOML diagnostics: location, entry, expected type.
    for entry in ["test = 'six'", "test = { count = 6 }"] {
        let config = format!("{JOBS}{entry}\n");
        assert_rejected(&config, "line 12");
        assert_rejected(&config, entry);
        assert_rejected(&config, "expected i64");
    }
    // An omitted map and an empty table are accepted.
    for config in [
        JOBS.replace("partitions]", "settings]\nfail_fast = false"),
        JOBS.to_owned(),
    ] {
        let p = Project::new(&config);
        success(&p.run(&["validate"]));
        success(&p.run(&["validate", "--env", "local"]));
        assert!(!p.dir.join("ran").exists());
    }
}

#[test]
fn list_shows_profile_partition_count() {
    let p = Project::new(
        r#"
[[jobs]]
name = "compile"
command = "touch ran"
[[jobs]]
name = "test"
command = "touch ran"
partitions = 3
[environments.local.partitions]
test = 6
"#,
    );
    for (args, count) in [
        (vec!["list", "--json", "--env", "local"], 6),
        (vec!["list", "--json"], 3),
    ] {
        let output = p.run(&args);
        success(&output);
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(event["type"], "jobs");
        let jobs = event["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            jobs[1]["partitions"],
            serde_json::json!({"count": count, "strategy": "index"})
        );
    }
    for (args, count) in [(vec!["list", "--env", "local"], 6), (vec!["list"], 3)] {
        let output = p.run(&args);
        success(&output);
        let text = stdout(&output);
        assert_eq!(text.matches("  test").count(), 1, "{text}");
        assert!(
            text.contains(&format!("partitions: {count} (strategy=index)")),
            "{text}"
        );
    }
    let output = p.run(&["validate", "--json", "--env", "local"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["total_defined_jobs"], 2);
    assert_eq!(event["jobs"], 2);
    let output = p.run(&["validate", "--env", "local"]);
    success(&output);
    assert!(stdout(&output).contains("2 defined"), "{}", stdout(&output));
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn inspection_shows_default_history_settings_without_writing_history() {
    let p = Project::new(
        "[[jobs]]\nname='a'\ncommand='true'\n[[workflows.ci.jobs]]\nname='b'\ncommand='true'\n",
    );
    let history = p.dir.join("goodflows-history");
    let expected = serde_json::json!({
        "history": false,
        "history_dir": history.to_str().unwrap(),
        "log_runs": 10,
        "history_runs": 1000,
        "history_bytes": 10485760,
    });
    let check = |settings: &serde_json::Value| {
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&settings[key], value, "{key}: {settings}");
        }
    };
    let list = |args: &[&str]| -> serde_json::Value {
        let output = p.run(args);
        success(&output);
        serde_json::from_slice(&output.stdout).unwrap()
    };
    check(&list(&["list", "--json"])["settings"]);
    check(&list(&["validate", "--json"])["workflows"][0]["settings"]);
    check(&list(&["list", "--workflows", "--json"])["workflows"][1]["settings"]);
    check(&list(&["validate", "default", "--json"])["settings"]);
    let output = p.run(&["list"]);
    success(&output);
    let text = stdout(&output);
    assert!(
        text.contains(
            "History: history=false history_runs=1000 history_bytes=10485760 log_runs=10"
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!("history_dir: {}", history.display())),
        "{text}"
    );
    let output = p.run(&["list", "--workflows"]);
    success(&output);
    assert!(
        stdout(&output).contains("    History: history=false"),
        "{}",
        stdout(&output)
    );
    // Inspection never writes history, even when history is forced on.
    for args in [
        vec!["list", "--history"],
        vec!["validate", "--history"],
        vec!["list", "--workflows", "--history"],
        vec!["validate", "ci", "--history-dir", "runs"],
    ] {
        success(&p.run(&args));
    }
    assert!(!history.exists());
    assert!(!p.dir.join("runs").exists());
}

#[test]
fn inspection_applies_history_overrides_like_parallelism() {
    let p = Project::new(
        "[settings]\nhistory = true\nhistory_dir = 'shared'\nlog_runs = 3\n\
         [[jobs]]\nname='a'\ncommand='true'\n\
         [environments.quiet.settings]\nhistory = false\nhistory_dir = 'profile'\nhistory_runs = 7\n",
    );
    let settings = |args: &[&str]| -> serde_json::Value {
        let output = p.run(args);
        success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["settings"].clone()
    };
    let shared = settings(&["list", "--json"]);
    assert_eq!(shared["history"], true);
    assert_eq!(
        shared["history_dir"],
        p.dir.join("shared").to_str().unwrap()
    );
    assert_eq!(shared["log_runs"], 3);
    let profile = settings(&["list", "--json", "--env", "quiet"]);
    assert_eq!(profile["history"], false);
    assert_eq!(
        profile["history_dir"],
        p.dir.join("profile").to_str().unwrap()
    );
    assert_eq!(profile["history_runs"], 7);
    let cli = settings(&[
        "validate",
        "default",
        "--json",
        "--env",
        "quiet",
        "--history-dir",
        "cli",
        "--log-runs",
        "4",
        "--history-runs",
        "5",
        "--history-bytes",
        "6",
    ]);
    assert_eq!(cli["history"], true);
    assert_eq!(cli["history_dir"], p.dir.join("cli").to_str().unwrap());
    assert_eq!(
        (
            &cli["log_runs"],
            &cli["history_runs"],
            &cli["history_bytes"]
        ),
        (&4.into(), &5.into(), &6.into())
    );
    assert_eq!(
        settings(&["list", "--json", "--no-history"])["history"],
        false
    );
    for flag in ["--log-runs", "--history-runs", "--history-bytes"] {
        let output = p.run(&["list", flag, "0"]);
        assert_eq!(output.status.code(), Some(2), "{flag}");
    }
    let output = p.run(&["list", "--history", "--no-history"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn sprite_configuration_is_inspectable_offline_without_values() {
    let p = Project::new(
        r#"
[settings]
sprite_prefix = "goodflows"
[sprite]
setup = "bash scripts/sprite-setup.sh"
resume = "true"
pass_env = ["GF_TEST_UNSET_ALLOWLISTED"]
[[jobs]]
name = "test"
command = "true"
"#,
    );
    // No Sprite CLI on PATH and the allowlisted variable unset: still valid.
    let run = |args: &[&str]| {
        p.command()
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env_remove("GF_TEST_UNSET_ALLOWLISTED")
            .env_remove("SPRITE_TOKEN")
            .env_remove("SPRITES_TOKEN")
            .output()
            .unwrap()
    };
    let output = run(&["validate", "--json"]);
    success(&output);
    let output = run(&["list", "--json"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["settings"]["workdir"], p.dir.to_str().unwrap());
    // A local run ignores the Sprite settings and runs no hook.
    let output = run(&["run", "--json"]);
    success(&output);
    assert!(!p.dir.join("scripts").exists());
}
