mod common;
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn events(output: &std::process::Output) -> Vec<Value> {
    stdout(output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn only(output: &std::process::Output, kind: &str) -> Value {
    let events: Vec<_> = events(output)
        .into_iter()
        .filter(|event| event["type"] == kind)
        .collect();
    assert_eq!(events.len(), 1, "{}", stdout(output));
    events[0].clone()
}

/// `prep` passes; `slow` creates `started`, waits for the file `release`
/// (at most 30 seconds, so a failed test leaves no process behind), then
/// fails; `after` is skipped because `slow` failed.
const CI: &str = "[settings]\nfail_fast = false\n\
[[jobs]]\nname = 'prep'\ncommand = 'echo prepared'\n\
[[jobs]]\nname = 'slow'\ndepends_on = ['prep']\n\
command = 'touch started; for i in $(seq 600); do test -f release && break; sleep 0.05; done; echo broken >&2; exit 1'\n\
[[jobs]]\nname = 'after'\ncommand = 'true'\ndepends_on = ['slow']\n";

/// A run's summary without what differs between two runs of the same jobs:
/// identity, start time, host, timings and history paths.
fn stable(summary: &Value) -> Value {
    let mut summary = summary.clone();
    for key in [
        "run_id",
        "started_at",
        "duration_ms",
        "execute_ms",
        "max_in_flight",
        "host",
        "history_file",
    ] {
        summary.as_object_mut().unwrap().remove(key);
    }
    for result in summary["results"].as_array_mut().unwrap() {
        for key in ["duration_ms", "blocked_ms", "queued_ms", "commands"] {
            result.as_object_mut().unwrap().remove(key);
        }
        for step in result["steps"].as_array_mut().into_iter().flatten() {
            step.as_object_mut().unwrap().remove("duration_ms");
        }
    }
    summary
}

/// Text output with each duration (`12ms`, `3.0s`) replaced and spacing
/// collapsed, so two runs of the same jobs compare equal.
fn stable_text(text: &str) -> Vec<String> {
    let duration = |word: &str| {
        let number = word
            .strip_suffix("ms")
            .or_else(|| word.strip_suffix('s'))
            .unwrap_or("");
        !number.is_empty() && number.chars().all(|c| c.is_ascii_digit() || c == '.')
    };
    text.lines()
        .map(|line| {
            line.split_whitespace()
                .map(|word| if duration(word) { "<t>" } else { word })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn wait_for(p: &Project, file: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !p.dir.join(file).exists() {
        assert!(Instant::now() < deadline, "{file} never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Start a detached run and return its id.
fn detach(p: &Project, args: &[&str]) -> String {
    let started = Instant::now();
    let output = p.run(&[&["run", "--detach", "--json"], args].concat());
    success(&output);
    // The caller's pipes close at once: the run does not inherit them.
    assert!(started.elapsed() < Duration::from_secs(10));
    let event = only(&output, "run_detached");
    event["run_id"].as_str().unwrap().to_owned()
}

#[test]
fn polling_wait_gives_the_foreground_summary_and_exit_code() {
    let p = Project::new(CI);
    // The foreground run, with `slow` released at once.
    std::fs::write(p.dir.join("release"), "").unwrap();
    let foreground = p.run(&["run", "--json"]);
    assert_eq!(foreground.status.code(), Some(1), "{}", stderr(&foreground));
    let foreground_summary = only(&foreground, "summary");
    let foreground_text = p.run(&["run", "--output", "failures"]);
    assert_eq!(foreground_text.status.code(), Some(1));
    for file in ["release", "started"] {
        std::fs::remove_file(p.dir.join(file)).unwrap();
    }

    let id = detach(&p, &[]);
    wait_for(&p, "started");

    // While `slow` waits, status shows it running after `prep` passed.
    let status = p.run(&["status", &id, "--json"]);
    success(&status);
    let status = only(&status, "run_status");
    assert_eq!(status["run_id"], id.as_str());
    assert_eq!(status["state"], "running");
    assert_eq!(status["jobs"], 3);
    assert_eq!(status["running"], json!(["slow"]));
    assert_eq!(status["passed"], json!(["prep"]));
    assert_eq!(status["failed"], json!([]));
    let text = p.run(&["status", &id]);
    success(&text);
    assert!(stdout(&text).contains(": running"), "{}", stdout(&text));
    assert!(stdout(&text).contains("Running: slow"), "{}", stdout(&text));

    // A wait that times out exits 75 with the run's progress and no summary.
    let started = Instant::now();
    let timed_out = p.run(&["wait", &id, "--timeout", "1", "--json"]);
    assert_eq!(timed_out.status.code(), Some(75), "{}", stderr(&timed_out));
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(only(&timed_out, "run_status")["state"], "running");
    let timed_out = p.run(&["wait", &id, "--timeout", "0"]);
    assert_eq!(timed_out.status.code(), Some(75));
    assert!(
        stdout(&timed_out).contains("Still running"),
        "{}",
        stdout(&timed_out)
    );

    // Short polls until the run ends, as an agent would.
    std::fs::write(p.dir.join("release"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let done = loop {
        let output = p.run(&["wait", &id, "--timeout", "1", "--json"]);
        if output.status.code() != Some(75) {
            break output;
        }
        assert!(Instant::now() < deadline, "the run never finished");
    };
    assert_eq!(done.status.code(), foreground.status.code());
    let summary = only(&done, "summary");
    assert_eq!(summary["run_id"], id.as_str());
    assert_eq!(summary["outcome"], "failed");
    assert_eq!(stable(&summary), stable(&foreground_summary));

    // Waiting again, in text mode, prints the foreground run's report.
    let report = p.run(&["wait", &id, "--timeout", "1"]);
    assert_eq!(report.status.code(), Some(1));
    let foreground_text = stdout(&foreground_text);
    let foreground_report = &foreground_text[foreground_text.find("\ndefault  FAILED").unwrap()..];
    assert_eq!(
        stable_text(&stdout(&report)),
        stable_text(foreground_report)
    );

    let status = p.run(&["status", &id, "--json"]);
    success(&status);
    let status = only(&status, "run_status");
    assert_eq!(status["state"], "finished");
    assert_eq!(status["exit_code"], 1);
    assert_eq!(status["failed"], json!(["slow"]));
    assert_eq!(status["skipped"], json!(["after"]));
}

#[test]
fn a_passing_detached_run_exits_0_and_is_the_last_run() {
    let p = Project::new("[[jobs]]\nname = 'a'\ncommand = 'true'\n");
    let id = detach(&p, &[]);
    let done = p.run(&["wait", &id, "--json"]);
    success(&done);
    let summary = only(&done, "summary");
    assert_eq!(summary["run_id"], id.as_str());
    assert_eq!(summary["success"], true);
    let last = p.run(&["last", "--json"]);
    success(&last);
    assert_eq!(only(&last, "summary")["run_id"], id.as_str());

    // A summary-only caller still gets the run id, and status still sees
    // every job.
    let output = p.run(&["run", "--detach", "--json=summary"]);
    success(&output);
    let id = only(&output, "run_detached")["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = p.run(&["wait", &id, "--json=summary"]);
    success(&done);
    assert_eq!(only(&done, "summary")["run_id"], id.as_str());
    let status = only(&p.run(&["status", &id, "--json"]), "run_status");
    assert_eq!(status["passed"], json!(["a"]));

    // Plain text prints only the run id on stdout.
    let output = p.run(&["run", "--detach"]);
    success(&output);
    let id = stdout(&output).trim().to_owned();
    assert!(p.dir.join(".goodflows/runs").join(&id).is_dir(), "{id}");
    assert!(
        stderr(&output).contains(&format!("`goodflows cancel {id}`")),
        "{}",
        stderr(&output)
    );
    success(&p.run(&["wait", &id]));
}

#[test]
fn detach_reports_usage_and_config_errors_before_starting() {
    let p = Project::new("[[jobs]]\nname = 'a'\ncommand = 'true'\n");
    for args in [
        &["run", "--detach", "--jobs", "missing"][..],
        &["run", "missing", "--detach"],
        &["run", "--detach", "--sprite", "T-1"],
        &["run", "--detached-run", "x"],
    ] {
        let output = p.run(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert!(!p.dir.join(".goodflows/runs").exists());

    let unknown = "00000000-0000-4000-8000-000000000000";
    for command in ["status", "wait", "cancel"] {
        let output = p.run(&[command, unknown, "--json"]);
        assert_eq!(output.status.code(), Some(2), "{command}");
        assert_eq!(only(&output, "error")["code"], "unknown_run", "{command}");
        let output = p.run(&[command, "not-a-run"]);
        assert_eq!(output.status.code(), Some(2), "{command}");
        let output = p.run(&[command, unknown, "--jobs", "a"]);
        assert_eq!(output.status.code(), Some(2), "{command}");
        assert!(stderr(&output).contains("--jobs cannot be used with"));
    }
}

#[test]
fn status_keeps_each_call_of_one_workflow_apart() {
    // Both calls run `suite`. The first `work` to take `claimed` fails at
    // once; the other waits for `release`, then passes.
    let p = Project::new(
        "[settings]\nfail_fast = false\n\
         [[workflows.ci.jobs]]\nname = 'call-a'\nworkflow = 'suite'\n\
         [[workflows.ci.jobs]]\nname = 'call-b'\nworkflow = 'suite'\n\
         [[workflows.suite.jobs]]\nname = 'work'\n\
         command = 'mkdir claimed 2>/dev/null && exit 1; touch started; for i in $(seq 600); do test -f release && exit 0; sleep 0.05; done; exit 1'\n",
    );
    let id = detach(&p, &["ci"]);
    wait_for(&p, "started");
    // The failed call's results may land just after `started`.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = only(&p.run(&["status", &id, "--json"]), "run_status");
        if status["failed"].as_array().unwrap().len() == 2 {
            break status;
        }
        assert!(Instant::now() < deadline, "{status}");
        std::thread::sleep(Duration::from_millis(20));
    };
    std::fs::write(p.dir.join("release"), "").unwrap();
    // One call and its `work` failed; the other call and its `work` run.
    let child = |list: &Value| -> String {
        let names: Vec<_> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{status}");
        assert!(names[0] == "call-a" || names[0] == "call-b", "{status}");
        assert!(
            names[1].starts_with("suite [") && names[1].ends_with("] work"),
            "{status}"
        );
        names[1].to_owned()
    };
    assert_ne!(child(&status["running"]), child(&status["failed"]));

    let done = p.run(&["wait", &id, "--timeout", "30"]);
    assert_eq!(done.status.code(), Some(1), "{}", stderr(&done));
    let status = only(&p.run(&["status", &id, "--json"]), "run_status");
    assert_eq!(status["passed"].as_array().unwrap().len(), 2, "{status}");
    assert_eq!(status["failed"].as_array().unwrap().len(), 2, "{status}");
}

#[test]
fn a_detached_run_that_dies_is_lost_not_running() {
    let p = Project::new(CI);
    let id = detach(&p, &[]);
    wait_for(&p, "started");
    let pid = p.read(&format!(".goodflows/runs/{id}/pid"));
    let kill = std::process::Command::new("kill")
        .args(["-KILL", pid.trim()])
        .status()
        .unwrap();
    assert!(kill.success());
    // Release the orphaned job so it ends too.
    std::fs::write(p.dir.join("release"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = only(&p.run(&["status", &id, "--json"]), "run_status");
        if status["state"] != "running" {
            break status;
        }
        assert!(Instant::now() < deadline, "still running after SIGKILL");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status["state"], "lost");
    let wait = p.run(&["wait", &id, "--json"]);
    assert_eq!(wait.status.code(), Some(1));
    assert_eq!(only(&wait, "error")["code"], "runtime");
}

#[test]
fn cancel_stops_a_running_detached_run() {
    let p = Project::new(CI);
    let id = detach(&p, &[]);
    wait_for(&p, "started");
    let cancel = p.run(&["cancel", &id, "--json"]);
    success(&cancel);
    let event = only(&cancel, "run_status");
    assert_eq!(event["was_running"], true);
    assert_eq!(event["state"], "finished");
    assert_eq!(event["exit_code"], 143);
    assert_eq!(event["running"], json!([]));
    // `slow` was running and `after` waiting for it.
    assert_eq!(event["cancelled"], json!(["slow", "after"]));
    assert_eq!(event["passed"], json!(["prep"]));
    let status = only(&p.run(&["status", &id, "--json"]), "run_status");
    assert_eq!(status["state"], "finished");
    assert_eq!(status["cancelled"], json!(["slow", "after"]));
    assert_eq!(p.run(&["wait", &id]).status.code(), Some(143));

    // Cancelling it again says it already stopped.
    let again = p.run(&["cancel", &id]);
    success(&again);
    let text = stdout(&again);
    assert!(
        text.starts_with(&format!(
            "Run {id} had already stopped; nothing to cancel.\nRun {id}: finished (exit 143)\n"
        )),
        "{text}"
    );
}

#[test]
fn starting_a_detached_run_keeps_the_newest_ten_finished_runs_of_its_config() {
    let p = Project::new(CI);
    std::fs::write(
        p.dir.join("other.toml"),
        "[[jobs]]\nname = 'a'\ncommand = 'true'\n",
    )
    .unwrap();
    let runs = p.dir.join(".goodflows/runs");
    let names = || {
        let mut names: Vec<_> = std::fs::read_dir(&runs)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    // A run of another config beside this one is not this config's to prune.
    let other = detach(&p, &["--config", "other.toml"]);
    success(&p.run(&["wait", &other, "--config", "other.toml"]));
    // The first run is still going while eleven more start and finish.
    let going = detach(&p, &[]);
    wait_for(&p, "started");
    std::fs::write(
        p.dir.join("goodflows.toml"),
        "[[jobs]]\nname = 'a'\ncommand = 'true'\n",
    )
    .unwrap();
    let mut ids = vec![going.clone()];
    for _ in 0..11 {
        let id = detach(&p, &[]);
        success(&p.run(&["wait", &id]));
        ids.push(id);
    }
    // Twelve runs: the running one stays, the oldest finished one goes.
    let mut kept: Vec<_> = [&[other.clone(), going.clone()][..], &ids[2..]].concat();
    kept.sort();
    assert_eq!(names(), kept);

    // Once it has finished, the next run prunes it too: ten remain.
    std::fs::write(p.dir.join("release"), "").unwrap();
    assert_eq!(p.run(&["wait", &going]).status.code(), Some(1));
    let id = detach(&p, &[]);
    success(&p.run(&["wait", &id]));
    ids.push(id);
    let mut kept: Vec<_> = [&[other][..], &ids[3..]].concat();
    kept.sort();
    assert_eq!(names(), kept);
    assert_eq!(kept.len(), 11);
}
