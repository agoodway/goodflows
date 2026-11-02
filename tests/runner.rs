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
    let output = p.run(&[]);
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
    let mut names: Vec<_> = std::fs::read_dir(p.dir.join("logs"))
        .unwrap()
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
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
    for index in 1..=3 {
        assert_eq!(
            p.read(&format!("logs/002-test.p{index:02}.stdout.log")),
            format!("out {index}")
        );
        assert_eq!(
            p.read(&format!("logs/002-test.p{index:02}.stderr.log")),
            format!("err {index}")
        );
    }
}
