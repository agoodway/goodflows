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
