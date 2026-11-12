mod common;
use common::*;

#[test]
fn selection_includes_transitive_dependencies() {
    let p = Project::new(
        r#"
[[jobs]]
name = "setup"
command = "echo setup >> order"
[[jobs]]
name = "build"
command = "echo build >> order"
depends_on = ["setup"]
[[jobs]]
name = "test"
command = "echo test >> order"
depends_on = ["build"]
[[jobs]]
name = "other"
command = "touch other"
"#,
    );
    success(&p.run(&["--jobs", "test"]));
    assert_eq!(p.read("order"), "setup\nbuild\ntest\n");
    assert!(!p.dir.join("other").exists());
    let output = p.run(&["--jobs", "test,typo"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown job 'typo'"));
}

#[test]
fn validation_rejects_invalid_configs_before_commands_execute() {
    let cases = [
        ("", "no jobs"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\n[[jobs]]\nname='a'\ncommand='true'", "duplicate job"),
        ("[[jobs]]\nname=' '\ncommand='touch ran'", "empty job name"),
        ("[[jobs]]\nname='a'\ncommand=' '", "empty command"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ncondition=' '", "condition must be nonempty"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ncondition=\"true\\u0000\"", "condition must be nonempty"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ncondition=true", "untagged enum Condition"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ndepends_on=['missing']", "unknown job"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ndepends_on=['a']", "itself"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ndepends_on=['b']\n[[jobs]]\nname='b'\ncommand='true'\ndepends_on=['a']", "cycle"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ntimeout_secs=0", "timeout"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ntimout_secs=1", "unknown field"),
        ("[settings]\nfail_fats=true\n[[jobs]]\nname='a'\ncommand='touch ran'", "unknown field"),
        ("oops=true\n[[jobs]]\nname='a'\ncommand='touch ran'", "unknown field"),
        ("[[jobs]]\nname='a'\ncommand=\"echo 'unclosed\"\nshell=false", "quot"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\ndepends_on=['b','b']\n[[jobs]]\nname='b'\ncommand='true'", "duplicate dependency"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\nenv={'BAD=NAME'='value'}", "invalid environment"),
        ("[[jobs]]\nname='a'\ncommand='touch ran'\nenv={''='value'}", "invalid environment"),
    ];
    for (config, error) in cases {
        let p = Project::new(config);
        for args in [&[][..], &["validate"][..], &["list"][..]] {
            let output = p.run(args);
            assert!(!output.status.success(), "accepted {config:?} {args:?}");
            assert!(
                stderr(&output).contains(error),
                "expected {error}: {}",
                stderr(&output)
            );
            assert!(!p.dir.join("ran").exists());
        }
    }
}

#[test]
fn validate_does_not_execute_jobs_and_validates_unselected_jobs() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='touch ran'");
    success(&p.run(&["validate"]));
    assert!(!p.dir.join("ran").exists());
    let output = p.run(&["validate", "--jobs", "typo"]);
    assert!(!output.status.success());
    let p = Project::new("[[jobs]]\nname='a'\ncommand='touch ran'\n[[jobs]]\nname='b'\ncommand='true'\ndepends_on=['missing']");
    assert!(!p.run(&["--jobs", "a"]).status.success());
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn nested_config_resolves_relative_workdir_from_config_directory() {
    let p = Project::new("");
    std::fs::create_dir_all(p.dir.join("ci/app")).unwrap();
    std::fs::write(
        p.dir.join("ci/checks.toml"),
        "[settings]\nworkdir='app'\n[[jobs]]\nname='a'\ncommand='pwd > location'",
    )
    .unwrap();
    success(&p.run(&["--config", "ci/checks.toml"]));
    assert_eq!(
        p.read("ci/app/location").trim(),
        p.dir
            .join("ci/app")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    std::fs::write(
        p.dir.join("ci/checks.toml"),
        "[settings]\nworkdir='missing'\n[[jobs]]\nname='a'\ncommand='true'",
    )
    .unwrap();
    assert!(!p
        .run(&["validate", "--config", "ci/checks.toml"])
        .status
        .success());
}

#[test]
fn contradictory_fail_fast_flags_are_rejected() {
    let p = Project::new("[[jobs]]\nname='a'\ncommand='touch ran'");
    let output = p.run(&["--fail-fast", "--no-fail-fast"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("cannot be used with"));
    assert!(!p.dir.join("ran").exists());
}

#[test]
fn direct_exec_preserves_quoted_arguments() {
    let p = Project::new(
        r#"[[jobs]]
name = "direct"
shell = false
command = "printf '%s' 'hello world'"
"#,
    );
    let output = p.run(&["--output=stream"]);
    success(&output);
    assert!(stdout(&output).contains("hello world"));
}

#[test]
fn commands_inherit_env_and_direct_spawn_failure_preserves_diagnostics() {
    let p = Project::new("[[jobs]]\nname='env'\ncommand=\"printf '%s' \\\"$CI_TEST_VALUE\\\" > value\"\nenv={CI_TEST_VALUE='hello world'}");
    success(&p.run(&[]));
    assert_eq!(p.read("value"), "hello world");
    let p = Project::new(
        "[[jobs]]\nname='spawn'\ncommand='goodflows-nonexistent-program-7f98'\nshell=false",
    );
    let output = p.run(&["--json"]);
    assert!(!output.status.success());
    let summary: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
    assert_eq!(summary["results"][0]["status"], "failed");
    assert!(summary["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("failed to spawn"));
}

#[test]
fn each_instance_has_its_own_timeout_budget_and_failure_tails() {
    // Parallelism 1 runs the instances back to back. Instance 3 starts after
    // instance 2 has used its whole 1s budget, so its passing proves the budget is
    // per instance; each passing instance stays far below 1s to avoid flakes.
    let p = Project::new(
        r#"
[settings]
parallelism = 1
fail_fast = false
[[jobs]]
name = "test"
partitions = 3
timeout_secs = 1
command = "printf 'out %s' $GOODFLOWS_PARTITION; if [ $GOODFLOWS_PARTITION = 2 ]; then sleep 5; else sleep 0.35; fi"
"#,
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events = events(&output);
    let results = events.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .clone();
    let statuses: Vec<_> = results
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["passed", "timed_out", "passed"]);
    assert_eq!(results[1]["error"], "timed out after 1s");
    assert_eq!(results[1]["stdout_tail"], "out 2");
    for index in [0, 2] {
        assert!(results[index].get("stdout_tail").is_none());
    }
}

const PARTITION_PROBE: &str =
    r#"printf '%s/%s' "$GOODFLOWS_PARTITION" "$GOODFLOWS_PARTITION_COUNT""#;

#[test]
fn instances_receive_partition_identity_in_every_command_and_condition() {
    let p = Project::new(&format!(
        r#"
[settings]
parallelism = 1
[[jobs]]
name = "shell"
partitions = 3
condition = {{ shell = '''{PARTITION_PROBE} > cond.$GOODFLOWS_PARTITION''' }}
command = '''{PARTITION_PROBE} > shell.$GOODFLOWS_PARTITION'''
[[jobs]]
name = "direct"
partitions = 2
shell = false
command = '''sh -c 'printf %s/%s "$GOODFLOWS_PARTITION" "$GOODFLOWS_PARTITION_COUNT" > direct.$GOODFLOWS_PARTITION' '''
[[jobs]]
name = "stepped"
partitions = {{ count = 2 }}
env = {{ OTHER = "x" }}
[[jobs.steps]]
id = "a"
condition = {{ shell = '''{PARTITION_PROBE} > stepcond.$GOODFLOWS_PARTITION''' }}
command = '''{PARTITION_PROBE} > step.$GOODFLOWS_PARTITION'''
[[jobs]]
name = "first"
partitions = 3
condition = {{ expression = 'env.GOODFLOWS_PARTITION == "1" && env.GOODFLOWS_PARTITION_COUNT == "3"' }}
command = "touch first.$GOODFLOWS_PARTITION"
"#,
    ));
    // Inherited values must be overridden for instances.
    let output = p
        .command()
        .env("GOODFLOWS_PARTITION", "99")
        .env("GOODFLOWS_PARTITION_COUNT", "99")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    for index in 1..=3 {
        assert_eq!(p.read(&format!("shell.{index}")), format!("{index}/3"));
        assert_eq!(p.read(&format!("cond.{index}")), format!("{index}/3"));
    }
    for index in 1..=2 {
        assert_eq!(p.read(&format!("direct.{index}")), format!("{index}/2"));
        assert_eq!(p.read(&format!("step.{index}")), format!("{index}/2"));
        assert_eq!(p.read(&format!("stepcond.{index}")), format!("{index}/2"));
    }
    assert!(p.dir.join("first.1").exists());
    assert!(!p.dir.join("first.2").exists() && !p.dir.join("first.3").exists());
    let events = events(&output);
    let results = events.last().unwrap()["results"]
        .as_array()
        .unwrap()
        .clone();
    let first: Vec<_> = results.iter().filter(|r| r["name"] == "first").collect();
    assert_eq!(first[0]["status"], "passed");
    for skipped in &first[1..] {
        assert_eq!(skipped["status"], "skipped");
        assert_eq!(skipped["error"], "condition expression was false");
    }
}

#[test]
fn selected_profile_reaches_commands_and_unselected_runs_hide_it() {
    let p = Project::new(
        r#"
[environments.local.settings]
parallelism = 1
[[jobs]]
name = "seen"
condition = { expression = 'env.GOODFLOWS_ENVIRONMENT == "local"' }
command = 'test "$GOODFLOWS_ENVIRONMENT" = local && touch seen'
[[jobs]]
name = "direct"
shell = false
command = "sh -c 'test \"$GOODFLOWS_ENVIRONMENT\" = local && touch direct'"
[[jobs]]
name = "stepped"
[[jobs.steps]]
id = "a"
condition = { shell = 'test "$GOODFLOWS_ENVIRONMENT" = local' }
command = 'test "$GOODFLOWS_ENVIRONMENT" = local && touch stepped'
"#,
    );
    let output = p
        .command()
        .args(["--env", "local", "--json"])
        .output()
        .unwrap();
    success(&output);
    for file in ["seen", "direct", "stepped"] {
        assert!(p.dir.join(file).exists(), "{file}: {}", stdout(&output));
    }

    let hidden = Project::new(
        r#"
[[jobs]]
name = "plain"
condition = { expression = 'env.GOODFLOWS_ENVIRONMENT == null' }
command = 'test -z "${GOODFLOWS_ENVIRONMENT+x}" && touch plain'
[[jobs]]
name = "direct"
shell = false
command = "sh -c 'test -z \"${GOODFLOWS_ENVIRONMENT+x}\" && touch direct'"
"#,
    );
    let output = hidden
        .command()
        .env("GOODFLOWS_ENVIRONMENT", "orb")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    for file in ["plain", "direct"] {
        assert!(
            hidden.dir.join(file).exists(),
            "{file}: {}",
            stdout(&output)
        );
    }
}

#[test]
fn unpartitioned_jobs_never_see_inherited_partition_variables() {
    let p = Project::new(
        r#"
[[jobs]]
name = "plain"
condition = { expression = 'env.GOODFLOWS_PARTITION == null && env.GOODFLOWS_PARTITION_COUNT == null' }
command = 'test -z "${GOODFLOWS_PARTITION+x}" && test -z "${GOODFLOWS_PARTITION_COUNT+x}" && touch plain'
[[jobs]]
name = "direct"
shell = false
command = "sh -c 'test -z \"${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}\" && touch direct'"
[[jobs]]
name = "stepped"
[[jobs.steps]]
id = "a"
condition = { shell = 'test -z "${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}"' }
command = 'test -z "${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}" && touch stepped'
"#,
    );
    let output = p
        .command()
        .env("GOODFLOWS_PARTITION", "2")
        .env("GOODFLOWS_PARTITION_COUNT", "3")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    for file in ["plain", "direct", "stepped"] {
        assert!(p.dir.join(file).exists(), "{file}: {}", stdout(&output));
    }
}

#[test]
fn instances_keep_partition_variables_when_a_reporting_block_exists() {
    let p = Project::new(&format!(
        r#"
[reporting]
project = "goodcases"
[[jobs]]
name = "test"
partitions = 2
condition = {{ expression = 'env.GOODFLOWS_PARTITION != null' }}
command = '''{PARTITION_PROBE} > out.$GOODFLOWS_PARTITION'''
"#
    ));
    let output = p.run(&["--json"]);
    success(&output);
    assert_eq!(p.read("out.1"), "1/2");
    assert_eq!(p.read("out.2"), "2/2");
}

#[test]
fn each_instance_writes_its_own_logs() {
    let p = Project::new(
        r#"
[[jobs]]
name = "compile"
command = "true"
[[jobs]]
name = "test"
partitions = 3
command = "printf 'out %s' $GOODFLOWS_PARTITION; printf 'err %s' $GOODFLOWS_PARTITION >&2"
"#,
    );
    success(&p.run(&["--log-dir", "logs"]));
    let run = run_dir(&p.dir.join("logs"));
    assert_eq!(
        log_names(&run),
        [
            "001-compile.stderr.log",
            "001-compile.stdout.log",
            "002-test.p01.stderr.log",
            "002-test.p01.stdout.log",
            "002-test.p02.stderr.log",
            "002-test.p02.stdout.log",
            "002-test.p03.stderr.log",
            "002-test.p03.stdout.log",
        ]
    );
    let read = |name: String| std::fs::read_to_string(run.join(name)).unwrap();
    for index in 1..=3 {
        assert_eq!(
            read(format!("002-test.p{index:02}.stdout.log")),
            format!("out {index}")
        );
        assert_eq!(
            read(format!("002-test.p{index:02}.stderr.log")),
            format!("err {index}")
        );
    }
    // A later run keeps the first run's six instance logs in their directory.
    let first: Vec<_> = log_names(&run)
        .into_iter()
        .filter(|name| name.starts_with("002-"))
        .map(|name| (name.clone(), read(name)))
        .collect();
    assert_eq!(first.len(), 6);
    success(&p.run(&["--log-dir", "logs", "--log-runs", "2"]));
    let runs = run_dirs(&p.dir.join("logs"));
    assert_eq!(runs.len(), 2);
    assert!(runs.contains(&run));
    for (name, content) in &first {
        assert_eq!(&read(name.clone()), content);
    }
    let second = runs.iter().find(|dir| **dir != run).unwrap();
    let second_logs: Vec<_> = log_names(second)
        .into_iter()
        .filter(|name| name.starts_with("002-"))
        .collect();
    assert_eq!(second_logs.len(), 6);
}

#[test]
fn settings_env_sits_between_caller_and_job_and_step_env() {
    let p = Project::new(
        r#"
[settings.env]
MIX_ENV = "test"
VALUE = "settings"
[[jobs]]
name = "shell"
command = 'printf %s "$MIX_ENV" > shell.out'
[[jobs]]
name = "direct"
shell = false
command = "sh -c 'printf %s \"$MIX_ENV\" > direct.out'"
[[jobs]]
name = "stepped"
env = { VALUE = "job" }
[[jobs.steps]]
id = "override"
env = { VALUE = "step" }
command = 'printf %s "$VALUE" > step1.out'
[[jobs.steps]]
id = "after"
command = 'printf %s "$VALUE" > step2.out'
[[jobs]]
name = "plain"
command = 'printf %s "$VALUE" > plain.out'
"#,
    );
    let output = p
        .command()
        .env("MIX_ENV", "dev")
        .env("VALUE", "caller")
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("shell.out"), "test");
    assert_eq!(p.read("direct.out"), "test");
    assert_eq!(p.read("step1.out"), "step");
    assert_eq!(p.read("step2.out"), "job");
    assert_eq!(p.read("plain.out"), "settings");
}

#[test]
fn without_settings_env_commands_see_the_caller_environment_unchanged() {
    let p = Project::new(
        r#"
[[jobs]]
name = "shell"
env = { JOB = "job" }
command = 'printf %s/%s "$MIX_ENV" "$JOB" > shell.out'
"#,
    );
    let output = p.command().env("MIX_ENV", "dev").output().unwrap();
    success(&output);
    assert_eq!(p.read("shell.out"), "dev/job");
}

#[test]
fn conditions_and_job_outputs_observe_settings_and_profile_env() {
    let p = Project::new(
        r#"
[settings.env]
VALUE = "settings"
[environments.local.settings.env]
RUN_CHECKS = "1"
[[jobs]]
name = "expression"
condition = { expression = 'env.RUN_CHECKS == "1"' }
command = 'test "$RUN_CHECKS" = 1 && touch expression'
[[jobs]]
name = "shell"
condition = { shell = 'test "$RUN_CHECKS" = 1' }
command = 'test "$RUN_CHECKS" = 1 && touch shell'
[[jobs]]
name = "stepped"
outputs = { seen = "env.VALUE" }
[[jobs.steps]]
id = "a"
env = { VALUE = "step" }
condition = { expression = 'env.VALUE == "step"' }
command = 'test "$VALUE" = step && touch step-expression'
[[jobs.steps]]
id = "b"
env = { VALUE = "step" }
condition = { shell = 'test "$VALUE" = step' }
command = 'test "$VALUE" = step && touch step-shell'
[[jobs]]
name = "consumer"
depends_on = ["stepped"]
condition = { expression = 'needs.stepped.outputs.seen == "settings"' }
command = "touch consumer"
"#,
    );
    let output = p
        .command()
        .env("RUN_CHECKS", "0")
        .args(["--env", "local", "--json"])
        .output()
        .unwrap();
    success(&output);
    for file in [
        "expression",
        "shell",
        "step-expression",
        "step-shell",
        "consumer",
    ] {
        assert!(p.dir.join(file).exists(), "{file}: {}", stdout(&output));
    }
    let summary = events(&output).last().unwrap().clone();
    let stepped = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "stepped")
        .unwrap()
        .clone();
    assert_eq!(stepped["outputs"], serde_json::json!({"seen":"settings"}));
    // Without the profile, the conditions observe the caller's value and skip.
    let output = p
        .command()
        .env("RUN_CHECKS", "0")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    let summary = events(&output).last().unwrap().clone();
    for name in ["expression", "shell"] {
        let result = summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap()
            .clone();
        assert_eq!(result["status"], "skipped", "{name}");
    }
}

#[test]
fn runner_owned_variables_stay_authoritative_with_settings_env() {
    let p = Project::new(
        r#"
[settings.env]
SHARED = "yes"
[environments.local.settings.env]
PROFILE = "yes"
[[jobs]]
name = "split"
partitions = 2
condition = { expression = 'env.GOODFLOWS_PARTITION != null && env.GOODFLOWS_PARTITION_COUNT == "2" && env.GOODFLOWS_ENVIRONMENT == "local" && env.SHARED == "yes"' }
command = 'printf %s/%s/%s "$GOODFLOWS_PARTITION" "$GOODFLOWS_PARTITION_COUNT" "$GOODFLOWS_ENVIRONMENT" > split.$GOODFLOWS_PARTITION'
[[jobs]]
name = "plain"
condition = { expression = 'env.GOODFLOWS_PARTITION == null && env.GOODFLOWS_PARTITION_COUNT == null && env.GOODFLOWS_ENVIRONMENT == "local"' }
outputs = { profile = "env.GOODFLOWS_ENVIRONMENT" }
[[jobs.steps]]
id = "a"
condition = { shell = 'test -z "${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}" && test "$GOODFLOWS_ENVIRONMENT" = local' }
command = 'test -z "${GOODFLOWS_PARTITION+x}${GOODFLOWS_PARTITION_COUNT+x}" && test "$GOODFLOWS_ENVIRONMENT" = local && test -n "$GOODFLOWS_OUTPUT" && test "$GOODFLOWS_OUTPUT" != /nonexistent/outer && test "$PROFILE" = yes && touch plain'
"#,
    );
    let run = |args: &[&str]| {
        p.command()
            .env("GOODFLOWS_PARTITION", "9")
            .env("GOODFLOWS_PARTITION_COUNT", "9")
            .env("GOODFLOWS_ENVIRONMENT", "outer")
            .env("GOODFLOWS_OUTPUT", "/nonexistent/outer")
            .args(args)
            .output()
            .unwrap()
    };
    let output = run(&["--env", "local", "--json"]);
    success(&output);
    assert_eq!(p.read("split.1"), "1/2/local");
    assert_eq!(p.read("split.2"), "2/2/local");
    assert!(p.dir.join("plain").exists());
    let summary = events(&output).last().unwrap().clone();
    let plain = summary["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "plain")
        .unwrap()
        .clone();
    // Job output expressions see the profile name.
    assert_eq!(plain["outputs"]["profile"], "local");

    // Without a profile, inherited GOODFLOWS_ENVIRONMENT is removed everywhere.
    let p = Project::new(
        r#"
[settings.env]
SHARED = "yes"
[[jobs]]
name = "plain"
condition = { expression = 'env.GOODFLOWS_ENVIRONMENT == null && env.SHARED == "yes"' }
outputs = { profile = "env.GOODFLOWS_ENVIRONMENT ?? \"none\"" }
[[jobs.steps]]
id = "a"
condition = { shell = 'test -z "${GOODFLOWS_ENVIRONMENT+x}"' }
command = 'test -z "${GOODFLOWS_ENVIRONMENT+x}" && touch hidden'
"#,
    );
    let output = p
        .command()
        .env("GOODFLOWS_ENVIRONMENT", "outer")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    assert!(p.dir.join("hidden").exists());
    let summary = events(&output).last().unwrap().clone();
    assert_eq!(summary["results"][0]["outputs"]["profile"], "none");
}

#[test]
fn inherited_goodflows_output_never_reaches_expressions() {
    let p = Project::new(
        r#"
[[jobs]]
name = "command"
condition = { expression = 'env.GOODFLOWS_OUTPUT == null' }
command = 'test -n "$GOODFLOWS_OUTPUT" && test "$GOODFLOWS_OUTPUT" != /nonexistent/outer'
[[jobs]]
name = "stepped"
outputs = { path = "env.GOODFLOWS_OUTPUT ?? \"none\"" }
[[jobs.steps]]
id = "a"
condition = { expression = 'env.GOODFLOWS_OUTPUT == null' }
command = 'test -n "$GOODFLOWS_OUTPUT" && test "$GOODFLOWS_OUTPUT" != /nonexistent/outer'
[[jobs]]
name = "explicit"
env = { GOODFLOWS_OUTPUT = "job" }
outputs = { path = "env.GOODFLOWS_OUTPUT ?? \"none\"" }
[[jobs.steps]]
id = "a"
command = 'test "$GOODFLOWS_OUTPUT" != job'
"#,
    );
    let output = p
        .command()
        .env("GOODFLOWS_OUTPUT", "/nonexistent/outer")
        .arg("--json")
        .output()
        .unwrap();
    success(&output);
    let summary = events(&output).last().unwrap().clone();
    let results = summary["results"].as_array().unwrap();
    let path = |name: &str| {
        let result = results.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(result["status"], "passed", "{name}: {result}");
        result["outputs"]["path"].clone()
    };
    // The command-only job's condition and command checks passed.
    path("command");
    assert_eq!(path("stepped"), "none");
    // An explicit job env value keeps its existing expression behavior.
    assert_eq!(path("explicit"), "job");
}

#[test]
fn job_shell_conditions_see_profile_and_settings_but_not_the_token() {
    let config = |condition: &str| {
        format!(
            r#"
[reporting]
project = "goodcases"
[settings.env]
SHARED = "yes"
[environments.local.settings]
fail_fast = true
[[jobs]]
name = "guarded"
condition = {{ shell = '{condition}' }}
command = "touch ran"
"#
        )
    };
    let run = |p: &Project, args: &[&str]| {
        p.command()
            .env("GOODFLOWS_API_KEY", "secret-value")
            .env("GOODFLOWS_ENVIRONMENT", "outer")
            .args(args)
            .output()
            .unwrap()
    };
    let p = Project::new(&config(
        r#"test "$GOODFLOWS_ENVIRONMENT" = local && test -z "${GOODFLOWS_API_KEY+x}" && test "$SHARED" = yes"#,
    ));
    success(&run(&p, &["--env", "local"]));
    assert!(p.dir.join("ran").exists());
    // Without --env, the inherited profile name is removed as well.
    let p = Project::new(&config(
        r#"test -z "${GOODFLOWS_ENVIRONMENT+x}" && test -z "${GOODFLOWS_API_KEY+x}" && test "$SHARED" = yes"#,
    ));
    success(&run(&p, &[]));
    assert!(p.dir.join("ran").exists());
}

/// Declared count 3; the `local` profile raises it to 6 and `one` lowers it to 1.
fn profile_count_project(extra: &str) -> Project {
    Project::new(&format!(
        r#"
[settings]
parallelism = 5
{extra}
[[jobs]]
name = "test"
partitions = {{ count = 3, strategy = "index" }}
command = '''{PARTITION_PROBE} > out.$GOODFLOWS_PARTITION; printf 'out %s' $GOODFLOWS_PARTITION; printf 'err %s' $GOODFLOWS_PARTITION >&2'''
[environments.local.partitions]
test = 6
[environments.one.partitions]
test = 1
[environments.other.settings]
fail_fast = false
"#
    ))
}

fn partition_outputs(p: &Project) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(&p.dir)
        .unwrap()
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("out."))
        .collect();
    names.sort();
    names.iter().map(|name| p.read(name)).collect()
}

#[test]
fn selected_profile_count_drives_instances_identity_and_logs() {
    let p = profile_count_project("");
    let output = p.run(&["--json", "--env", "local", "--log-dir", "logs"]);
    success(&output);
    assert_eq!(
        partition_outputs(&p),
        (1..=6).map(|i| format!("{i}/6")).collect::<Vec<_>>()
    );
    let events = events(&output);
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[0]["jobs"], 6);
    // Started events tag the instance; finished events carry it in the result.
    for (kind, path) in [
        ("job_started", "/partition"),
        ("job_finished", "/result/partition"),
    ] {
        let tagged: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == kind)
            .map(|e| e.pointer(path).cloned().unwrap_or_default())
            .collect();
        assert_eq!(tagged.len(), 6, "{kind}");
        for partition in tagged {
            assert_eq!(partition["count"], 6, "{kind}");
        }
    }
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "summary");
    assert_eq!(summary["passed"], 6);
    let results = summary["results"].as_array().unwrap();
    assert_eq!(results.len(), 6);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            result["partition"],
            serde_json::json!({"index": index + 1, "count": 6})
        );
    }
    let run = run_dir(&p.dir.join("logs"));
    let names = log_names(&run);
    let expected: Vec<_> = (1..=6)
        .flat_map(|i| {
            [
                format!("001-test.p{i:02}.stderr.log"),
                format!("001-test.p{i:02}.stdout.log"),
            ]
        })
        .collect();
    assert_eq!(names, expected);
    let read = |name: String| std::fs::read_to_string(run.join(name)).unwrap();
    for index in 1..=6 {
        assert_eq!(
            read(format!("001-test.p{index:02}.stdout.log")),
            format!("out {index}")
        );
        assert_eq!(
            read(format!("001-test.p{index:02}.stderr.log")),
            format!("err {index}")
        );
    }
}

#[test]
fn selected_profile_count_drives_step_identities() {
    let p = Project::new(&format!(
        r#"
[[jobs]]
name = "stepped"
partitions = {{ count = 3, strategy = "index" }}
[[jobs.steps]]
id = "a"
command = '''{PARTITION_PROBE} > a.$GOODFLOWS_PARTITION'''
[[jobs.steps]]
id = "b"
command = "true"
[environments.local.partitions]
stepped = 6
"#
    ));
    let output = p.run(&["--json", "--env", "local"]);
    success(&output);
    for index in 1..=6 {
        assert_eq!(p.read(&format!("a.{index}")), format!("{index}/6"));
    }
    let events = events(&output);
    for kind in ["step_started", "step_finished"] {
        let mut indexes: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == kind)
            .map(|e| {
                assert_eq!(e["partition"]["count"], 6, "{kind}: {e}");
                e["partition"]["index"].as_u64().unwrap()
            })
            .collect();
        indexes.sort();
        let expected: Vec<u64> = (1..=6).flat_map(|i| [i, i]).collect();
        assert_eq!(indexes, expected, "{kind}");
    }
}

#[test]
fn profile_count_of_one_and_declared_count_without_override() {
    let p = profile_count_project("");
    success(&p.run(&["--env", "one"]));
    assert_eq!(partition_outputs(&p), ["1/1"]);
    for args in [&["--env", "other"][..], &[][..]] {
        let p = profile_count_project("");
        success(&p.run(args));
        assert_eq!(partition_outputs(&p), ["1/3", "2/3", "3/3"], "{args:?}");
    }
}

#[test]
fn profile_count_does_not_change_parallelism() {
    // Each instance records how many instances are running alongside it.
    let p = Project::new(
        r#"
[[jobs]]
name = "test"
partitions = 3
command = "touch running.$GOODFLOWS_PARTITION; sleep 0.3; ls running.* | wc -l > seen.$GOODFLOWS_PARTITION; rm running.$GOODFLOWS_PARTITION"
[environments.local.partitions]
test = 6
"#,
    );
    let output = p.run(&["--json", "--env", "local", "--parallelism", "2"]);
    success(&output);
    let events = events(&output);
    assert_eq!(events[0]["parallelism"], 2);
    let mut running = 0;
    let mut peak = 0;
    for event in &events {
        match event["type"].as_str() {
            Some("job_started") => running += 1,
            Some("job_finished") => running -= 1,
            _ => continue,
        }
        peak = peak.max(running);
    }
    assert_eq!(peak, 2);
    for index in 1..=6 {
        let seen: u32 = p.read(&format!("seen.{index}")).trim().parse().unwrap();
        assert!(seen <= 2, "instance {index} saw {seen} running");
    }
}

#[test]
fn profile_partition_map_keeps_parallelism_precedence() {
    let p = profile_count_project("[environments.local.settings]\nparallelism = 2\n");
    for (args, parallelism) in [
        (vec!["--json", "--env", "local"], 2),
        (vec!["--json", "--env", "local", "--parallelism", "3"], 3),
        (vec!["--json", "--env", "one"], 5),
        (vec!["--json"], 5),
    ] {
        let output = p.run(&args);
        success(&output);
        let events = events(&output);
        assert_eq!(events[0]["parallelism"], parallelism, "{args:?}");
    }
    let output = p.run(&["list", "--json", "--env", "local"]);
    success(&output);
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["settings"]["parallelism"], 2);
    assert_eq!(event["jobs"][0]["partitions"]["count"], 6);
}

#[test]
fn plain_text_labels_instances_with_the_profile_count() {
    let p = profile_count_project("");
    let output = p.run(&["--env", "local"]);
    success(&output);
    let out = stdout(&output);
    for index in 1..=6 {
        assert!(out.contains(&format!("test ({index}/6) PASSED")), "{out}");
    }
    assert!(!out.contains("/3)"), "{out}");
}
