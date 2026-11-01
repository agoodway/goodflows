#![cfg(unix)]
mod common;
use common::*;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[test]
fn timeout_kills_descendants_and_reports_timeout() {
    let p = Project::new(
        "[[jobs]]\nname='timeout'\ncommand='(sleep 1.3; touch escaped) & wait'\ntimeout_secs=1",
    );
    let output = p.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let summary: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
    assert_eq!(summary["results"][0]["status"], "timed_out");
    std::thread::sleep(Duration::from_millis(450));
    assert!(!p.dir.join("escaped").exists());
}

#[test]
fn condition_timeout_cleans_up_and_command_shares_the_timeout_budget() {
    for (condition, command, expected_tail) in [
        (
            "printf condition; (sleep 1.3; touch escaped) & wait",
            "touch ran",
            "condition",
        ),
        (
            "printf condition; sleep 0.6",
            "printf command; touch started; sleep 0.7; touch ran",
            "conditioncommand",
        ),
    ] {
        let p = Project::new(&format!(
            "[[jobs]]\nname='timeout'\ncondition='{condition}'\ncommand='{command}'\ntimeout_secs=1"
        ));
        let output = p.run(&["--json", "--log-dir", "logs"]);
        assert_eq!(output.status.code(), Some(1));
        let summary: serde_json::Value =
            serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
        assert_eq!(summary["results"][0]["status"], "timed_out");
        assert_eq!(summary["results"][0]["stdout_tail"], expected_tail);
        assert_eq!(p.read("logs/001-timeout.stdout.log"), expected_tail);
        assert!(!p.dir.join("ran").exists());
        assert_eq!(
            p.dir.join("started").exists(),
            expected_tail == "conditioncommand"
        );
        std::thread::sleep(Duration::from_millis(450));
        assert!(!p.dir.join("escaped").exists());
    }
}

#[test]
fn steps_and_their_conditions_share_one_timeout_and_cleanup_descendants() {
    let p = Project::new(
        r#"
[[jobs]]
name = "timeout"
timeout_secs = 1
[[jobs.steps]]
id = "first"
command = "sleep 0.6; printf first"
[[jobs.steps]]
id = "second"
condition = { shell = "sleep 0.1; printf condition" }
command = "printf second; (sleep 0.7; touch escaped) & wait"
[[jobs.steps]]
id = "later"
command = "touch later"
"#,
    );
    let output = p.run(&["--json", "--log-dir", "logs"]);
    assert_eq!(output.status.code(), Some(1));
    let summary: serde_json::Value =
        serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
    let job = &summary["results"][0];
    assert_eq!(job["status"], "timed_out");
    assert_eq!(job["steps"][0]["status"], "passed");
    assert_eq!(job["steps"][1]["status"], "timed_out");
    assert_eq!(job["steps"][2]["status"], "skipped");
    // Process startup also consumes the shared budget. The second step may
    // time out before its condition or command gets to print.
    let tail = job["stdout_tail"].as_str().unwrap();
    assert!(matches!(
        tail,
        "first" | "firstcondition" | "firstconditionsecond"
    ));
    assert_eq!(p.read("logs/001-timeout.stdout.log"), tail);
    std::thread::sleep(Duration::from_millis(600));
    assert!(!p.dir.join("escaped").exists());
    assert!(!p.dir.join("later").exists());
}

#[test]
fn interrupt_and_termination_kill_descendants_and_emit_summary() {
    for (signal, code, field) in [
        (libc::SIGINT, 130, "command"),
        (libc::SIGTERM, 143, "command"),
        (libc::SIGINT, 130, "condition"),
        (libc::SIGTERM, 143, "condition"),
        (libc::SIGINT, 130, "step-command"),
        (libc::SIGTERM, 143, "step-condition"),
    ] {
        let is_step = field.starts_with("step-");
        let field = field.strip_prefix("step-").unwrap_or(field);
        let command = if field == "condition" {
            "command='touch ran'\n"
        } else {
            ""
        };
        let step = if is_step {
            "[[jobs.steps]]\nid='running'\n"
        } else {
            ""
        };
        let later = if is_step {
            "[[jobs.steps]]\nid='later'\ncommand='touch later'\n"
        } else {
            ""
        };
        // Signal only after the descendant starts, and let it escape only after
        // the CLI exits. A fixed sleep can finish before cancellation on busy CI.
        let p = Project::new(&format!("[settings]\nparallelism=1\n[[jobs]]\nname='running'\n{step}{command}{field}='(touch started; while ! test -f release; do sleep 0.01; done; touch escaped) >/dev/null 2>&1 & wait'\n{later}[[jobs]]\nname='queued'\ncommand='touch queued'"));
        let mut child = p
            .command()
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !p.dir.join("started").exists() {
            if start.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                panic!("job did not start");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: the child PID identifies the CLI spawned above.
        assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        let summary: serde_json::Value =
            serde_json::from_str(stdout(&output).lines().last().unwrap()).unwrap();
        assert_eq!(summary["cancelled"], 2);
        assert!(summary["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "cancelled"));
        std::fs::write(p.dir.join("release"), "").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !p.dir.join("escaped").exists(),
            "descendant escaped cancellation: signal={signal}, field={field}, step={is_step}"
        );
        assert!(!p.dir.join("queued").exists());
        assert!(!p.dir.join("ran").exists());
        assert!(!p.dir.join("later").exists());
        if is_step {
            assert_eq!(summary["results"][0]["steps"][0]["status"], "cancelled");
            assert_eq!(summary["results"][0]["steps"][1]["status"], "cancelled");
        }
    }
}

#[test]
fn parent_exit_does_not_leave_descendants_holding_output_pipes() {
    let p = Project::new(
        "[[jobs]]\nname='background'\ncommand='(sleep 0.4; touch escaped) & echo done'",
    );
    success(&p.run(&[]));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!p.dir.join("escaped").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn timeout_and_signal_cleanup_work_when_stdout_is_not_consumed() {
    let p =
        Project::new("[[jobs]]\nname='blocked'\ncommand='echo $$ > pid; yes out'\ntimeout_secs=1");
    let mut child = p
        .command()
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !p.dir.join("pid").exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid: i32 = p.read("pid").trim().parse().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let job_stopped = !std::path::Path::new(&format!("/proc/{pid}")).exists();
    // SAFETY: signal only the child CLI spawned by this test.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let stop = Instant::now();
    let mut exited = false;
    while stop.elapsed() < Duration::from_secs(2) {
        if child.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Drain for cleanup even when the regression occurs; do not leave the CLI stuck.
    if !exited {
        let _ = child.kill();
    }
    let _ = child.wait_with_output();
    // SAFETY: cleanup our own test job if the old implementation left it alive.
    if !job_stopped {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    assert!(
        job_stopped,
        "timeout did not reap the job while stdout was blocked"
    );
    assert!(exited, "SIGTERM did not stop CLI while stdout was blocked");
}
