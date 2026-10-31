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
fn interrupt_and_termination_kill_descendants_and_emit_summary() {
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let p = Project::new("[settings]\nparallelism=1\n[[jobs]]\nname='running'\ncommand='touch started; (sleep 0.5; touch escaped) & wait'\n[[jobs]]\nname='queued'\ncommand='touch queued'");
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
        std::thread::sleep(Duration::from_millis(600));
        assert!(!p.dir.join("escaped").exists());
        assert!(!p.dir.join("queued").exists());
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
