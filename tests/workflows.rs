mod common;
use common::*;

const MULTI: &str = r#"
[settings]
parallelism = 3
fail_fast = false
[[workflows.ci.jobs]]
name = "setup"
command = "echo setup >> order"
[[workflows.ci.jobs]]
name = "test"
command = "echo test >> order"
depends_on = ["setup"]
[workflows.release.settings]
parallelism = 1
fail_fast = true
[[workflows.release.jobs]]
name = "test"
command = "touch released"
"#;

fn event(output: &std::process::Output) -> serde_json::Value {
    success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn named_selection_runs_only_its_graph_and_tags_every_event() {
    let p = Project::new(MULTI);
    let output = p.run(&["run", "ci", "--jobs", "test", "--json"]);
    success(&output);
    assert_eq!(p.read("order"), "setup\ntest\n");
    assert!(!p.dir.join("released").exists());
    for line in stdout(&output).lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["workflow"], "ci");
    }
    success(&p.run(&["run", "release"]));
    assert!(p.dir.join("released").exists());
}

#[test]
fn settings_inherit_and_cli_overrides_win() {
    let p = Project::new(MULTI);
    let ci = event(&p.run(&["list", "ci", "--json"]));
    assert_eq!(ci["settings"]["parallelism"], 3);
    assert_eq!(ci["settings"]["fail_fast"], false);
    let release = event(&p.run(&["list", "release", "--json"]));
    assert_eq!(release["settings"]["parallelism"], 1);
    assert_eq!(release["settings"]["fail_fast"], true);
    let release = event(&p.run(&[
        "list",
        "release",
        "--parallelism",
        "2",
        "--no-fail-fast",
        "--json",
    ]));
    assert_eq!(release["settings"]["parallelism"], 2);
    assert_eq!(release["settings"]["fail_fast"], false);
    let p = Project::new("[settings]\nparallelism=1\nfail_fast=true\n[workflows.ci.settings]\nparallelism=0\nfail_fast=false\n[[workflows.ci.jobs]]\nname='a'\ncommand='true'");
    let ci = event(&p.run(&["list", "ci", "--json"]));
    assert_eq!(
        ci["settings"]["parallelism"],
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    );
    assert_eq!(ci["settings"]["fail_fast"], false);
}

#[test]
fn relative_workdirs_use_config_directory_and_inherit() {
    let p = Project::new("");
    std::fs::create_dir_all(p.dir.join("ci/shared")).unwrap();
    std::fs::create_dir_all(p.dir.join("ci/special")).unwrap();
    std::fs::write(p.dir.join("ci/checks.toml"), "[settings]\nworkdir='shared'\n[[workflows.ci.jobs]]\nname='a'\ncommand='pwd > here'\n[workflows.release.settings]\nworkdir='special'\n[[workflows.release.jobs]]\nname='a'\ncommand='pwd > here'").unwrap();
    for (workflow, dir) in [("ci", "shared"), ("release", "special")] {
        success(&p.run(&["run", workflow, "--config", "ci/checks.toml"]));
        assert_eq!(
            p.read(&format!("ci/{dir}/here")).trim(),
            p.dir
                .join(format!("ci/{dir}"))
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
    }
}

#[test]
fn discovery_and_aggregate_validation_report_every_workflow() {
    let p = Project::new(MULTI);
    let listed = event(&p.run(&["list", "--workflows", "--json"]));
    assert_eq!(listed["type"], "workflows");
    assert!(listed["default_workflow"].is_null());
    let workflows = listed["workflows"].as_array().unwrap();
    assert_eq!(
        workflows
            .iter()
            .map(|w| w["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci", "release"]
    );
    assert_eq!(workflows[0]["jobs"], 2);
    assert_eq!(workflows[1]["jobs"], 1);
    let validated = event(&p.run(&["validate", "--json"]));
    assert_eq!(validated["type"], "config_valid");
    assert_eq!(validated["total_defined_jobs"], 3);
    assert_eq!(validated["workflows"].as_array().unwrap().len(), 2);
    let ci = event(&p.run(&["validate", "ci", "--jobs", "test", "--json"]));
    assert_eq!(ci["workflow"], "ci");
    assert_eq!(ci["selected_jobs"], serde_json::json!(["setup", "test"]));
    assert!(!p.dir.join("order").exists());
    let human = p.run(&["list", "--workflows"]);
    success(&human);
    assert!(stdout(&human).contains("ci"));
    assert!(stdout(&human).contains("release"));
}

#[test]
fn legacy_and_named_default_workflows_are_backward_compatible() {
    for config in [
        "[[jobs]]\nname='test'\ncommand='touch ran'",
        "[[workflows.default.jobs]]\nname='test'\ncommand='touch ran'",
    ] {
        let p = Project::new(config);
        success(&p.run(&[]));
        assert!(p.dir.join("ran").exists());
        success(&p.run(&["run", "default"]));
        let listed = event(&p.run(&["list", "--workflows", "--json"]));
        assert_eq!(listed["default_workflow"], "default");
    }
    let p = Project::new(&format!(
        "[[jobs]]\nname='test'\ncommand='touch legacy'\n{MULTI}"
    ));
    success(&p.run(&[]));
    assert!(p.dir.join("legacy").exists());
    assert!(!p.dir.join("released").exists());
}

#[test]
fn missing_or_unknown_workflow_never_runs_arbitrary_jobs() {
    let p = Project::new(MULTI);
    for args in [
        vec![],
        vec!["run", "typo"],
        vec!["list"],
        vec!["validate", "--jobs", "test"],
        vec!["list", "ci", "--workflows"],
    ] {
        let output = p.run(&args);
        assert!(!output.status.success(), "accepted {args:?}");
        assert!(stderr(&output).contains("workflow"), "{}", stderr(&output));
    }
    assert!(!p.dir.join("released").exists());
    assert!(!p.dir.join("order").exists());
}

#[test]
fn graphs_are_scoped_and_all_workflows_are_validated_before_execution() {
    let cases = [
        ("[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'\ndepends_on=['b']\n[[workflows.release.jobs]]\nname='b'\ncommand='true'", "unknown job 'b'"),
        ("[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'\n[[workflows.release.jobs]]\nname='b'\ncommand='true'\ndepends_on=['c']\n[[workflows.release.jobs]]\nname='c'\ncommand='true'\ndepends_on=['b']", "cycle detected"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\n[[workflows.default.jobs]]\nname='b'\ncommand='true'", "default"),
        ("[[workflows.''.jobs]]\nname='a'\ncommand='touch ran'", "workflow name"),
        ("[workflows.ci]", "no jobs"),
        ("[workflows.ci.settings]\nfail_fats=true\n[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'", "unknown field"),
    ];
    for (config, error) in cases {
        let p = Project::new(config);
        let output = p.run(&["run", "ci"]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains(error), "{}", stderr(&output));
        assert!(!p.dir.join("ran").exists());
    }
}

#[test]
fn named_log_paths_are_separate_and_cannot_escape_the_log_directory() {
    let p = Project::new("[[workflows.ci.jobs]]\nname='a'\ncommand='echo ci'\n[[workflows.release.jobs]]\nname='a'\ncommand='echo release'\n[[workflows.'../escape'.jobs]]\nname='a'\ncommand='echo safe'");
    for name in ["ci", "release", "../escape"] {
        success(&p.run(&["run", name, "--log-dir", "logs"]));
    }
    assert_eq!(p.read("logs/ci/001-a.stdout.log"), "ci\n");
    assert_eq!(p.read("logs/release/001-a.stdout.log"), "release\n");
    assert_eq!(p.read("logs/%2E%2E%2Fescape/001-a.stdout.log"), "safe\n");
    assert!(!p.dir.join("escape").exists());
}

#[test]
fn init_can_generate_a_clean_named_workflow_starter() {
    let p = Project::new("");
    success(&p.run(&["init", "--template", "rust", "--workflow", "ci", "--force"]));
    let content = p.read("goodflows.toml");
    let value: toml::Value = toml::from_str(&content).unwrap();
    assert!(value.get("jobs").is_none());
    assert_eq!(
        value["workflows"]["ci"]["jobs"][0]["name"].as_str(),
        Some("fmt")
    );
    assert!(!content.contains("depends_on = []"));
    assert!(!content.contains("shell = true"));
    success(&p.run(&["validate"]));
    success(&p.run(&["list", "ci"]));
}

#[test]
fn implicit_default_errors_still_identify_the_selected_workflow() {
    let p = Project::new("[[workflows.default.jobs]]\nname='a'\ncommand='true'");
    let output = p.run(&["--jobs", "missing", "--json"]);
    assert!(!output.status.success());
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["workflow"], "default");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("unknown job 'missing'"));
}

#[test]
fn workflow_fail_fast_settings_control_actual_execution() {
    let p = Project::new("[settings]\nparallelism=1\nfail_fast=true\n[workflows.ci.settings]\nfail_fast=false\n[[workflows.ci.jobs]]\nname='fail'\ncommand='exit 1'\n[[workflows.ci.jobs]]\nname='queued'\ncommand='touch continued'\n[[workflows.release.jobs]]\nname='fail'\ncommand='exit 1'\n[[workflows.release.jobs]]\nname='queued'\ncommand='touch released'");
    assert!(!p.run(&["run", "ci"]).status.success());
    assert!(p.dir.join("continued").exists());
    assert!(!p.run(&["run", "release"]).status.success());
    assert!(!p.dir.join("released").exists());
    assert!(!p
        .run(&["run", "release", "--no-fail-fast"])
        .status
        .success());
    assert!(p.dir.join("released").exists());
}

#[test]
fn log_encoding_distinguishes_case_and_literal_percent_names() {
    let p = Project::new("[[workflows.ci.jobs]]\nname='a'\ncommand='echo lower'\n[[workflows.CI.jobs]]\nname='a'\ncommand='echo upper'\n[[workflows.'%43%49'.jobs]]\nname='a'\ncommand='echo percent'");
    for name in ["ci", "CI", "%43%49"] {
        success(&p.run(&["run", name, "--log-dir", "logs"]));
    }
    assert_eq!(p.read("logs/ci/001-a.stdout.log"), "lower\n");
    assert_eq!(p.read("logs/%43%49/001-a.stdout.log"), "upper\n");
    assert_eq!(p.read("logs/%2543%2549/001-a.stdout.log"), "percent\n");
}

#[test]
fn named_init_nested_workdir_and_name_validation_are_preserved() {
    let p = Project::new("keep");
    success(&p.run(&[
        "init",
        "--template",
        "generic",
        "--workflow",
        "ci",
        "--config",
        "ci/checks.toml",
    ]));
    let mut config: toml::Value = toml::from_str(&p.read("ci/checks.toml")).unwrap();
    config["workflows"]["ci"]["jobs"][0]["command"] = toml::Value::String("pwd > here".into());
    std::fs::write(
        p.dir.join("ci/checks.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    success(&p.run(&["run", "ci", "--config", "ci/checks.toml"]));
    assert_eq!(
        p.read("here").trim(),
        p.dir.canonicalize().unwrap().to_str().unwrap()
    );
    let output = p.run(&[
        "init",
        "--workflow",
        &"w".repeat(65),
        "--template",
        "generic",
        "--force",
    ]);
    assert!(!output.status.success());
    assert_eq!(p.read("goodflows.toml"), "keep");
}

#[test]
fn workflow_name_matching_a_flag_does_not_enable_that_flag() {
    let p = Project::new("[[workflows.'--json'.jobs]]\nname='a'\ncommand='true'");
    let output = p.run(&["run", "--", "--json"]);
    success(&output);
    assert!(
        stdout(&output).starts_with("goodflows"),
        "{}",
        stdout(&output)
    );
    let output = p.run(&["run", "--json", "--", "--json"]);
    success(&output);
    let summary: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
    assert_eq!(summary["workflow"], "--json");
    assert_eq!(summary["success"], true);
}

#[test]
fn environment_settings_override_workflows_field_by_field_and_cli_wins() {
    let p = Project::new(&format!(
        "{MULTI}\n[environments.orb.settings]\nparallelism=7\n[environments.local.settings]\nparallelism=0\nfail_fast=false\n"
    ));
    let baseline = event(&p.run(&["list", "release", "--json"]));
    assert_eq!(baseline["settings"]["parallelism"], 1);
    assert!(baseline.get("environment").is_none());
    let orb = event(&p.run(&["list", "release", "--env", "orb", "--json"]));
    assert_eq!(orb["environment"], "orb");
    assert_eq!(orb["settings"]["parallelism"], 7);
    assert_eq!(orb["settings"]["fail_fast"], true);
    let local = event(&p.run(&["list", "release", "--env", "local", "--json"]));
    assert_eq!(local["settings"]["fail_fast"], false);
    assert_eq!(
        local["settings"]["parallelism"],
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    );
    let overridden = event(&p.run(&[
        "--env",
        "local",
        "list",
        "release",
        "--parallelism",
        "2",
        "--fail-fast",
        "--json",
    ]));
    assert_eq!(overridden["settings"]["parallelism"], 2);
    assert_eq!(overridden["settings"]["fail_fast"], true);
    for command in [vec!["list", "--workflows"], vec!["validate"]] {
        let mut args = command;
        args.extend(["--env", "orb", "--json"]);
        let aggregate = event(&p.run(&args));
        assert_eq!(aggregate["environment"], "orb");
        let workflows = aggregate["workflows"].as_array().unwrap();
        assert_eq!(workflows[0]["settings"]["parallelism"], 7);
        assert_eq!(workflows[0]["settings"]["fail_fast"], false);
        assert_eq!(workflows[1]["settings"]["parallelism"], 7);
        assert_eq!(workflows[1]["settings"]["fail_fast"], true);
    }
    let selected = event(&p.run(&["validate", "ci", "--env", "orb", "--jobs", "test", "--json"]));
    assert_eq!(
        selected["selected_jobs"],
        serde_json::json!(["setup", "test"])
    );
    assert_eq!(selected["settings"]["parallelism"], 7);
    assert!(!p.dir.join("order").exists());
}

#[test]
fn environment_workdir_is_config_relative_and_execution_uses_profile_settings() {
    for jobs in ["jobs", "workflows.ci.jobs"] {
        let p = Project::new("");
        std::fs::create_dir_all(p.dir.join("ci/base")).unwrap();
        std::fs::create_dir_all(p.dir.join("ci/orb")).unwrap();
        std::fs::write(p.dir.join("ci/checks.toml"), format!(
            "[settings]\nparallelism=1\nfail_fast=true\nworkdir='base'\n[environments.orb.settings]\nworkdir='orb'\nfail_fast=false\n[[{jobs}]]\nname='fail'\ncommand='exit 1'\n[[{jobs}]]\nname='queued'\ncommand='pwd > continued'\n"
        )).unwrap();
        let workflow = if jobs == "jobs" { "default" } else { "ci" };
        let output = p.run(&[
            "run",
            workflow,
            "--config",
            "ci/checks.toml",
            "--env",
            "orb",
            "--json",
        ]);
        assert!(!output.status.success());
        assert_eq!(
            p.read("ci/orb/continued").trim(),
            p.dir
                .join("ci/orb")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert!(!p.dir.join("ci/base/continued").exists());
        for line in stdout(&output).lines() {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(event["environment"], "orb");
            assert_eq!(event["workflow"], workflow);
        }
        std::fs::remove_file(p.dir.join("ci/orb/continued")).unwrap();
        let output = p.run(&[
            "run",
            workflow,
            "--config",
            "ci/checks.toml",
            "--env",
            "orb",
            "--fail-fast",
        ]);
        assert!(!output.status.success());
        assert!(!p.dir.join("ci/orb/continued").exists());
        assert!(stdout(&output).contains("Environment: orb"));
    }
}

#[test]
fn invalid_and_unknown_environments_fail_before_running_jobs() {
    for (profile, expected) in [
        (
            "[environments.orb.settings]\nparallelism=2",
            "unknown environment 'typo'",
        ),
        (
            "[environments.orb.settings]\nworkdir='missing'",
            "workdir does not exist",
        ),
        (
            "[environments.orb.settings]\nfail_fats=true",
            "unknown field",
        ),
        ("[environments.orb]\nparallelism=2", "unknown field"),
        (
            "[environments.''.settings]\nparallelism=2",
            "environment name",
        ),
        (
            &format!(
                "[environments.'{}'.settings]\nparallelism=2",
                "a".repeat(65)
            ),
            "environment name",
        ),
    ] {
        let p = Project::new(&format!(
            "[[jobs]]\nname='a'\ncommand='touch ran'\n{profile}\n"
        ));
        let output = p.run(&["--env", "typo", "--json"]);
        assert!(!output.status.success());
        let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            error["message"].as_str().unwrap().contains(expected),
            "{error}"
        );
        assert_eq!(error["environment"], "typo");
        assert!(!p.dir.join("ran").exists());
        // All declared profiles are validated even when none is selected.
        if expected != "unknown environment 'typo'" {
            assert!(!p.run(&["validate"]).status.success());
        }
    }
    let p = Project::new("keep");
    let output = p.run(&["init", "--env", "orb", "--template", "generic", "--force"]);
    assert!(!output.status.success());
    assert_eq!(p.read("goodflows.toml"), "keep");
}
