use std::io::Write;
use std::process::{Command, Stdio};

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
