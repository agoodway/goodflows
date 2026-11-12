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
    assert_eq!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs/ci")).join("001-a.stdout.log")).unwrap(),
        "ci\n"
    );
    assert_eq!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs/release")).join("001-a.stdout.log"))
            .unwrap(),
        "release\n"
    );
    assert_eq!(
        std::fs::read_to_string(
            run_dir(&p.dir.join("logs/%2E%2E%2Fescape")).join("001-a.stdout.log")
        )
        .unwrap(),
        "safe\n"
    );
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
    assert_eq!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs/ci")).join("001-a.stdout.log")).unwrap(),
        "lower\n"
    );
    assert_eq!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs/%43%49")).join("001-a.stdout.log"))
            .unwrap(),
        "upper\n"
    );
    assert_eq!(
        std::fs::read_to_string(run_dir(&p.dir.join("logs/%2543%2549")).join("001-a.stdout.log"))
            .unwrap(),
        "percent\n"
    );
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

// Settings env fixtures, concatenated per test.
const SHARED: &str = r#"
[settings.env]
MIX_ENV = "test"
KEEP = "shared"
VALUE = "shared"
"#;
const LEGACY_JOB: &str = r#"
[[jobs]]
name = "show"
command = "printf '%s|%s|%s|%s|%s' \"$MIX_ENV\" \"$KEEP\" \"${CI-unset}\" \"${VALUE-unset}\" \"${VALUE+set}\" > default.out"
"#;
const EXTRA: &str = r#"
[[workflows.extra.jobs]]
name = "show"
command = "printf '%s|%s|%s|%s|%s' \"$MIX_ENV\" \"$KEEP\" \"${CI-unset}\" \"${VALUE-unset}\" \"${VALUE+set}\" > default.out"
"#;
const CI: &str = r#"
[workflows.ci.settings.env]
MIX_ENV = "dev"
[[workflows.ci.jobs]]
name = "show"
command = "printf '%s|%s|%s|%s|%s' \"$MIX_ENV\" \"$KEEP\" \"${CI-unset}\" \"${VALUE-unset}\" \"${VALUE+set}\" > ci.out"
env = { JOB_ONLY = "job" }
"#;
const RELEASE: &str = r#"
[workflows.release.settings.env]
[[workflows.release.jobs]]
name = "show"
command = "printf '%s|%s|%s|%s|%s' \"$MIX_ENV\" \"$KEEP\" \"${CI-unset}\" \"${VALUE-unset}\" \"${VALUE+set}\" > release.out"
"#;
const PROFILES: &str = r#"
[environments.ci.settings.env]
MIX_ENV = "prod"
CI = "true"
VALUE = ""
[environments.parallel.settings]
parallelism = 2
"#;

/// Shared settings, the `ci`/`release`/`extra` workflows and the profiles.
fn named_settings_env() -> String {
    [SHARED, EXTRA, CI, RELEASE, PROFILES].concat()
}

#[test]
fn settings_env_inherits_shared_workflow_and_selected_profile_per_key() {
    let p = Project::new(&[SHARED, CI, RELEASE, PROFILES].concat());
    let run = |args: &[&str]| {
        let output = p
            .command()
            .env("MIX_ENV", "caller")
            .env("CI", "caller")
            .args(args)
            .output()
            .unwrap();
        success(&output);
    };
    // Profile overrides workflow, which overrides shared; unrelated keys survive.
    run(&["run", "ci", "--env", "ci"]);
    assert_eq!(p.read("ci.out"), "prod|shared|true||set");
    // Absent profile selection: profile env has no effect; caller fills the gaps.
    run(&["run", "ci"]);
    assert_eq!(p.read("ci.out"), "dev|shared|caller|shared|set");
    // A profile with no env changes nothing.
    run(&["run", "ci", "--env", "parallel"]);
    assert_eq!(p.read("ci.out"), "dev|shared|caller|shared|set");
    // Sibling isolation: an empty workflow map inherits shared values only.
    run(&["run", "release"]);
    assert_eq!(p.read("release.out"), "test|shared|caller|shared|set");
}

#[test]
fn legacy_jobs_use_shared_settings_env_beside_named_workflows() {
    // Only a workflow named "default" conflicts with root jobs, so legacy jobs
    // coexist with named workflows and must not see their overrides.
    let p = Project::new(&[SHARED, LEGACY_JOB, CI, RELEASE, PROFILES].concat());
    let run = |args: &[&str]| {
        let output = p
            .command()
            .env("MIX_ENV", "caller")
            // CI runners export CI; keep it unset so only the profile supplies it.
            .env_remove("CI")
            .args(args)
            .output()
            .unwrap();
        success(&output);
    };
    run(&["run"]);
    assert_eq!(p.read("default.out"), "test|shared|unset|shared|set");
    run(&["run", "ci"]);
    assert_eq!(p.read("ci.out"), "dev|shared|unset|shared|set");
    run(&["run", "release"]);
    assert_eq!(p.read("release.out"), "test|shared|unset|shared|set");
    // The selected profile applies to legacy jobs too.
    run(&["run", "--env", "ci"]);
    assert_eq!(p.read("default.out"), "prod|shared|true||set");
}

#[test]
fn every_settings_scope_accepts_env_and_env_only_profiles_are_kept() {
    let p = Project::new(&named_settings_env());
    success(&p.run(&["validate"]));
    // An env-only profile is a real profile: it can be selected and contributes env.
    let listed = event(&p.run(&["list", "ci", "--env", "ci", "--json"]));
    assert_eq!(listed["environment"], "ci");
    assert_eq!(listed["settings"]["env"]["CI"], "true");
    for value in ["1", "true", "['a']", "{ a = 'b' }"] {
        let p = Project::new(&format!(
            "[[jobs]]\nname='a'\ncommand='touch ran'\n[environments.local.settings.env]\nVALUE = {value}\n"
        ));
        let output = p.run(&["validate"]);
        assert!(!output.status.success(), "accepted {value}");
        assert!(
            stderr(&output).contains("invalid type"),
            "{}",
            stderr(&output)
        );
        assert!(!p.run(&[]).status.success());
        assert!(!p.dir.join("ran").exists());
    }
}

#[test]
fn invalid_settings_env_in_any_scope_fails_before_jobs_run() {
    for (section, context) in [
        ("[settings.env]", "settings env"),
        (
            "[workflows.other.settings.env]",
            "workflow 'other' settings env",
        ),
        (
            "[environments.unused.settings.env]",
            "environment 'unused' settings env",
        ),
    ] {
        for (entry, message) in [
            (
                "GOODFLOWS_PARTITION = 'hidden-value'",
                "reserved variable GOODFLOWS_PARTITION",
            ),
            (
                "GOODFLOWS_ENVIRONMENT = 'hidden-value'",
                "reserved variable GOODFLOWS_ENVIRONMENT",
            ),
            (
                "'BAD=NAME' = 'hidden-value'",
                "invalid environment variable",
            ),
            ("PRIVATE_TOKEN = 'hidden-value'", "reporting token_env"),
        ] {
            let p = Project::new(&format!(
                "[reporting]\nproject='demo'\ntoken_env='PRIVATE_TOKEN'\n[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'\n[[workflows.other.jobs]]\nname='a'\ncommand='true'\n{section}\n{entry}\n"
            ));
            for args in [
                &["run", "ci"][..],
                &["validate"][..],
                &["list", "ci", "--json"][..],
            ] {
                let output = p.run(args);
                assert!(!output.status.success(), "accepted {section} {entry}");
                let text = format!("{}{}", stdout(&output), stderr(&output));
                assert!(text.contains(context), "{text}");
                assert!(text.contains(message), "{text}");
                assert!(!text.contains("hidden-value"), "{text}");
            }
            assert!(!p.dir.join("ran").exists());
        }
    }
    // A shared entry that a workflow overrides is still validated.
    let p = Project::new("[settings.env]\nGOODFLOWS_PARTITION='x'\n[workflows.ci.settings.env]\nGOODFLOWS_PARTITION='y'\n[[workflows.ci.jobs]]\nname='a'\ncommand='touch ran'\n");
    let output = p.run(&["run", "ci"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("settings env"),
        "{}",
        stderr(&output)
    );
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn inspection_reports_merged_settings_env_without_caller_or_job_env() {
    let p = Project::new(&named_settings_env());
    let inspect = |args: &[&str]| {
        let output = p
            .command()
            .env("AMBIENT_ONLY", "1")
            .args(args)
            .output()
            .unwrap();
        event(&output)
    };
    let expected = serde_json::json!({"MIX_ENV":"prod", "KEEP":"shared", "VALUE":"", "CI":"true"});
    let listed = inspect(&["list", "ci", "--env", "ci", "--json"]);
    assert_eq!(listed["settings"]["env"], expected);
    assert_eq!(
        listed["jobs"][0]["env"],
        serde_json::json!({"JOB_ONLY":"job"})
    );
    let validated = inspect(&["validate", "ci", "--env", "ci", "--json"]);
    assert_eq!(validated["settings"]["env"], expected);
    let no_profile = inspect(&["list", "ci", "--json"]);
    assert_eq!(
        no_profile["settings"]["env"],
        serde_json::json!({"MIX_ENV":"dev", "KEEP":"shared", "VALUE":"shared"})
    );
    for args in [
        &["list", "--workflows", "--env", "ci", "--json"][..],
        &["validate", "--env", "ci", "--json"][..],
    ] {
        let aggregate = inspect(args);
        let workflows = aggregate["workflows"].as_array().unwrap();
        let env = |name: &str| {
            workflows.iter().find(|w| w["name"] == name).unwrap()["settings"]["env"].clone()
        };
        assert_eq!(env("ci"), expected);
        assert_eq!(
            env("release"),
            serde_json::json!({"MIX_ENV":"prod", "KEEP":"shared", "VALUE":"", "CI":"true"})
        );
        assert_eq!(env("extra"), env("release"));
    }
    let aggregate = inspect(&["list", "--workflows", "--json"]);
    let release = &aggregate["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "release")
        .unwrap()["settings"]["env"];
    assert_eq!(
        *release,
        serde_json::json!({"MIX_ENV":"test", "KEEP":"shared", "VALUE":"shared"})
    );
    // Human output shows the merged settings env on the settings line.
    let human = |args: &[&str]| {
        let output = p
            .command()
            .env("AMBIENT_ONLY", "1")
            .args(args)
            .output()
            .unwrap();
        success(&output);
        stdout(&output)
    };
    let text = human(&["list", "ci", "--env", "ci"]);
    assert!(
        text.contains(r#"env={"CI":"true","KEEP":"shared","MIX_ENV":"prod","VALUE":""}"#),
        "{text}"
    );
    assert!(!text.contains("AMBIENT_ONLY"), "{text}");
    let text = human(&["list", "--workflows"]);
    assert!(
        text.contains(r#"release: 1 jobs  parallelism="#)
            && text.contains(r#"env={"KEEP":"shared","MIX_ENV":"test","VALUE":"shared"}"#),
        "{text}"
    );
    // No settings env anywhere: an empty object, never the caller's variables.
    let p = Project::new(MULTI);
    let output = p
        .command()
        .env("AMBIENT_ONLY", "1")
        .args(["list", "ci", "--json"])
        .output()
        .unwrap();
    assert_eq!(event(&output)["settings"]["env"], serde_json::json!({}));
    let output = p
        .command()
        .env("AMBIENT_ONLY", "1")
        .args(["validate", "--json"])
        .output()
        .unwrap();
    for workflow in event(&output)["workflows"].as_array().unwrap() {
        assert_eq!(workflow["settings"]["env"], serde_json::json!({}));
    }
    let text = stdout(&p.run(&["list", "ci"]));
    assert!(text.contains(" env={}\n"), "{text}");
}

/// Every README `toml` block that mentions `settings.env` must validate. Blocks
/// that declare jobs are validated as written. Fragments without jobs get one
/// trivial job per `[workflows.NAME...]` table they mention (or a root job when
/// they mention none), so only the settings they show are under test.
#[test]
fn readme_settings_env_examples_validate() {
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    let blocks: Vec<&str> = readme
        .split("```toml\n")
        .skip(1)
        .map(|rest| rest.split("```").next().unwrap())
        .filter(|block| block.contains("settings.env"))
        .collect();
    assert!(
        blocks.len() >= 2,
        "expected settings.env examples in README"
    );
    for block in blocks {
        let mut config = block.to_owned();
        if !config.contains("jobs]]") {
            let mut workflows: Vec<&str> = block
                .lines()
                .filter_map(|line| line.trim().strip_prefix("[workflows."))
                .filter_map(|rest| rest.split('.').next())
                .collect();
            workflows.dedup();
            if workflows.is_empty() {
                config.push_str("\n[[jobs]]\nname = 'readme'\ncommand = 'true'\n");
            }
            for name in workflows {
                config.push_str(&format!(
                    "\n[[workflows.{name}.jobs]]\nname = 'readme'\ncommand = 'true'\n"
                ));
            }
        }
        let p = Project::new(&config);
        let output = p.run(&["validate"]);
        assert!(
            output.status.success(),
            "README example failed:\n{block}\n{}",
            stderr(&output)
        );
    }
}

const PARTITIONED_CI: &str = r#"
[[workflows.ci.jobs]]
name = "test"
partitions = 3
command = "printf '%s/%s' $GOODFLOWS_PARTITION $GOODFLOWS_PARTITION_COUNT > ci.$GOODFLOWS_PARTITION"
[environments.local.partitions]
test = 6
"#;

fn ci_outputs(p: &Project) -> Vec<String> {
    (1..=6)
        .filter(|i| p.dir.join(format!("ci.{i}")).exists())
        .map(|i| p.read(&format!("ci.{i}")))
        .collect()
}

#[test]
fn profile_partition_entry_is_ignored_where_the_job_is_absent() {
    let p = Project::new(&format!(
        "{PARTITIONED_CI}[[workflows.nightly.jobs]]\nname = 'lint'\ncommand = 'touch nightly'\n"
    ));
    success(&p.run(&["run", "ci", "--env", "local"]));
    assert_eq!(
        ci_outputs(&p),
        (1..=6).map(|i| format!("{i}/6")).collect::<Vec<_>>()
    );
    success(&p.run(&["run", "nightly", "--env", "local"]));
    assert!(p.dir.join("nightly").exists());
    success(&p.run(&["validate", "--env", "local"]));
    // Without --env, aggregate validation checks every profile and passes here.
    success(&p.run(&["validate"]));
    let output = p.run(&["list", "--workflows", "--env", "local", "--json"]);
    success(&output);
}

#[test]
fn profile_partition_entry_fails_where_the_job_is_not_partitioned() {
    let p = Project::new(&format!(
        "{PARTITIONED_CI}[[workflows.nightly.jobs]]\nname = 'test'\ncommand = 'touch nightly'\n"
    ));
    success(&p.run(&["run", "ci", "--env", "local"]));
    assert_eq!(ci_outputs(&p).len(), 6);
    let message = "environment 'local': workflow 'nightly': job 'test' is not partitioned";
    for args in [
        &["run", "nightly", "--env", "local"][..],
        &["validate", "nightly", "--env", "local"][..],
        &["list", "nightly", "--env", "local"][..],
        &["validate", "--env", "local"][..],
        &["list", "--workflows", "--env", "local"][..],
        // Without --env, aggregate validation resolves every profile.
        &["validate"][..],
    ] {
        let output = p.run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&output).contains(message),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(!p.dir.join("nightly").exists(), "{args:?}");
    }
    // The JSON error event names the workflow too.
    let output = p.run(&["run", "nightly", "--env", "local", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains(message), "{}", stdout(&output));
    // Without the profile, nightly still runs and the other commands pass.
    success(&p.run(&["validate", "nightly"]));
    success(&p.run(&["list", "--workflows"]));
    success(&p.run(&["run", "nightly"]));
    assert!(p.dir.join("nightly").exists());
}

#[test]
fn profile_partition_checks_apply_with_jobs_selection() {
    let p = Project::new(&format!(
        "{PARTITIONED_CI}[[workflows.nightly.jobs]]\nname = 'test'\ncommand = 'touch nightly'\n\
         [[workflows.nightly.jobs]]\nname = 'lint'\ncommand = 'touch lint'\n"
    ));
    // The check covers the whole workflow, not only the --jobs selection.
    let output = p.run(&["run", "nightly", "--jobs", "lint", "--env", "local"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output)
            .contains("environment 'local': workflow 'nightly': job 'test' is not partitioned"),
        "{}",
        stderr(&output)
    );
    assert!(!p.dir.join("lint").exists());
    assert!(!p.dir.join("nightly").exists());
    success(&p.run(&["run", "ci", "--jobs", "test", "--env", "local"]));
    assert_eq!(
        ci_outputs(&p),
        (1..=6).map(|i| format!("{i}/6")).collect::<Vec<_>>()
    );
}

#[test]
fn readme_profile_partition_examples_validate() {
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    let blocks: Vec<&str> = readme
        .split("```toml\n")
        .skip(1)
        .map(|rest| rest.split("```").next().unwrap())
        .filter(|block| block.contains(".partitions]"))
        .collect();
    assert!(!blocks.is_empty(), "expected a profile partitions example");
    for block in blocks {
        let p = Project::new(block);
        for args in [
            &["validate"][..],
            &["validate", "--env", "local"][..],
            &["validate", "--env", "orb"][..],
        ] {
            let output = p.run(args);
            assert!(
                output.status.success(),
                "README example failed {args:?}:\n{block}\n{}",
                stderr(&output)
            );
        }
        let output = p.run(&["list", "--env", "local"]);
        success(&output);
        assert!(stdout(&output).contains("partitions: 6 (strategy=index)"));
    }
    let profiles = readme
        .split("## Environment profiles")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    assert!(!profiles.contains("cannot change a partition count"));
}

// ---- workflow-call jobs ----

/// Every summary event, in output order.
fn summaries(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|e| e["type"] == "summary")
        .cloned()
        .collect()
}

/// The summary of the invocation of `workflow` (the first, if repeated).
fn summary_for(events: &[serde_json::Value], workflow: &str) -> serde_json::Value {
    summaries(events)
        .into_iter()
        .find(|s| s["workflow"] == workflow)
        .unwrap_or_else(|| panic!("no summary for {workflow}"))
}

fn result_of(summary: &serde_json::Value, name: &str) -> serde_json::Value {
    summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no result {name}: {summary}"))
        .clone()
}

fn error_of(output: &std::process::Output) -> String {
    assert_eq!(output.status.code(), Some(1), "{}", stdout(output));
    let event: serde_json::Value = serde_json::from_str(stdout(output).lines().last().unwrap())
        .unwrap_or_else(|_| panic!("{}", stdout(output)));
    assert_eq!(event["type"], "error", "{event}");
    event["message"].as_str().unwrap().to_owned()
}

const SUITE: &str = "[[workflows.suite.jobs]]\nname='t'\ncommand='echo suite >> order'\n";

#[test]
fn call_jobs_accept_the_workflow_form_with_harmless_defaults() {
    let p = Project::new(&format!(
        "[[workflows.ci.jobs]]\nname='marker'\ncommand='touch marker'\n\
         [[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\ndepends_on=['marker']\n\
         condition={{ expression = 'true' }}\nenv={{}}\noutputs={{}}\nreports=[]\nshell=true\nsteps=[]\n{SUITE}"
    ));
    success(&p.run(&["validate", "ci"]));
    assert!(!p.dir.join("marker").exists());
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(p.read("order"), "suite\n");
    let events = events(&output);
    let call = result_of(&summary_for(&events, "ci"), "call");
    assert_eq!(call["status"], "passed");
    assert_eq!(call["workflow"], "suite");
    assert!(call["command"].is_null() && call["exit_code"].is_null());
    assert_eq!(call["outputs"], serde_json::json!({}));
}

#[test]
fn call_jobs_reject_other_forms_modifiers_and_output_references_before_running() {
    let base = "[[workflows.ci.jobs]]\nname='marker'\ncommand='touch marker'\n";
    let call = "[[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\n";
    for (extra, message) in [
        (
            "command='true'\n",
            "job 'call': define exactly one of a command, nonempty steps, or a workflow",
        ),
        (
            "[[workflows.ci.jobs.steps]]\nid='s'\ncommand='true'\n",
            "job 'call': define exactly one of a command, nonempty steps, or a workflow",
        ),
        (
            "timeout_secs=5\n",
            "job 'call': workflow-call jobs do not support timeout_secs",
        ),
        (
            "partitions=2\n",
            "job 'call': workflow-call jobs do not support partitions",
        ),
        (
            "env={ A = '1' }\n",
            "job 'call': workflow-call jobs do not support env",
        ),
        (
            "outputs={ x = 'needs.marker.outputs.x' }\n",
            "job 'call': workflow-call jobs do not support outputs",
        ),
        (
            "reports=['test-timings']\n",
            "job 'call': workflow-call jobs do not support reports",
        ),
        (
            "shell=false\n",
            "job 'call': workflow-call jobs do not support shell = false",
        ),
    ] {
        let p = Project::new(&format!("{base}{call}{extra}{SUITE}"));
        for args in [&["validate", "--json"][..], &["run", "ci", "--json"]] {
            let error = error_of(&p.run(args));
            assert_eq!(error, format!("workflow 'ci': {message}"), "{extra}");
        }
        assert!(!p.dir.join("marker").exists(), "{extra}");
    }
    let p = Project::new(&format!(
        "{base}[[workflows.ci.jobs]]\nname='call'\nworkflow=''\n{SUITE}"
    ));
    for args in [&["validate", "--json"][..], &["run", "ci", "--json"]] {
        assert_eq!(
            error_of(&p.run(args)),
            "workflow 'ci': job 'call': workflow-call target must be a nonempty workflow name"
        );
    }
    assert!(!p.dir.join("marker").exists());
    // A call publishes no outputs, to conditions or to output mappings.
    for dependent in [
        "[[workflows.ci.jobs]]\nname='after'\ndepends_on=['call']\ncondition={ expression = 'needs.call.outputs.x' }\ncommand='true'\n",
        "[[workflows.ci.jobs]]\nname='after'\ndepends_on=['call']\noutputs={ v = 'needs.call.outputs.x' }\n[[workflows.ci.jobs.steps]]\nid='s'\ncommand='true'\n",
    ] {
        let p = Project::new(&format!("{base}{call}{dependent}{SUITE}"));
        let error = error_of(&p.run(&["run", "ci", "--json"]));
        assert!(
            error.starts_with("workflow 'ci': job 'after': ")
                && error.ends_with("workflow-call job 'call' publishes no outputs"),
            "{error}"
        );
        assert!(!p.dir.join("marker").exists());
    }
}

#[test]
fn call_targets_and_cycles_are_validated_across_the_whole_document() {
    let marker = "[[workflows.ci.jobs]]\nname='marker'\ncommand='touch marker'\n";
    for (config, message) in [
        (
            // An unselected workflow's unknown target still fails.
            format!("{marker}[[workflows.other.jobs]]\nname='c'\nworkflow='missing'\n"),
            "workflow 'other': job 'c': unknown workflow 'missing' (available: ci, other)".to_owned(),
        ),
        (
            format!("{marker}[[workflows.ci.jobs]]\nname='again'\nworkflow='ci'\n"),
            "workflow call cycle detected: workflow 'ci' job 'again' -> workflow 'ci'".to_owned(),
        ),
        (
            // Direct, behind a false condition and outside the selection.
            format!(
                "{marker}[[workflows.a.jobs]]\nname='to-b'\nworkflow='b'\ncondition={{ expression = 'false' }}\n\
                 [[workflows.b.jobs]]\nname='to-a'\nworkflow='a'\n"
            ),
            "workflow call cycle detected: workflow 'a' job 'to-b' -> workflow 'b' job 'to-a' -> workflow 'a'"
                .to_owned(),
        ),
        (
            format!(
                "{marker}[[workflows.a.jobs]]\nname='x'\nworkflow='b'\n[[workflows.b.jobs]]\nname='y'\nworkflow='c'\n\
                 [[workflows.c.jobs]]\nname='z'\nworkflow='a'\n"
            ),
            "workflow call cycle detected: workflow 'a' job 'x' -> workflow 'b' job 'y' -> workflow 'c' job 'z' -> workflow 'a'"
                .to_owned(),
        ),
    ] {
        let p = Project::new(&config);
        for args in [&["validate", "--json"][..], &["run", "ci", "--json"], &["list", "ci", "--json"]] {
            assert_eq!(error_of(&p.run(args)), message, "{args:?}");
        }
        assert!(!p.dir.join("marker").exists());
    }
}

#[test]
fn legacy_default_repeated_and_deep_targets_are_valid() {
    // A named workflow may call the root jobs' `default` workflow.
    let p = Project::new(
        "[[jobs]]\nname='root'\ncommand='echo root >> order'\n\
         [[workflows.ci.jobs]]\nname='legacy'\nworkflow='default'\n",
    );
    success(&p.run(&["run", "ci"]));
    assert_eq!(p.read("order"), "root\n");
    // Diamond and repeated targets are acyclic; each call runs separately.
    let p = Project::new(
        "[[workflows.ci.jobs]]\nname='b'\nworkflow='b'\n[[workflows.ci.jobs]]\nname='c'\nworkflow='c'\n\
         [[workflows.ci.jobs]]\nname='d'\nworkflow='d'\n\
         [[workflows.b.jobs]]\nname='d'\nworkflow='d'\n[[workflows.c.jobs]]\nname='d'\nworkflow='d'\n\
         [[workflows.d.jobs]]\nname='t'\ncommand='echo d >> order'\n",
    );
    success(&p.run(&["run", "ci"]));
    assert_eq!(p.read("order"), "d\nd\nd\n");
    // No nesting-depth limit: a long call chain validates and runs.
    let mut config = String::new();
    for i in 0..300 {
        config.push_str(&format!(
            "[[workflows.w{i}.jobs]]\nname='next'\nworkflow='w{}'\n",
            i + 1
        ));
    }
    config.push_str("[[workflows.w300.jobs]]\nname='leaf'\ncommand='touch leaf'\n");
    let p = Project::new(&config);
    success(&p.run(&["validate"]));
    let mut short = String::new();
    for i in 0..20 {
        short.push_str(&format!(
            "[[workflows.w{i}.jobs]]\nname='next'\nworkflow='w{}'\n",
            i + 1
        ));
    }
    short.push_str("[[workflows.w20.jobs]]\nname='leaf'\ncommand='touch leaf'\n");
    let p = Project::new(&short);
    success(&p.run(&["run", "w0", "--parallelism", "1"]));
    assert!(p.dir.join("leaf").exists());
}

const SELECT: &str = r#"
[[workflows.ci.jobs]]
name = "prepare"
command = "echo prepare >> order"
[[workflows.ci.jobs]]
name = "checks"
workflow = "suite"
depends_on = ["prepare"]
[[workflows.ci.jobs]]
name = "publish"
command = "echo publish >> order"
depends_on = ["checks"]
[[workflows.ci.jobs]]
name = "other"
command = "echo other >> order"
[[workflows.suite.jobs]]
name = "setup"
command = "echo setup >> order"
[[workflows.suite.jobs]]
name = "unit"
command = "echo unit >> order"
[[workflows.suite.jobs]]
name = "test"
command = "echo test >> order"
depends_on = ["setup"]
"#;

#[test]
fn root_selection_is_local_and_children_run_their_complete_graph() {
    let p = Project::new(SELECT);
    let output = p.run(&["run", "ci", "--jobs", "publish", "--json"]);
    success(&output);
    let order = p.read("order");
    let lines: Vec<_> = order.lines().collect();
    assert_eq!(lines.first(), Some(&"prepare"));
    assert_eq!(lines.last(), Some(&"publish"));
    let mut child: Vec<_> = lines[1..lines.len() - 1].to_vec();
    child.sort();
    assert_eq!(child, ["setup", "test", "unit"], "{order}");
    let events = events(&output);
    let root = summary_for(&events, "ci");
    // Local counts: three selected root jobs, the call counted once.
    assert_eq!(
        (
            root["passed"].as_u64(),
            root["results"].as_array().unwrap().len()
        ),
        (Some(3), 3)
    );
    assert_eq!(summary_for(&events, "suite")["passed"], 3);
    // Selecting the call itself does not filter the child.
    std::fs::remove_file(p.dir.join("order")).unwrap();
    success(&p.run(&["run", "ci", "--jobs", "checks"]));
    let mut order: Vec<_> = p.read("order").lines().map(str::to_owned).collect();
    order.sort();
    assert_eq!(order, ["prepare", "setup", "test", "unit"]);
}

#[test]
fn list_and_validate_show_call_targets_without_running() {
    let p = Project::new(SELECT);
    let validated = event(&p.run(&["validate", "ci", "--jobs", "publish", "--json"]));
    assert_eq!(
        validated["selected_jobs"],
        serde_json::json!(["prepare", "checks", "publish"])
    );
    assert_eq!(validated["jobs"], 3);
    assert_eq!(validated["total_defined_jobs"], 4);
    assert_eq!(
        validated["calls"],
        serde_json::json!([{"job":"checks", "workflow":"suite"}])
    );
    let text = p.run(&["validate", "ci"]);
    success(&text);
    assert!(
        stdout(&text).contains("  call: checks -> workflow suite\n"),
        "{}",
        stdout(&text)
    );
    let listed = event(&p.run(&["list", "ci", "--json"]));
    let checks = &listed["jobs"][1];
    assert_eq!(checks["name"], "checks");
    assert_eq!(checks["workflow"], "suite");
    assert!(checks["command"].is_null());
    assert!(listed["jobs"][0].get("workflow").is_none());
    let text = stdout(&p.run(&["list", "ci"]));
    assert!(
        text.contains("  checks  depends_on: prepare\n    workflow: suite\n"),
        "{text}"
    );
    // Discovery and aggregate validation keep local per-workflow counts.
    let workflows = event(&p.run(&["list", "--workflows", "--json"]));
    assert_eq!(workflows["workflows"][0]["jobs"], 4);
    assert_eq!(workflows["workflows"][1]["jobs"], 3);
    assert_eq!(
        event(&p.run(&["validate", "--json"]))["total_defined_jobs"],
        7
    );
    assert!(!p.dir.join("order").exists());
}

#[test]
fn child_settings_merge_shared_settings_per_key_without_caller_overrides() {
    std::fs::create_dir_all(std::env::temp_dir()).unwrap();
    let p = Project::new(
        r#"
[settings]
parallelism = 4
fail_fast = false
[settings.env]
KEEP = "shared"
X = "shared"
[workflows.ci.settings]
parallelism = 1
workdir = "caller"
[workflows.ci.settings.env]
X = "caller"
ONLY = "caller"
[[workflows.ci.jobs]]
name = "here"
command = "pwd > where; echo \"$KEEP:$X:${ONLY-unset}:${GOODFLOWS_ENVIRONMENT-none}\" > env"
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
[workflows.suite.settings]
workdir = "child"
[workflows.suite.settings.env]
X = "child"
[[workflows.suite.jobs]]
name = "here"
command = "pwd > where; echo \"$KEEP:$X:${ONLY-unset}:${GOODFLOWS_ENVIRONMENT-none}\" > env"
[environments.p.settings.env]
KEEP = "profile"
"#,
    );
    for dir in ["caller", "child"] {
        std::fs::create_dir_all(p.dir.join(dir)).unwrap();
    }
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    for (dir, env) in [
        ("caller", "shared:caller:caller:none"),
        ("child", "shared:child:unset:none"),
    ] {
        assert_eq!(
            p.read(&format!("{dir}/where")).trim(),
            p.dir.join(dir).to_str().unwrap()
        );
        assert_eq!(p.read(&format!("{dir}/env")).trim(), env);
    }
    let events = events(&output);
    let started: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "run_started")
        .collect();
    // The omitted child parallelism inherits shared 4, not the caller's 1.
    assert_eq!(
        (
            started[0]["workflow"].as_str(),
            started[0]["parallelism"].as_u64()
        ),
        (Some("ci"), Some(1))
    );
    assert_eq!(
        (
            started[1]["workflow"].as_str(),
            started[1]["parallelism"].as_u64()
        ),
        (Some("suite"), Some(4))
    );
    // The selected profile and CLI flags reach every invocation.
    let output = p.run(&["run", "ci", "--env", "p", "--parallelism", "2", "--json"]);
    success(&output);
    assert_eq!(p.read("caller/env").trim(), "profile:caller:caller:p");
    assert_eq!(p.read("child/env").trim(), "profile:child:unset:p");
    assert!(common::events(&output)
        .iter()
        .filter(|e| e["type"] == "run_started")
        .all(|e| e["parallelism"] == 2));
}

#[test]
fn profile_partitions_are_shared_by_job_name_across_reachable_workflows() {
    let config = |condition: &str| {
        format!(
            r#"
[[workflows.ci.jobs]]
name = "marker"
command = "touch marker"
[[workflows.ci.jobs]]
name = "test"
partitions = 2
command = "echo ci-$GOODFLOWS_ENVIRONMENT-$GOODFLOWS_PARTITION_COUNT >> counts"
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
[[workflows.suite.jobs]]
name = "test"
partitions = 2
command = "echo suite-$GOODFLOWS_ENVIRONMENT-$GOODFLOWS_PARTITION_COUNT >> counts"
[[workflows.suite.jobs]]
name = "maybe"
workflow = "plain"
condition = {{ expression = '{condition}' }}
[[workflows.plain.jobs]]
name = "test"
command = "true"
[[workflows.release.jobs]]
name = "call"
workflow = "suite"
[environments.local.partitions]
test = 3
"#
        )
    };
    // The reachable unpartitioned `test` fails the profile before any root
    // job runs, even behind a false call condition.
    let p = Project::new(&config("false"));
    for args in [
        &["run", "ci", "--env", "local", "--json"][..],
        &["list", "ci", "--env", "local", "--json"],
        &["validate", "ci", "--env", "local", "--json"],
        &["validate", "--json"],
    ] {
        assert_eq!(
            error_of(&p.run(args)),
            "environment 'local': workflow 'plain': job 'test' is not partitioned",
            "{args:?}"
        );
    }
    assert!(!p.dir.join("marker").exists() && !p.dir.join("counts").exists());
    // Without the profile the same graph runs, and the false call is skipped.
    success(&p.run(&["run", "ci"]));
    let mut counts: Vec<_> = p.read("counts").lines().map(str::to_owned).collect();
    counts.sort();
    assert_eq!(counts, ["ci--2", "ci--2", "suite--2", "suite--2"]);
    // Without the incompatible workflow, one profile count covers both jobs.
    let p = Project::new(&config("false").replace(
        "[[workflows.plain.jobs]]\nname = \"test\"\ncommand = \"true\"\n",
        "[[workflows.plain.jobs]]\nname = \"unrelated\"\ncommand = \"true\"\n",
    ));
    success(&p.run(&["run", "ci", "--env", "local"]));
    let mut counts: Vec<_> = p.read("counts").lines().map(str::to_owned).collect();
    counts.sort();
    let mut expected = vec!["ci-local-3"; 3];
    expected.extend(["suite-local-3"; 3]);
    assert_eq!(counts, expected);
}

#[test]
fn calls_order_preparation_nesting_and_repeated_executions() {
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[workflows.ci.jobs]]
name = "init"
command = "echo v1 > state; echo init >> order"
[[workflows.ci.jobs]]
name = "before"
workflow = "checks"
depends_on = ["init"]
[[workflows.ci.jobs]]
name = "mutate"
command = "echo v2 > state; echo mutate >> order"
depends_on = ["before"]
[[workflows.ci.jobs]]
name = "after"
workflow = "checks"
depends_on = ["mutate"]
[[workflows.ci.jobs]]
name = "publish"
command = "echo publish >> order"
depends_on = ["after"]
[[workflows.checks.jobs]]
name = "read"
command = "cat state >> seen; echo checks >> order"
[[workflows.checks.jobs]]
name = "nested"
workflow = "leaf"
depends_on = ["read"]
[[workflows.leaf.jobs]]
name = "leaf"
command = "echo leaf >> order"
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    assert_eq!(
        p.read("order"),
        "init\nchecks\nleaf\nmutate\nchecks\nleaf\npublish\n"
    );
    // The second execution observes the workspace change.
    assert_eq!(p.read("seen"), "v1\nv2\n");
    let events = events(&output);
    let root = summary_for(&events, "ci");
    let before = result_of(&root, "before")["child_run_id"].clone();
    let after = result_of(&root, "after")["child_run_id"].clone();
    assert!(before.is_string() && after.is_string() && before != after);
    let children: Vec<_> = summaries(&events)
        .into_iter()
        .filter(|s| s["workflow"] == "checks")
        .collect();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["run_id"], before);
    assert_eq!(children[0]["caller_job"], "before");
    assert_eq!(children[1]["run_id"], after);
    assert_eq!(
        summaries(&events)
            .iter()
            .filter(|s| s["workflow"] == "leaf")
            .count(),
        2
    );
}

#[test]
fn independent_jobs_are_not_gated_by_a_call_listed_first() {
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[workflows.ci.jobs]]
name = "call"
workflow = "slow"
[[workflows.ci.jobs]]
name = "free"
command = "touch free"
[[workflows.slow.jobs]]
name = "wait"
command = "i=0; while [ $i -lt 500 ] && ! test -f free; do sleep 0.01; i=$((i+1)); done; test -f free"
"#,
    );
    success(&p.run(&["run", "ci"]));
}

#[test]
fn a_false_call_condition_creates_no_child_state() {
    let p = Project::new(
        r#"
[[workflows.ci.jobs]]
name = "detect"
outputs = { run = "steps.scan.outputs.run" }
[[workflows.ci.jobs.steps]]
id = "scan"
command = "printf '{\"run\":false}' > \"$GOODFLOWS_OUTPUT\""
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
depends_on = ["detect"]
condition = { expression = "needs.detect.outputs.run" }
[[workflows.ci.jobs]]
name = "after"
command = "touch after"
depends_on = ["call"]
[[workflows.suite.jobs]]
name = "t"
command = "touch ran"
"#,
    );
    let output = p.run(&["run", "ci", "--json", "--history", "--log-dir", "logs"]);
    success(&output);
    let events = events(&output);
    assert!(
        events.iter().all(|e| e["workflow"] == "ci"),
        "{}",
        stdout(&output)
    );
    assert_eq!(summaries(&events).len(), 1);
    let root = summary_for(&events, "ci");
    let call = result_of(&root, "call");
    assert_eq!(call["status"], "skipped");
    assert_eq!(call["error"], "condition expression was false");
    assert_eq!(call["workflow"], "suite");
    assert!(call.get("child_run_id").is_none());
    assert_eq!(
        result_of(&root, "after")["error"],
        "dependency did not pass"
    );
    assert!(!p.dir.join("ran").exists() && !p.dir.join("after").exists());
    assert!(!p.dir.join("logs/suite").exists());
    assert!(!p.dir.join("goodflows-history/suite").exists());
    // A true output activates the child.
    let p = Project::new(&p.read("goodflows.toml").replace("false", "true"));
    success(&p.run(&["run", "ci"]));
    assert!(p.dir.join("ran").exists() && p.dir.join("after").exists());
}

#[test]
fn calls_complete_with_the_child_outcome() {
    let p = Project::new(
        r#"
[settings]
fail_fast = false
[[workflows.ci.jobs]]
name = "integration"
workflow = "suite"
[[workflows.ci.jobs]]
name = "publish"
command = "touch published"
depends_on = ["integration"]
[[workflows.ci.jobs]]
name = "optional"
workflow = "optional"
[[workflows.ci.jobs]]
name = "after-optional"
command = "touch after-optional"
depends_on = ["optional"]
[[workflows.suite.jobs]]
name = "test"
command = "exit 1"
[[workflows.optional.jobs]]
name = "ok"
command = "true"
[[workflows.optional.jobs]]
name = "never"
condition = { expression = "false" }
command = "touch never"
[[workflows.optional.jobs]]
name = "blocked"
depends_on = ["never"]
command = "touch blocked"
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let root = summary_for(&events, "ci");
    let integration = result_of(&root, "integration");
    assert_eq!(integration["status"], "failed");
    assert_eq!(integration["error"], "workflow 'suite' failed");
    assert!(integration["exit_code"].is_null() && integration["command"].is_null());
    assert!(integration["duration_ms"].is_u64());
    assert!(integration.get("commands").is_none());
    assert_eq!(
        result_of(&root, "publish")["error"],
        "dependency did not pass"
    );
    // Conditional skips in the child do not fail the call.
    assert_eq!(result_of(&root, "optional")["status"], "passed");
    assert!(p.dir.join("after-optional").exists());
    assert!(!p.dir.join("published").exists());
    assert_eq!(
        (
            root["passed"].as_u64(),
            root["failed"].as_u64(),
            root["skipped"].as_u64()
        ),
        (Some(2), Some(1), Some(1))
    );
    assert_eq!(root["outcome"], "failed");
    assert_eq!(summary_for(&events, "optional")["skipped"], 2);
    // The root summary is last, after every child summary.
    assert_eq!(events.last().unwrap()["type"], "summary");
    assert_eq!(events.last().unwrap()["workflow"], "ci");
    // Without any failure in the run, the child's skips still pass the call.
    let output = p.run(&["run", "ci", "--json", "--jobs", "after-optional"]);
    let events = common::events(&output);
    assert_eq!(output.status.code(), Some(0), "{events:?}");
    let root = summary_for(&events, "ci");
    assert_eq!(result_of(&root, "optional")["status"], "passed");
    assert_eq!(result_of(&root, "after-optional")["status"], "passed");
    assert_eq!(root["outcome"], "passed");
    assert_eq!(summary_for(&events, "optional")["skipped"], 2);
}

#[test]
fn child_setup_failure_fails_the_call_with_context_and_keeps_the_root_summary() {
    let p = Project::new(
        "[[workflows.ci.jobs]]\nname='call'\nworkflow='suite'\n\
         [[workflows.ci.jobs]]\nname='after'\ncommand='touch after'\ndepends_on=['call']\n\
         [[workflows.suite.jobs]]\nname='t'\ncommand='touch ran'\n",
    );
    std::fs::create_dir_all(p.dir.join("logs")).unwrap();
    // A file where the child's log namespace must be created.
    std::fs::write(p.dir.join("logs/suite"), "").unwrap();
    let output = p.run(&["run", "ci", "--json", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let root = events.last().unwrap();
    assert_eq!(
        (root["type"].as_str(), root["workflow"].as_str()),
        (Some("summary"), Some("ci"))
    );
    let call = result_of(root, "call");
    assert_eq!(call["status"], "failed");
    let error = call["error"].as_str().unwrap();
    assert!(
        error.starts_with("workflow 'suite': failed to create log directory"),
        "{error}"
    );
    // The child identity was allocated, though it never ran a job. As
    // documented, its run started but it has no summary.
    let child = call["child_run_id"].as_str().unwrap();
    let kinds: Vec<_> = events
        .iter()
        .filter(|e| e["invocation_id"] == child)
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["run_started"], "{events:?}");
    assert_eq!(result_of(root, "after")["error"], "dependency did not pass");
    assert!(!p.dir.join("ran").exists());
    let text = p.run(&["run", "ci", "--log-dir", "logs"]);
    assert_eq!(text.status.code(), Some(1));
    assert!(stdout(&text).contains("Rerun failed: goodflows run ci --jobs='call,after'"));
}

#[test]
fn child_wait_metrics_use_the_child_invocation_clock() {
    let p = Project::new(
        r#"
[workflows.ci.settings]
parallelism = 2
[[workflows.ci.jobs]]
name = "prepare"
command = "i=0; while [ $i -lt 3000 ] && ! test -f hog; do sleep 0.01; i=$((i+1)); done; sleep 0.3"
[[workflows.ci.jobs]]
name = "call"
workflow = "suite"
depends_on = ["prepare"]
# Holds the second slot from before the child starts until well after
# `second` is ready, however the invocations' threads are scheduled.
[[workflows.ci.jobs]]
name = "hog"
command = "touch hog; i=0; while [ $i -lt 3000 ] && ! test -f first; do sleep 0.01; i=$((i+1)); done; sleep 0.5"
[[workflows.suite.jobs]]
name = "first"
command = "sleep 0.2; touch first"
[[workflows.suite.jobs]]
name = "other"
command = "sleep 0.6"
[[workflows.suite.jobs]]
name = "second"
command = "true"
depends_on = ["first"]
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    let events = events(&output);
    let root = summary_for(&events, "ci");
    let child = summary_for(&events, "suite");
    let ms = |result: &serde_json::Value, key: &str| result[key].as_u64().unwrap();
    // The parent call waited for preparation on the parent's clock.
    let call = result_of(&root, "call");
    assert!(ms(&call, "blocked_ms") >= 250, "{call}");
    // An immediately ready child job excludes the parent's preparation.
    let first = result_of(&child, "first");
    assert!(ms(&first, "blocked_ms") < 150, "{first}");
    // Local dependency wait and the later shared-capacity wait are separate.
    let second = result_of(&child, "second");
    let (blocked, queued) = (ms(&second, "blocked_ms"), ms(&second, "queued_ms"));
    assert!((150..450).contains(&blocked), "{second}");
    assert!(queued >= 250, "{second}");
    assert!(
        blocked + queued < ms(&call, "duration_ms") + 50,
        "{second} {call}"
    );
}

#[test]
fn call_aware_events_identify_owners_targets_and_repeated_children() {
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[workflows.ci.jobs]]
name = "one"
workflow = "suite"
[[workflows.ci.jobs]]
name = "two"
workflow = "suite"
[[workflows.suite.jobs]]
name = "test"
command = "echo hi; touch started.$$; i=0; while [ $i -lt 300 ] && [ $(ls started.* | wc -l) -lt 2 ]; do sleep 0.01; i=$((i+1)); done"
"#,
    );
    let output = p.run(&["run", "ci", "--json"]);
    success(&output);
    let events = events(&output);
    let root = events.last().unwrap();
    assert_eq!(root["workflow"], "ci");
    let root_id = root["run_id"].as_str().unwrap();
    let mut children = Vec::new();
    for call in ["one", "two"] {
        let result = result_of(root, call);
        // The result names the target; the event's workflow is the owner.
        assert_eq!(result["workflow"], "suite");
        let child = result["child_run_id"].as_str().unwrap().to_owned();
        let owned: Vec<_> = events
            .iter()
            .filter(|e| e["invocation_id"] == child.as_str())
            .collect();
        assert!(owned.len() >= 5, "{owned:?}");
        for event in &owned {
            assert_eq!(event["workflow"], "suite");
            assert_eq!(event["parent_run_id"], root_id);
            assert_eq!(event["caller_job"], call);
        }
        let summary = owned.last().unwrap();
        assert_eq!(
            (summary["type"].as_str(), summary["run_id"].as_str()),
            (Some("summary"), Some(child.as_str()))
        );
        assert_eq!(summary["passed"], 1);
        children.push(child);
    }
    assert_ne!(children[0], children[1]);
    // Root events carry the root identity only.
    for event in events.iter().filter(|e| e["workflow"] == "ci") {
        assert_eq!(event["invocation_id"], root_id, "{event}");
        assert!(event.get("parent_run_id").is_none() && event.get("caller_job").is_none());
    }
    assert_eq!(
        (
            root["passed"].as_u64(),
            root["results"].as_array().unwrap().len()
        ),
        (Some(2), 2)
    );

    // Text lines name the owning workflow and a short invocation id.
    let text = p.run(&["run", "ci", "--output=stream"]);
    success(&text);
    let text = stdout(&text);
    let labels: std::collections::BTreeSet<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("[suite ["))
        .filter_map(|rest| rest.strip_suffix("] test stdout] hi"))
        .collect();
    assert_eq!(labels.len(), 2, "{text}");
    assert!(labels.iter().all(|id| id.len() == 8), "{labels:?}");
    for id in &labels {
        assert!(text.contains(&format!("→ suite [{id}] test\n")), "{text}");
        assert!(
            text.contains(&format!("suite [{id}] test PASSED")),
            "{text}"
        );
    }
    let last_report = text.rfind("\nci [").unwrap();
    assert!(text[last_report..].contains("  PASS  one"), "{text}");
    // No child line or report follows the root report.
    assert!(!text[last_report..].contains("\nsuite ["), "{text}");
}

#[test]
fn root_rerun_hint_reruns_preparation_before_the_failed_child() {
    let config = |name: &str| {
        format!(
            r#"
[[workflows."{name}".jobs]]
name = "prepare"
command = "echo prepare >> order"
[[workflows."{name}".jobs]]
name = "integ ration"
workflow = "suite"
depends_on = ["prepare"]
[[workflows."{name}".jobs]]
name = "publish"
command = "echo publish >> order"
depends_on = ["integ ration"]
[[workflows.suite.jobs]]
name = "test"
command = "echo test >> order; test -f second || {{ touch second; exit 1; }}"
"#
        )
    };
    let p = Project::new(&config("my ci"));
    let output = p.run(&["run", "my ci"]);
    assert_eq!(output.status.code(), Some(1));
    let text = stdout(&output);
    let hint = text.lines().last().unwrap();
    assert_eq!(
        hint,
        "Rerun failed: goodflows run 'my ci' --jobs='integ ration,publish'"
    );
    // Only the root report offers a rerun.
    assert_eq!(text.matches("Rerun").count(), 1, "{text}");
    let command = hint.strip_prefix("Rerun failed: goodflows ").unwrap();
    let rerun = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("\"$GOODFLOWS\" {command}"))
        .env("GOODFLOWS", env!("CARGO_BIN_EXE_goodflows"))
        .current_dir(&p.dir)
        .output()
        .unwrap();
    success(&rerun);
    assert_eq!(p.read("order"), "prepare\ntest\nprepare\ntest\npublish\n");

    // A comma in a call name falls back to the whole root workflow.
    let p = Project::new(&config("ci").replace("integ ration", "in,tegration"));
    let output = p.run(&["run", "ci"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(&output).ends_with("Selective rerun unavailable: job 'in,tegration' contains a comma, which --jobs cannot select.\nRerun workflow: goodflows run ci\n"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn repeated_target_logs_are_isolated_and_wrappers_write_none() {
    let p = Project::new(
        r#"
[settings]
parallelism = 4
[[workflows.ci.jobs]]
name = "one"
workflow = "suite"
[[workflows.ci.jobs]]
name = "two"
workflow = "suite"
condition = { shell = "echo condition" }
[[workflows.suite.jobs]]
name = "test"
command = "echo $$ > pid.$$; touch started.$$; i=0; while [ $i -lt 300 ] && [ $(ls started.* | wc -l) -lt 2 ]; do sleep 0.01; i=$((i+1)); done; echo out-$$"
"#,
    );
    // Concurrent children with a cap of one: neither live directory is pruned.
    let output = p.run(&[
        "run",
        "ci",
        "--json",
        "--log-dir",
        "logs",
        "--log-runs",
        "1",
    ]);
    success(&output);
    let events = events(&output);
    let mut outputs = Vec::new();
    for summary in summaries(&events)
        .iter()
        .filter(|s| s["workflow"] == "suite")
    {
        let dir = p.dir.join(summary["log_dir"].as_str().unwrap());
        assert!(
            dir.starts_with(p.dir.join("logs/suite")),
            "{}",
            dir.display()
        );
        let log = std::fs::read_to_string(dir.join("001-test.stdout.log")).unwrap();
        assert!(log.starts_with("out-"), "{log}");
        outputs.push(log);
    }
    assert_eq!(outputs.len(), 2);
    assert_ne!(outputs[0], outputs[1]);
    // The root run directory holds only the shell condition's ordinary logs.
    let root = p
        .dir
        .join(events.last().unwrap()["log_dir"].as_str().unwrap());
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".goodflows.lock",
            "002-two.stderr.log",
            "002-two.stdout.log"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("002-two.stdout.log")).unwrap(),
        "condition\n"
    );
}
