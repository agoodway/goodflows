use std::io::Write;
use std::process::{Command, Stdio};
mod common;
use common::*;

#[test]
fn init_requires_confirmation_to_overwrite() {
    for (index, (answer, overwrite)) in [
        ("n\n", false),
        ("\n", false),
        ("", false),
        ("maybe\n", false),
        ("y\n", true),
        ("YES\n", true),
        ("y", true),
        ("yes", true),
        (" YeS \r\n", true),
    ]
    .iter()
    .enumerate()
    {
        let dir =
            std::env::temp_dir().join(format!("goodflows-init-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("goodflows.toml");
        std::fs::write(&config, "existing config\n").unwrap();
        std::fs::write(dir.join("answer"), answer).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_goodflows"))
            .arg("init")
            .current_dir(&dir)
            .stdin(Stdio::from(
                std::fs::File::open(dir.join("answer")).unwrap(),
            ))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let content = std::fs::read_to_string(&config).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("Overwrite?"),
            "nonterminal input should not prompt: {stdout}"
        );
        if *overwrite {
            assert!(output.status.success(), "{output:?}");
            assert!(content.contains("[[jobs]]"));
            assert!(stdout.contains("Wrote goodflows.toml"));
        } else {
            assert_eq!(output.status.code(), Some(2), "answer: {answer:?}");
            assert_eq!(content, "existing config\n", "answer: {answer:?}");
            assert!(!stdout.contains("Wrote goodflows.toml"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("pass --force"));
        }
    }
}

#[test]
fn init_creates_new_config_without_prompting() {
    let dir = std::env::temp_dir().join(format!("goodflows-init-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_goodflows"))
        .arg("init")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let content = std::fs::read_to_string(dir.join("goodflows.toml")).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(output.status.success());
    assert!(content.contains("[[jobs]]"));
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains("Overwrite?"));
}

#[test]
fn init_detects_projects_and_explicit_templates_override_detection() {
    for (marker, template, command) in [
        ("Cargo.toml", "rust", "cargo test"),
        ("mix.exs", "elixir", "mix test"),
        ("go.mod", "go", "go test"),
        ("", "generic", "Replace this command"),
    ] {
        let p = Project::new("");
        std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
        if !marker.is_empty() {
            std::fs::write(p.dir.join(marker), "").unwrap();
        }
        let output = p.run(&["init", "--json"]);
        success(&output);
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(event["template"], template);
        assert!(p.read("goodflows.toml").contains(command));
        success(&p.run(&["validate"]));
        success(&p.run(&["init", "--template", "elixir", "--force"]));
        assert!(p.read("goodflows.toml").contains("mix test"));
    }
}

#[test]
fn init_rejects_ambiguous_projects_without_writing_config() {
    let p = Project::new("keep");
    std::fs::write(p.dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(p.dir.join("mix.exs"), "").unwrap();
    let output = p.run(&["init", "--force"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--template"));
    assert_eq!(p.read("goodflows.toml"), "keep");
}

#[test]
fn init_honors_custom_path_force_and_generated_workdir() {
    let p = Project::new("keep root");
    let output = p.run(&[
        "init",
        "--template",
        "generic",
        "--config",
        "nested/config/checks.toml",
    ]);
    success(&output);
    assert_eq!(p.read("goodflows.toml"), "keep root");
    success(&p.run(&["validate", "--config", "nested/config/checks.toml"]));
    let mut config: toml::Value = toml::from_str(&p.read("nested/config/checks.toml")).unwrap();
    config["jobs"][0]["command"] = toml::Value::String("pwd > location".into());
    std::fs::write(
        p.dir.join("nested/config/checks.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    success(&p.run(&["--config", "nested/config/checks.toml"]));
    assert_eq!(
        p.read("location").trim(),
        p.dir.canonicalize().unwrap().to_str().unwrap()
    );
    success(&p.run(&[
        "init",
        "--template",
        "go",
        "--config",
        "nested/config/checks.toml",
        "--force",
        "--json",
    ]));
    assert!(p.read("nested/config/checks.toml").contains("go test"));
}

#[test]
fn json_init_without_terminal_refuses_existing_config_and_names_force() {
    let p = Project::new("keep");
    let output = p
        .command()
        .args(["init", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(p.read("goodflows.toml"), "keep");
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["type"], "error");
    assert_eq!(
        event["message"],
        "goodflows.toml already exists; pass --force to overwrite it"
    );
}

#[test]
fn node_init_uses_declared_scripts_and_lockfile_package_manager() {
    for (lock, manager) in [
        ("package-lock.json", "npm"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
    ] {
        let p = Project::new("");
        std::fs::write(
            p.dir.join("package.json"),
            r#"{"scripts":{"lint":"eslint .","test":"vitest run","start":"node index.js"}}"#,
        )
        .unwrap();
        std::fs::write(p.dir.join(lock), "").unwrap();
        success(&p.run(&["init", "--force"]));
        let config = p.read("goodflows.toml");
        assert!(config.contains(&format!("{manager} run lint")));
        assert!(config.contains(&format!("{manager} run test")));
        assert!(!config.contains("run build"));
        assert!(!config.contains("run start"));
        success(&p.run(&["validate"]));
    }
}

#[test]
fn node_init_rejects_malformed_manifest_or_missing_check_scripts() {
    for manifest in ["{", r#"{"scripts":{"start":"serve"}}"#] {
        let p = Project::new("keep");
        std::fs::write(p.dir.join("package.json"), manifest).unwrap();
        assert!(!p.run(&["init", "--force"]).status.success());
        assert_eq!(p.read("goodflows.toml"), "keep");
    }
}

#[test]
fn node_template_requires_a_manifest_and_rejects_conflicting_lockfiles() {
    let p = Project::new("keep");
    let output = p.run(&["init", "--template", "node", "--force"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("package.json"));
    assert_eq!(p.read("goodflows.toml"), "keep");
    std::fs::write(p.dir.join("package.json"), r#"{"scripts":{"test":"test"}}"#).unwrap();
    for lock in ["package-lock.json", "pnpm-lock.yaml"] {
        std::fs::write(p.dir.join(lock), "").unwrap();
    }
    let output = p.run(&["init", "--force"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("conflicting"));
    assert_eq!(p.read("goodflows.toml"), "keep");
}

#[cfg(unix)]
#[test]
fn init_through_symlink_preserves_project_workdir() {
    use std::os::unix::fs::symlink;
    let p = Project::new("");
    std::fs::create_dir(p.dir.join("elsewhere")).unwrap();
    std::fs::write(p.dir.join("elsewhere/checks.toml"), "keep").unwrap();
    std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
    symlink("elsewhere/checks.toml", p.dir.join("goodflows.toml")).unwrap();
    success(&p.run(&["init", "--template", "generic", "--force"]));
    let mut config: toml::Value = toml::from_str(&p.read("goodflows.toml")).unwrap();
    config["jobs"][0]["command"] = toml::Value::String("pwd > location".into());
    std::fs::write(
        p.dir.join("goodflows.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    success(&p.run(&[]));
    assert!(p.dir.join("location").exists());
    assert_eq!(
        p.read("location").trim(),
        p.dir.canonicalize().unwrap().to_str().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn nonterminal_init_never_waits_for_unavailable_or_incomplete_input() {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};
    let overlong = format!("yes{}\n", " ".repeat(5000));
    for (input, overwrite) in [
        (overlong.as_str(), false),
        ("", false),
        ("ye", false),
        ("yes", false),
        ("y\n", true),
        ("YES\n", true),
        ("n\n", false),
    ] {
        let p = Project::new("keep");
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(input.as_bytes()).unwrap();
        // SAFETY: reader owns this live descriptor. A clone shares its status flags.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        let mut child = p
            .command()
            .args(["init", "--json"])
            .stdin(Stdio::from(OwnedFd::from(reader.try_clone().unwrap())))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let mut done = false;
        while start.elapsed() < Duration::from_secs(1) {
            if child.try_wait().unwrap().is_some() {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !done {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(done, "blocked with available input {input:?}");
        assert_eq!(
            output.status.code(),
            Some(if overwrite { 0 } else { 2 }),
            "{input:?}"
        );
        assert!(output.stderr.is_empty());
        assert_eq!(
            unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
            flags
        );
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            event["type"],
            if overwrite { "config_written" } else { "error" }
        );
        assert_eq!(p.read("goodflows.toml") != "keep", overwrite);
    }
}

#[cfg(unix)]
#[test]
fn terminal_init_still_prompts_and_a_declined_prompt_succeeds() {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    for (answer, overwrite) in [("yes\n", true), ("n\n", false)] {
        let p = Project::new("keep");
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: openpty initializes both descriptor outputs; optional arguments are null.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        let child = p
            .command()
            .args(["init"])
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        master.write_all(answer.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        success(&output);
        assert!(stdout(&output).contains("Overwrite? [y/N]"));
        assert_eq!(p.read("goodflows.toml") != "keep", overwrite);
        if !overwrite {
            assert!(stdout(&output).contains("Kept existing goodflows.toml"));
        }
    }
}

#[test]
fn generic_placeholder_fails_until_replaced() {
    let p = Project::new("");
    success(&p.run(&["init", "--force", "--template", "generic"]));
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let events: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["type"] == "job_output"
        && e["stream"] == "stderr"
        && e["data"].as_str().unwrap().contains("Replace this command")));
    assert_eq!(events.last().unwrap()["success"], false);
}

#[test]
fn nonterminal_refusal_does_not_require_valid_project_detection() {
    let p = Project::new("keep");
    std::fs::write(p.dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(p.dir.join("package.json"), "invalid manifest").unwrap();
    let output = p
        .command()
        .args(["init", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(p.read("goodflows.toml"), "keep");
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(event["message"].as_str().unwrap().contains("--force"));
}

#[test]
fn generated_toml_omits_defaults_and_explains_parallelism() {
    let p = Project::new("");
    for template in ["rust", "elixir"] {
        success(&p.run(&["init", "--force", "--template", template]));
        let text = p.read("goodflows.toml");
        let config: toml::Value = toml::from_str(&text).unwrap();
        assert!(!text.contains("depends_on = []"));
        assert!(!text.contains("[jobs.env]\n\n"));
        for job in config["jobs"].as_array().unwrap() {
            assert!(job.get("shell").is_none());
            assert!(job.get("timeout_secs").is_none());
            if let Some(env) = job.get("env") {
                assert!(!env.as_table().unwrap().is_empty());
            }
        }
        assert!(config["jobs"][1]["depends_on"]
            .as_array()
            .is_some_and(|v| !v.is_empty()));
        if template == "elixir" {
            assert_eq!(
                config["settings"]["env"],
                toml::Value::Table(toml::toml! { MIX_ENV = "test" })
            );
        } else {
            assert!(config["settings"].get("env").is_none(), "{text}");
            assert!(!text.contains("[settings.env]"), "{text}");
        }
        assert!(text.contains("every logical CPU"));
        assert!(text.contains("machine size"));
        success(&p.run(&["validate"]));
    }
}

/// A stub `mix` that records the MIX_ENV each invocation sees.
fn stub_mix(p: &Project) -> String {
    let bin = p.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mix = bin.join("mix");
    std::fs::write(
        &mix,
        "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"${MIX_ENV-unset}\" >> mix-calls\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mix, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn elixir_init_declares_mix_env_once_and_every_job_inherits_it() {
    for workflow in [None, Some("ci")] {
        let p = Project::new("");
        let mut args = vec!["init", "--template", "elixir", "--force"];
        if let Some(name) = workflow {
            args.extend(["--workflow", name]);
        }
        success(&p.run(&args));
        let text = p.read("goodflows.toml");
        assert_eq!(text.matches("MIX_ENV").count(), 1, "{text}");
        // The header warns that shared settings env reaches later-added workflows.
        assert!(
            text.contains("# [settings.env] applies to every workflow; override per workflow with [workflows.NAME.settings.env]\n"),
            "{text}"
        );
        let config: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(config["settings"]["env"]["MIX_ENV"].as_str(), Some("test"));
        let jobs = match workflow {
            Some(name) => config["workflows"][name]["jobs"].clone(),
            None => config["jobs"].clone(),
        };
        for job in jobs.as_array().unwrap() {
            assert!(job.get("env").is_none(), "{text}");
        }
        success(&p.run(&["validate"]));
        let path = stub_mix(&p);
        let mut run = vec!["run"];
        run.extend(workflow);
        let output = p
            .command()
            .env("PATH", path)
            .env("MIX_ENV", "dev")
            .args(&run)
            .output()
            .unwrap();
        success(&output);
        let mut calls: Vec<_> = p.read("mix-calls").lines().map(str::to_owned).collect();
        calls.sort();
        assert_eq!(calls, ["compile test", "format test", "test test"]);
    }
}

#[test]
fn templates_without_env_defaults_omit_settings_env() {
    let p = Project::new("");
    std::fs::write(
        p.dir.join("package.json"),
        r#"{"scripts":{"test":"node --test"}}"#,
    )
    .unwrap();
    for template in ["rust", "go", "node", "generic"] {
        for workflow in [None, Some("ci")] {
            let mut args = vec!["init", "--template", template, "--force"];
            if let Some(name) = workflow {
                args.extend(["--workflow", name]);
            }
            success(&p.run(&args));
            let text = p.read("goodflows.toml");
            let config: toml::Value = toml::from_str(&text).unwrap();
            assert!(
                config["settings"].get("env").is_none(),
                "{template}: {text}"
            );
            assert!(!text.contains("[settings.env]"), "{template}: {text}");
        }
    }
}
