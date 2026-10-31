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
        let dir = std::env::temp_dir().join(format!("goodci-init-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("goodci.toml");
        std::fs::write(&config, "existing config\n").unwrap();
        std::fs::write(dir.join("answer"), answer).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_goodci"))
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

        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            !stdout.contains("Overwrite?"),
            "nonterminal input should not prompt: {stdout}"
        );
        if *overwrite {
            assert!(content.contains("[[jobs]]"));
            assert!(stdout.contains("Wrote goodci.toml"));
        } else {
            assert_eq!(content, "existing config\n", "answer: {answer:?}");
            assert!(!stdout.contains("Wrote goodci.toml"));
        }
    }
}

#[test]
fn init_creates_new_config_without_prompting() {
    let dir = std::env::temp_dir().join(format!("goodci-init-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_goodci"))
        .arg("init")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let content = std::fs::read_to_string(dir.join("goodci.toml")).unwrap();
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
        std::fs::remove_file(p.dir.join("goodci.toml")).unwrap();
        if !marker.is_empty() {
            std::fs::write(p.dir.join(marker), "").unwrap();
        }
        let output = p.run(&["init", "--json"]);
        success(&output);
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(event["template"], template);
        assert!(p.read("goodci.toml").contains(command));
        success(&p.run(&["validate"]));
        success(&p.run(&["init", "--template", "elixir", "--force"]));
        assert!(p.read("goodci.toml").contains("mix test"));
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
    assert_eq!(p.read("goodci.toml"), "keep");
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
    assert_eq!(p.read("goodci.toml"), "keep root");
    success(&p.run(&["validate", "--config", "nested/config/checks.toml"]));
    let mut config: toml::Value = toml::from_str(&p.read("nested/config/checks.toml")).unwrap();
    config["jobs"][0]["command"] = toml::Value::String("pwd > location".into());
    std::fs::write(
        p.dir.join("nested/config/checks.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    success(&p.run(&["--config", "nested/config/checks.toml"]));
    assert_eq!(p.read("location").trim(), p.dir.to_str().unwrap());
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
fn json_init_declines_existing_config_without_polluting_stdout() {
    let p = Project::new("keep");
    let output = p
        .command()
        .args(["init", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("goodci.toml"), "keep");
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["type"], "init_cancelled");
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
        let config = p.read("goodci.toml");
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
        assert_eq!(p.read("goodci.toml"), "keep");
    }
}

#[test]
fn node_template_requires_a_manifest_and_rejects_conflicting_lockfiles() {
    let p = Project::new("keep");
    let output = p.run(&["init", "--template", "node", "--force"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("package.json"));
    assert_eq!(p.read("goodci.toml"), "keep");
    std::fs::write(p.dir.join("package.json"), r#"{"scripts":{"test":"test"}}"#).unwrap();
    for lock in ["package-lock.json", "pnpm-lock.yaml"] {
        std::fs::write(p.dir.join(lock), "").unwrap();
    }
    let output = p.run(&["init", "--force"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("conflicting"));
    assert_eq!(p.read("goodci.toml"), "keep");
}

#[cfg(unix)]
#[test]
fn init_through_symlink_preserves_project_workdir() {
    use std::os::unix::fs::symlink;
    let p = Project::new("");
    std::fs::create_dir(p.dir.join("elsewhere")).unwrap();
    std::fs::write(p.dir.join("elsewhere/checks.toml"), "keep").unwrap();
    std::fs::remove_file(p.dir.join("goodci.toml")).unwrap();
    symlink("elsewhere/checks.toml", p.dir.join("goodci.toml")).unwrap();
    success(&p.run(&["init", "--template", "generic", "--force"]));
    let mut config: toml::Value = toml::from_str(&p.read("goodci.toml")).unwrap();
    config["jobs"][0]["command"] = toml::Value::String("pwd > location".into());
    std::fs::write(p.dir.join("goodci.toml"), toml::to_string(&config).unwrap()).unwrap();
    success(&p.run(&[]));
    assert!(p.dir.join("location").exists());
    assert_eq!(p.read("location").trim(), p.dir.to_str().unwrap());
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
        success(&output);
        assert!(output.stderr.is_empty());
        assert_eq!(
            unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
            flags
        );
        let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            event["type"],
            if overwrite {
                "config_written"
            } else {
                "init_cancelled"
            }
        );
        assert_eq!(p.read("goodci.toml") != "keep", overwrite);
    }
}

#[cfg(unix)]
#[test]
fn terminal_init_still_prompts_and_accepts_confirmation() {
    use std::fs::File;
    use std::os::fd::FromRawFd;
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
                std::ptr::null(),
                std::ptr::null(),
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
    master.write_all(b"yes\n").unwrap();
    let output = child.wait_with_output().unwrap();
    success(&output);
    assert!(stdout(&output).contains("Overwrite? [y/N]"));
    assert_ne!(p.read("goodci.toml"), "keep");
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
fn nonterminal_decline_does_not_require_valid_project_detection() {
    let p = Project::new("keep");
    std::fs::write(p.dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(p.dir.join("package.json"), "invalid manifest").unwrap();
    let output = p
        .command()
        .args(["init", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    success(&output);
    assert_eq!(p.read("goodci.toml"), "keep");
    let event: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(event["type"], "init_cancelled");
}

#[test]
fn generated_toml_omits_defaults_and_explains_parallelism() {
    let p = Project::new("");
    for template in ["rust", "elixir"] {
        success(&p.run(&["init", "--force", "--template", template]));
        let text = p.read("goodci.toml");
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
            assert_eq!(config["jobs"][0]["env"]["MIX_ENV"].as_str(), Some("test"));
        }
        assert!(text.contains("every logical CPU"));
        assert!(text.contains("machine size"));
        success(&p.run(&["validate"]));
    }
}
