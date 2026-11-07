mod common;
use common::*;
use serde_json::{json, Value};

fn events(output: &std::process::Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn step_outputs_flow_through_job_outputs_into_downstream_conditions() {
    let p = Project::new(
        r#"
[environments.orb.settings]
workdir = "app"
[[workflows.ci.jobs]]
name = "consumer"
depends_on = ["detect"]
env = { ENABLED = "yes" }
condition = { expression = 'env.ENABLED == "yes" && needs.detect.outputs.changed && needs.detect.outputs.count == 3' }
command = "touch consumed"
[[workflows.ci.jobs]]
name = "detect"
shell = false
env = { MODE = "job" }
condition = { expression = '(env.GOODFLOWS_TEST_UNSET ?? "off") == "off"' }
outputs = { changed = "steps.scan-job.outputs.changed", count = "steps.scan-job.outputs.count" }
[[workflows.ci.jobs.steps]]
id = "scan-job"
shell = true
command = """printf '%s' '{"changed":true,"count":3,"large":9007199254740993,"odd.key":"$(touch injected)"}' > "$GOODFLOWS_OUTPUT"; printf '%s' "$GOODFLOWS_OUTPUT" > output-path; touch ready; mkdir nested; export LEAK=yes; cd nested; printf scan"""
[[workflows.ci.jobs.steps]]
id = "skip"
condition = { expression = "!steps.scan-job.outputs.changed" }
command = "touch forbidden"
[[workflows.ci.jobs.steps]]
id = "run"
env = { MODE = "step" }
condition = { expression = 'env.MODE == "step" && steps.scan-job.outputs.large > 9007199254740992 && steps["scan-job"].outputs["odd.key"] == "$(touch injected)"' }
command = "sh -c 'test -f ready && test -z \"$LEAK\" && test \"$MODE\" = step && printf run'"
[[workflows.ci.jobs.steps]]
id = "verify"
condition = { shell = 'test "$MODE" = job' }
command = "printf verify"
[[workflows.ci.jobs]]
name = "unselected"
command = "touch unselected"
"#,
    );
    std::fs::create_dir(p.dir.join("app")).unwrap();
    let output = p
        .command()
        .env_remove("GOODFLOWS_TEST_UNSET")
        .env("ENABLED", "no")
        .args([
            "run",
            "ci",
            "--env",
            "orb",
            "--jobs",
            "consumer",
            "--json",
            "--log-dir",
            "logs",
        ])
        .output()
        .unwrap();
    success(&output);
    let events = events(&output);
    let summary = events.last().unwrap();
    assert_eq!(summary["passed"], 2);
    assert_eq!(
        summary["results"][1]["outputs"],
        json!({"changed":true,"count":3})
    );
    let steps = summary["results"][1]["steps"].as_array().unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|s| s["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["passed", "skipped", "passed", "passed"]
    );
    assert_eq!(steps[0]["outputs"]["large"], json!(9007199254740993_u64));
    assert_eq!(steps[1]["outputs"], json!({}));
    assert!(events
        .iter()
        .any(|e| e["type"] == "job_output" && e["step"] == "run" && e["data"] == "run"));
    assert!(events
        .iter()
        .all(|e| e["workflow"] == "ci" && e["environment"] == "orb"));
    assert!(p.dir.join("app/consumed").exists());
    for file in ["forbidden", "injected", "unselected"] {
        assert!(!p.dir.join("app").join(file).exists());
    }
    assert!(
        !std::path::Path::new(&p.read("app/output-path")).exists(),
        "temporary output file leaked"
    );
    let run = run_dir(&p.dir.join("logs/ci"));
    assert_eq!(
        std::fs::read_to_string(run.join("002-detect.stdout.log")).unwrap(),
        "scanrunverify"
    );
}

#[test]
fn expressions_use_strings_for_env_and_short_circuit_missing_outputs() {
    for (expression, status) in [
        (r#"env.FLAG == "false" && env.INHERITED == "yes""#, "passed"),
        (r#"(env.ABSENT ?? "fallback") == "fallback""#, "passed"),
        ("env.ABSENT == null", "passed"),
        ("false && needs.source.outputs.missing", "skipped"),
        ("true || needs.source.outputs.missing", "passed"),
        (r#"env.FLAG == "true""#, "skipped"),
        ("env.FLAG", "failed"),
        ("env.FLAG == false", "failed"),
        ("needs.source.outputs.missing ?? true", "failed"),
        ("needs.source.outputs.flag == true", "passed"),
    ] {
        let p = Project::new(&format!(
            r#"
[[jobs]]
name = "source"
command = '''printf '%s' '{{"flag":true}}' > "$GOODFLOWS_OUTPUT"'''
[[jobs]]
name = "consumer"
depends_on = ["source"]
env = {{ FLAG = "false" }}
condition = {{ expression = '{expression}' }}
command = "touch ran"
"#
        ));
        let output = p
            .command()
            .env("INHERITED", "yes")
            .env_remove("ABSENT")
            .args(["--json"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if status == "failed" { 1 } else { 0 }),
            "{expression}: {}",
            stdout(&output)
        );
        let events = events(&output);
        let result = &events.last().unwrap()["results"][1];
        assert_eq!(result["status"], status, "{expression}");
        assert_eq!(
            p.dir.join("ran").exists(),
            status == "passed",
            "{expression}"
        );
    }
}

#[test]
fn a_failed_step_stops_later_steps_and_publishes_no_job_outputs() {
    let p = Project::new(
        r#"
[settings]
fail_fast = false
[[jobs]]
name = "producer"
outputs = { value = "steps.first.outputs.value" }
[[jobs.steps]]
id = "first"
command = '''printf '%s' '{"value":7}' > "$GOODFLOWS_OUTPUT"'''
[[jobs.steps]]
id = "fail"
command = '''printf error >&2; printf '%s' '{"value":99}' > "$GOODFLOWS_OUTPUT"; exit 9'''
[[jobs.steps]]
id = "later"
command = "touch later"
[[jobs]]
name = "dependent"
depends_on = ["producer"]
command = "touch dependent"
[[jobs]]
name = "unrelated"
command = "touch unrelated"
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let result = &events.last().unwrap()["results"][0];
    assert_eq!(result["status"], "failed");
    assert_eq!(result["exit_code"], 9);
    assert!(result["error"].as_str().unwrap().contains("step 'fail'"));
    assert!(result["command"].as_str().unwrap().contains("exit 9"));
    assert_eq!(result["stderr_tail"], "error");
    assert_eq!(result["outputs"], json!({}));
    assert_eq!(result["steps"][0]["outputs"], json!({"value":7}));
    assert_eq!(result["steps"][1]["outputs"], json!({}));
    assert_eq!(result["steps"][2]["status"], "skipped");
    assert!(!p.dir.join("later").exists());
    assert!(!p.dir.join("dependent").exists());
    assert!(p.dir.join("unrelated").exists());
}

#[test]
fn invalid_outputs_fail_the_producer_instead_of_silently_skipping_consumers() {
    for command in [
        r#"printf broken > "$GOODFLOWS_OUTPUT""#,
        r#"printf '[]' > "$GOODFLOWS_OUTPUT""#,
        r#"printf '%s' '{"value":null}' > "$GOODFLOWS_OUTPUT""#,
        r#"printf '%s' '{"value":{}}' > "$GOODFLOWS_OUTPUT""#,
        r#"head -c 65537 /dev/zero > "$GOODFLOWS_OUTPUT""#,
        r#"rm "$GOODFLOWS_OUTPUT"; mkfifo "$GOODFLOWS_OUTPUT""#,
    ] {
        let p = Project::new(&format!("[[jobs]]\nname='producer'\ncommand='''{command}'''\n[[jobs]]\nname='consumer'\ncommand='touch ran'\ndepends_on=['producer']"));
        let output = p.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1));
        let events = events(&output);
        let results = &events.last().unwrap()["results"];
        assert_eq!(results[0]["status"], "failed", "{command}");
        assert!(results[0]["error"]
            .as_str()
            .unwrap()
            .contains("invalid job outputs"));
        assert_eq!(results[0]["outputs"], json!({}));
        assert_eq!(results[1]["status"], "skipped");
        assert!(!p.dir.join("ran").exists());
    }
}

#[test]
fn missing_or_skipped_step_outputs_are_errors_when_referenced() {
    for condition in ["true", "false"] {
        let p = Project::new(&format!(
            r#"
[[jobs]]
name = "test"
outputs = {{ value = "steps.optional.outputs.missing" }}
[[jobs.steps]]
id = "optional"
condition = {{ expression = "{condition}" }}
command = "true"
"#
        ));
        let output = p.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1));
        let events = events(&output);
        let result = &events.last().unwrap()["results"][0];
        assert_eq!(result["status"], "failed");
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("missing output steps.optional.outputs.missing"));
        assert_eq!(result["outputs"], json!({}));
    }
}

#[test]
fn step_structure_and_expression_scopes_are_validated_without_running_anything() {
    for invalid in [
        "[[jobs]]\nname='bad'",
        "[[jobs]]\nname='bad'\nsteps=[]",
        "[[jobs]]\nname='bad'\ncommand='true'\n[[jobs.steps]]\nid='one'\ncommand='true'",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid='one'\ncommand='true'\n[[jobs.steps]]\nid='one'\ncommand='true'",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid=''\ncommand='true'",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid='one'\ncommand=''",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid='one'\ncommand='true'\nenv={'BAD=KEY'='x'}",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid='one'\ncommand=\"echo '\"\nshell=false",
        "[[jobs]]\nname='bad'\ncommand='true'\ncondition={expression='true',shell='true'}",
        "[[jobs]]\nname='bad'\ncommand='true'\ncondition={expression='true',typo='true'}",
        "[[jobs]]\nname='bad'\ncommand='true'\ncondition={expression='env.FLAG =='}",
        "[[jobs]]\nname='bad'\ncommand='true'\ncondition={expression='needs.safe.outputs.key == true'}",
        "[[jobs]]\nname='bad'\ncondition={expression='steps.one.outputs.key'}\n[[jobs.steps]]\nid='one'\ncommand='true'",
        "[[jobs]]\nname='bad'\n[[jobs.steps]]\nid='one'\ncommand='true'\ncondition={expression='steps.two.outputs.key'}\n[[jobs.steps]]\nid='two'\ncommand='true'",
        "[[jobs]]\nname='bad'\noutputs={key='steps.missing.outputs.key'}\n[[jobs.steps]]\nid='one'\ncommand='true'",
    ] {
        let p = Project::new(&format!("[[jobs]]\nname='safe'\ncommand='touch ran'\n{invalid}"));
        for command in [vec!["--jobs", "safe"], vec!["validate"], vec!["list"]] {
            let output = p.run(&command);
            assert_eq!(output.status.code(), Some(1), "accepted {invalid}: {}", stdout(&output));
            assert!(!p.dir.join("ran").exists());
        }
    }
}

#[test]
fn command_only_instances_get_no_output_file_and_publish_no_outputs() {
    let p = Project::new(
        r#"
[[jobs]]
name = "test"
partitions = 2
command = '''test -z "${GOODFLOWS_OUTPUT+x}" && touch checked.$GOODFLOWS_PARTITION'''
"#,
    );
    let output = p
        .command()
        .env("GOODFLOWS_OUTPUT", "inherited")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    let events = events(&output);
    let results = events.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(results.len(), 2);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result["status"], "passed");
        assert_eq!(result["outputs"], json!({}));
        assert!(p.dir.join(format!("checked.{}", index + 1)).exists());
    }
}

#[test]
fn step_outputs_flow_between_steps_of_the_same_instance() {
    let p = Project::new(
        r#"
[settings]
parallelism = 3
[[jobs]]
name = "test"
partitions = 3
[[jobs.steps]]
id = "a"
command = '''sleep 0.0$GOODFLOWS_PARTITION; printf '{"key":"%s"}' "$GOODFLOWS_PARTITION" > "$GOODFLOWS_OUTPUT"'''
[[jobs.steps]]
id = "b"
condition = { expression = 'steps.a.outputs.key == env.GOODFLOWS_PARTITION' }
command = '''test -n "$GOODFLOWS_OUTPUT" && touch b.$GOODFLOWS_PARTITION'''
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    let events = events(&output);
    let results = events.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .clone();
    for (index, result) in results.iter().enumerate() {
        let partition = (index + 1).to_string();
        assert_eq!(result["steps"][0]["outputs"], json!({"key": partition}));
        assert_eq!(result["steps"][1]["status"], "passed");
        assert_eq!(result["outputs"], json!({}));
        assert!(p.dir.join(format!("b.{partition}")).exists());
    }
}

#[test]
fn partitioned_job_before_output_producer_does_not_shift_needs_outputs() {
    // `test` expands into three results ahead of `detect`, so `needs` must be
    // looked up by the producer's unit, not its configured-job position.
    let p = Project::new(
        r#"
[[jobs]]
name = "test"
partitions = 3
command = "true"
[[jobs]]
name = "detect"
command = '''printf '{"k":"v"}' > "$GOODFLOWS_OUTPUT"'''
[[jobs]]
name = "consume"
depends_on = ["test", "detect"]
condition = { expression = 'needs.detect.outputs.k == "v"' }
command = "touch consumed"
"#,
    );
    let output = p.run(&["--json"]);
    success(&output);
    assert!(p.dir.join("consumed").exists(), "{}", stdout(&output));
    let events = events(&output);
    let results = events.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(results[3]["name"], "detect");
    assert_eq!(results[3]["outputs"], json!({"k": "v"}));
    assert_eq!(results[4]["name"], "consume");
    assert_eq!(results[4]["status"], "passed");
}

#[test]
fn a_call_job_cannot_also_define_steps() {
    let p = Project::new(
        "[[jobs]]\nname='marker'\ncommand='touch marker'\n\
         [[workflows.ci.jobs]]\nname='call'\nworkflow='default'\n[[workflows.ci.jobs.steps]]\nid='s'\ncommand='touch step'\n",
    );
    let output = p.run(&["run", "ci", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        events(&output).last().unwrap()["message"],
        "workflow 'ci': job 'call': define exactly one of a command, nonempty steps, or a workflow"
    );
    assert!(!p.dir.join("marker").exists() && !p.dir.join("step").exists());
}

#[test]
fn stepped_jobs_run_in_children_and_step_outputs_gate_calls() {
    let p = Project::new(
        r#"
[workflows.ci.settings.env]
MODE = "caller"
[[workflows.ci.jobs]]
name = "detect"
outputs = { run = "steps.scan.outputs.run" }
[[workflows.ci.jobs.steps]]
id = "scan"
command = "printf '{\"run\":true}' > \"$GOODFLOWS_OUTPUT\""
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
depends_on = ["detect"]
condition = { expression = 'needs.detect.outputs.run && env.MODE == "caller"' }
[workflows.suite.settings.env]
MODE = "child"
[[workflows.suite.jobs]]
name = "build"
outputs = { mode = "steps.first.outputs.mode" }
[[workflows.suite.jobs.steps]]
id = "first"
command = "printf '{\"mode\":\"%s\"}' \"$MODE\" > \"$GOODFLOWS_OUTPUT\""
[[workflows.suite.jobs.steps]]
id = "second"
condition = { expression = 'steps.first.outputs.mode == "child"' }
command = "echo \"$MODE\" > mode"
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(p.read("mode"), "child\n");
    let events = events(&output);
    let child = events
        .iter()
        .find(|e| e["type"] == "summary" && e["workflow"] == "suite")
        .unwrap();
    assert_eq!(child["results"][0]["outputs"], json!({"mode": "child"}));
    assert_eq!(child["results"][0]["steps"][1]["status"], "passed");
    let steps: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "step_finished")
        .map(|e| {
            (
                e["workflow"].as_str().unwrap(),
                e["result"]["id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [("ci", "scan"), ("suite", "first"), ("suite", "second")]
    );
}
