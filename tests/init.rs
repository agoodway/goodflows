use std::io::Write;
use std::process::{Command, Stdio};
mod common;
use common::*;

#[test]
fn init_requires_confirmation_to_overwrite() {
    for (index, answer) in ["n\n", "\n", "", "maybe\n", "y\n", "YES\n"]
        .iter()
        .enumerate()
    {
        let dir = std::env::temp_dir().join(format!("goodci-init-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("goodci.toml");
        std::fs::write(&config, "existing config\n").unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_goodci"))
            .arg("init")
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(answer.as_bytes());
        let output = child.wait_with_output().unwrap();
        let content = std::fs::read_to_string(&config).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Overwrite? [y/N]"), "{stdout}");
        if matches!(*answer, "y\n" | "YES\n") {
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
